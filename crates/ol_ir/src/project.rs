use serde::{Deserialize, Serialize};

use crate::expr::Expr;
use crate::node::NodeDef;
use crate::state_machine::{lower, LowerError, StateMachineDef};
use crate::types::Type;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnumDef {
    pub name: String,
    pub variants: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordField {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TypeBody {
    Enum(EnumDef),
    Record { name: String, fields: Vec<RecordField> },
    Alias { name: String, target: Type },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypeDef {
    pub body: TypeBody,
}

impl TypeDef {
    pub fn name(&self) -> &str {
        match &self.body {
            TypeBody::Enum(e) => &e.name,
            TypeBody::Record { name, .. } => name,
            TypeBody::Alias { name, .. } => name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConstDef {
    pub name: String,
    pub ty: Type,
    pub value: Expr,
}

/// A package groups types, constants, nodes, contracts, and imported
/// operators. Contracts are stored as plain JSON values here so that the IR
/// crate does not depend on `ol_contract_ir` (the contract crate depends on
/// `ol_ir`, not the other way around). Higher layers re-hydrate them.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    #[serde(default)]
    pub types: Vec<TypeDef>,
    #[serde(default)]
    pub constants: Vec<ConstDef>,
    #[serde(default)]
    pub nodes: Vec<NodeDef>,
    /// Raw contract definitions; parsed by `ol_contract_ir`.
    #[serde(default)]
    pub contracts: Vec<serde_json::Value>,
    /// Imported operator manifests; parsed by `ol_clite_emit`.
    #[serde(default)]
    pub imported_operators: Vec<serde_json::Value>,
    /// Finite state machines. They are lowered to dataflow nodes (and an
    /// auto-generated state-enum type) by [`Project::lower_state_machines`]
    /// before any downstream tool runs.
    #[serde(default)]
    pub state_machines: Vec<StateMachineDef>,
}

impl Package {
    pub fn find_node(&self, name: &str) -> Option<&NodeDef> {
        self.nodes.iter().find(|n| n.name == name)
    }

    pub fn find_type(&self, name: &str) -> Option<&TypeDef> {
        self.types.iter().find(|t| t.name() == name)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub packages: Vec<Package>,
    /// Optional default entry point; used by simulator and Kind 2 adapter.
    #[serde(default)]
    pub main: Option<String>,
    /// Relative paths to other project files to merge into this one. The
    /// loader follows these recursively and concatenates packages by name.
    #[serde(default)]
    pub includes: Vec<String>,
}

impl Project {
    pub fn find_node(&self, name: &str) -> Option<&NodeDef> {
        for pkg in &self.packages {
            if let Some(n) = pkg.find_node(name) {
                return Some(n);
            }
        }
        None
    }

    pub fn all_nodes(&self) -> impl Iterator<Item = &NodeDef> {
        self.packages.iter().flat_map(|p| p.nodes.iter())
    }

    /// Merge `other` into `self`. Packages with the same name combine their
    /// types/constants/nodes/contracts/imports/state-machines; packages whose
    /// names do not yet exist are appended. `main` is inherited from `other`
    /// only if `self.main` is unset. Detection of duplicate definitions is
    /// left to the type and contract checkers.
    pub fn merge(&mut self, other: Project) {
        for src_pkg in other.packages {
            if let Some(dst_pkg) = self.packages.iter_mut().find(|p| p.name == src_pkg.name) {
                dst_pkg.types.extend(src_pkg.types);
                dst_pkg.constants.extend(src_pkg.constants);
                dst_pkg.nodes.extend(src_pkg.nodes);
                dst_pkg.contracts.extend(src_pkg.contracts);
                dst_pkg.imported_operators.extend(src_pkg.imported_operators);
                dst_pkg.state_machines.extend(src_pkg.state_machines);
            } else {
                self.packages.push(src_pkg);
            }
        }
        if self.main.is_none() {
            self.main = other.main;
        }
    }

    /// Slice this project down to `root` and everything it transitively
    /// uses — the SCADE-style "generate the selected operator and all that
    /// are used by that model" selection. See [`crate::slice::slice_for_root`].
    pub fn slice_for_root(&self, root: &str) -> Result<Project, String> {
        crate::slice::slice_for_root(self, root)
    }

    /// Replace each [`StateMachineDef`] in every package with the dataflow
    /// node and state-enum type it lowers to. After this call, downstream
    /// tools see only ordinary nodes and types and need no per-tool
    /// awareness of state machines.
    pub fn lower_state_machines(&mut self) -> Result<(), Vec<LowerError>> {
        let mut errors = Vec::new();
        for pkg in &mut self.packages {
            let machines = std::mem::take(&mut pkg.state_machines);
            // Resolve `refines` references against the package's machines
            // (so a state can delegate to another machine), then lower.
            let by_name: std::collections::HashMap<String, crate::StateMachineDef> =
                machines.iter().map(|m| (m.name.clone(), m.clone())).collect();
            for sm in &machines {
                let resolved = match crate::state_machine::resolve_refines(sm, &by_name) {
                    Ok(r) => r,
                    Err(e) => {
                        errors.push(e);
                        continue;
                    }
                };
                let low = match lower(&resolved) {
                    Ok(l) => l,
                    Err(e) => {
                        errors.push(e);
                        continue;
                    }
                };
                pkg.types.extend(low.state_types);
                match &sm.owner {
                    // Owner-less (e.g. stdlib library blocks): a standalone node.
                    None => pkg.nodes.push(low.node),
                    // Operator-owned: merge the automaton into the operator's
                    // body — its state locals and state/next/output equations
                    // drive the operator's outputs (no separate node).
                    Some(op) => match pkg.nodes.iter_mut().find(|n| &n.name == op) {
                        Some(node) => {
                            node.locals.extend(low.node.locals);
                            node.equations.extend(low.node.equations);
                        }
                        None => errors
                            .push(LowerError::UnknownOwner(sm.name.clone(), op.clone())),
                    },
                }
            }
        }
        if errors.is_empty() {
            self.desugar_activations();
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Split SCADE-style activations into the clock profile's supported
    /// shape. The parser sugars `activate(F, c, d, args…)` into
    /// `merge(c, F(args when c), d when not c)`; when `F` is STATEFUL, its
    /// activation clock must be explicit at the equation level so both
    /// backends freeze its state off-cycles — so each such call moves into
    /// its own fresh-local equation:
    ///
    /// ```text
    /// y = merge(c, F(x when c), d when not c);
    ///   ⇒  __act0 = F(x when c);
    ///      y = merge(c, __act0, d when not c);
    /// ```
    ///
    /// Only the exact activate shape is rewritten (a stateful single-output
    /// call whose every argument is `when` the merge's own clock); anything
    /// else is left for the typechecker to judge.
    fn desugar_activations(&mut self) {
        use crate::expr::Expr;
        // (name -> (stateful, single output type)) across the whole project.
        let sigs: std::collections::HashMap<String, (bool, Option<crate::Type>)> = self
            .packages
            .iter()
            .flat_map(|p| p.nodes.iter())
            .map(|n| {
                (
                    n.name.clone(),
                    (
                        !matches!(n.kind, crate::NodeKind::Function),
                        (n.outputs.len() == 1).then(|| n.outputs[0].ty.clone()),
                    ),
                )
            })
            .collect();
        for pkg in &mut self.packages {
            for node in &mut pkg.nodes {
                let mut fresh = 0usize;
                let mut hoisted: Vec<(String, crate::Type, Expr)> = Vec::new();
                let taken: std::collections::HashSet<String> = node
                    .inputs
                    .iter()
                    .map(|p| p.name.clone())
                    .chain(node.outputs.iter().map(|p| p.name.clone()))
                    .chain(node.locals.iter().map(|l| l.name.clone()))
                    .collect();
                for eq in &mut node.equations {
                    eq.rhs.visit_mut(&mut |e: &mut Expr| {
                        let Expr::Merge { clock, on_true, .. } = e else { return };
                        let Expr::Call { node: f, args } = on_true.as_mut() else { return };
                        let Some((true, Some(out_ty))) = sigs.get(f.as_str()) else { return };
                        let is_activate_shape = !args.is_empty()
                            && args.iter().all(|a| matches!(
                                a,
                                Expr::When { on: true, clock: c, .. } if c == clock
                            ));
                        if !is_activate_shape {
                            return;
                        }
                        let mut name = format!("__act{fresh}");
                        while taken.contains(&name) {
                            fresh += 1;
                            name = format!("__act{fresh}");
                        }
                        fresh += 1;
                        let call = std::mem::replace(
                            on_true.as_mut(),
                            Expr::Var { name: name.clone() },
                        );
                        hoisted.push((name, out_ty.clone(), call));
                    });
                }
                for (name, ty, call) in hoisted {
                    node.locals.push(crate::Local { name: name.clone(), ty });
                    node.equations.push(crate::Equation { lhs: vec![name], rhs: call });
                }
            }
        }
    }
}
