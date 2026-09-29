//! OpenLustre Studio: CoCoSpec contract IR.
//!
//! A contract is a separate first-class artifact from the node it describes,
//! exactly as in Kind 2's CoCoSpec model: it carries assumptions, guarantees,
//! ghost variables, modes, and imports, and refers to ports by name. The
//! contract checker verifies well-formedness and connection to a node; the
//! emitter renders it back into Kind 2-compatible Lustre syntax.

use serde::{Deserialize, Serialize};

use ol_ir::{Expr, Port, Type};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assumption {
    pub name: Option<String>,
    pub expr: Expr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Guarantee {
    pub name: Option<String>,
    pub expr: Expr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GhostVar {
    pub name: String,
    pub ty: Type,
    pub definition: Expr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mode {
    pub name: String,
    #[serde(default)]
    pub requires: Vec<Expr>,
    #[serde(default)]
    pub ensures: Vec<Expr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractImport {
    pub contract: String,
    pub input_map: Vec<(String, Expr)>,
    pub output_map: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractDef {
    pub name: String,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
    #[serde(default)]
    pub ghost_vars: Vec<GhostVar>,
    #[serde(default)]
    pub assumptions: Vec<Assumption>,
    #[serde(default)]
    pub guarantees: Vec<Guarantee>,
    #[serde(default)]
    pub modes: Vec<Mode>,
    #[serde(default)]
    pub imports: Vec<ContractImport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractRef {
    pub contract: String,
}

/// Resolve the contracts stored as raw JSON on `Package::contracts` into
/// strongly typed `ContractDef`s. Returns `(contracts, errors)` so callers
/// can surface partial-parse failures alongside the successfully parsed
/// contracts.
pub fn parse_contracts(raw: &[serde_json::Value]) -> (Vec<ContractDef>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for (i, v) in raw.iter().enumerate() {
        match serde_json::from_value::<ContractDef>(v.clone()) {
            Ok(c) => out.push(c),
            Err(e) => errors.push(format!("contract #{i}: {e}")),
        }
    }
    (out, errors)
}

impl ContractDef {
    pub fn find_mode(&self, name: &str) -> Option<&Mode> {
        self.modes.iter().find(|m| m.name == name)
    }

    /// The contract as an **observer**: an ordinary synchronous operator that
    /// reads the contract's inputs and outputs and computes one `bool` per
    /// clause, with the ghost variables as its locals. It is what the
    /// runtime monitors execute — the simulator steps it and the C back end
    /// compiles it like any other node — so `pre` / `->`, ghosts and calls
    /// in contracts get exactly the semantics the model itself has, and the
    /// simulated and generated monitors agree by construction. It is also
    /// what the contract checker type-checks (C0080).
    ///
    /// Equations: the ghost definitions first, then one per signal, in
    /// `signals` order (`node.outputs[i]` is `signals[i]`).
    pub fn observer(&self) -> Observer {
        use ol_ir::{Equation, Local, NodeDef, NodeKind};
        let mut signals = Vec::new();
        let mut outputs = Vec::new();
        let mut equations: Vec<Equation> = self
            .ghost_vars
            .iter()
            .map(|g| Equation { lhs: vec![g.name.clone()], rhs: g.definition.clone() })
            .collect();
        let mut signal = |sig: ObserverSignal, port: String, expr: &Expr| {
            outputs.push(Port { name: port.clone(), ty: Type::Bool });
            equations.push(Equation { lhs: vec![port], rhs: expr.clone() });
            signals.push(sig);
        };
        for (i, a) in self.assumptions.iter().enumerate() {
            let label = a.name.clone().unwrap_or_else(|| format!("assumption#{i}"));
            signal(ObserverSignal::Assumption { index: i, label }, format!("ol_a{i}"), &a.expr);
        }
        for (i, g) in self.guarantees.iter().enumerate() {
            let label = g.name.clone().unwrap_or_else(|| format!("guarantee#{i}"));
            signal(ObserverSignal::Guarantee { index: i, label }, format!("ol_g{i}"), &g.expr);
        }
        for (m, mode) in self.modes.iter().enumerate() {
            for (k, r) in mode.requires.iter().enumerate() {
                signal(ObserverSignal::Require { mode: m, index: k }, format!("ol_m{m}_r{k}"), r);
            }
            for (k, e) in mode.ensures.iter().enumerate() {
                signal(ObserverSignal::Ensure { mode: m, index: k }, format!("ol_m{m}_e{k}"), e);
            }
        }
        let node = NodeDef {
            name: format!("{}_observer", self.name),
            kind: NodeKind::Operator,
            inputs: self.inputs.iter().chain(self.outputs.iter()).cloned().collect(),
            outputs,
            locals: self
                .ghost_vars
                .iter()
                .map(|g| Local { name: g.name.clone(), ty: g.ty.clone() })
                .collect(),
            equations,
            contract: None,
            diagram: Default::default(),
            probes: vec![],
        };
        Observer { node, signals, ghost_count: self.ghost_vars.len() }
    }
}

/// A contract compiled to an observer node (see [`ContractDef::observer`]).
#[derive(Debug, Clone)]
pub struct Observer {
    pub node: ol_ir::NodeDef,
    /// What each output of `node` stands for, in output order.
    pub signals: Vec<ObserverSignal>,
    /// The first `ghost_count` equations of `node` define ghost variables.
    pub ghost_count: usize,
}

/// One Boolean output of an observer node.
#[derive(Debug, Clone, PartialEq)]
pub enum ObserverSignal {
    /// Assumption `index` holds; `label` names it in violation reports.
    Assumption { index: usize, label: String },
    /// Guarantee `index` holds.
    Guarantee { index: usize, label: String },
    /// Require `index` of mode `mode` holds (a mode is active when all do).
    Require { mode: usize, index: usize },
    /// Ensure `index` of mode `mode` holds (checked only while it is active).
    Ensure { mode: usize, index: usize },
}

impl Observer {
    /// Human wording for the clause behind equation `eq` of the observer
    /// node — the prefix of a C0080 message ("guarantee `safe`", "mode `M`
    /// require #0", "ghost `first`").
    pub fn describe_equation(&self, contract: &ContractDef, eq: usize) -> String {
        if eq < self.ghost_count {
            return format!("ghost `{}`", contract.ghost_vars[eq].name);
        }
        let mode_name = |m: usize| contract.modes.get(m).map(|x| x.name.as_str()).unwrap_or("?");
        match self.signals.get(eq - self.ghost_count) {
            Some(ObserverSignal::Assumption { index, .. }) => format!(
                "assumption `{}`",
                contract.assumptions[*index].name.clone().unwrap_or_else(|| format!("#{index}"))
            ),
            Some(ObserverSignal::Guarantee { index, .. }) => format!(
                "guarantee `{}`",
                contract.guarantees[*index].name.clone().unwrap_or_else(|| format!("#{index}"))
            ),
            Some(ObserverSignal::Require { mode, index }) => {
                format!("mode `{}` require #{index}", mode_name(*mode))
            }
            Some(ObserverSignal::Ensure { mode, index }) => {
                format!("mode `{}` ensure #{index}", mode_name(*mode))
            }
            None => "interface".to_string(),
        }
    }

    /// Interpret one cycle's signal values (in `signals` order) the way both
    /// monitors report it: the active modes, and the violated clauses —
    /// assumptions, then guarantees, then the ensures of active modes.
    pub fn verdict(&self, contract: &ContractDef, values: &[bool]) -> (Vec<String>, Vec<String>) {
        let mut violations = Vec::new();
        for (sig, ok) in self.signals.iter().zip(values) {
            match sig {
                ObserverSignal::Assumption { label, .. } | ObserverSignal::Guarantee { label, .. }
                    if !ok =>
                {
                    violations.push(label.clone())
                }
                _ => {}
            }
        }
        let mut active = Vec::new();
        for (m, mode) in contract.modes.iter().enumerate() {
            let holds = |want: fn(&ObserverSignal, usize) -> bool| {
                self.signals.iter().zip(values).filter(|(s, _)| want(s, m)).all(|(_, ok)| *ok)
            };
            if !holds(|s, m| matches!(s, ObserverSignal::Require { mode, .. } if *mode == m)) {
                continue;
            }
            active.push(mode.name.clone());
            for (sig, ok) in self.signals.iter().zip(values) {
                if let ObserverSignal::Ensure { mode: em, index } = sig {
                    if *em == m && !ok {
                        violations.push(format!("{}::ensure#{index}", mode.name));
                    }
                }
            }
        }
        (active, violations)
    }
}
