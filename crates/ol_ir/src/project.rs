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
    /// Conditional activations (SCADE "activate if" decision trees). Lowered
    /// into their owner operators by [`Project::lower_activations`] before
    /// any downstream tool runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activations: Vec<crate::ActivationDef>,
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
    /// Written first in every model file: the format it is in (see
    /// [`crate::format`]).
    #[serde(default)]
    pub format_version: crate::format::CurrentFormat,
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
    /// Provenance of lowered equations: which owned construct each block of
    /// an operator's equations came from. Filled by
    /// [`Project::lower_state_machines`] / [`Project::lower_activations`]
    /// (never saved) and read by the code generator's traceability.
    #[serde(skip)]
    pub origins: Vec<ConstructOrigin>,
}

/// A block of an operator's equations produced by lowering one owned
/// construct: equations `equations` of `node` came from state machine or
/// activation `name`.
#[derive(Debug, Clone, PartialEq)]
pub struct ConstructOrigin {
    pub node: String,
    pub kind: ConstructKind,
    pub name: String,
    pub equations: std::ops::Range<usize>,
    /// An activation's branch names, in order (the else scope is "else").
    pub branches: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstructKind {
    StateMachine,
    Activation,
}

impl ConstructKind {
    /// The diagram's id prefix for a construct block (`sm:` / `act:`).
    pub fn id_prefix(self) -> &'static str {
        match self {
            ConstructKind::StateMachine => "sm",
            ConstructKind::Activation => "act",
        }
    }
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

    /// The construct (if any) that equation `index` of `node` was lowered from.
    pub fn origin_of(&self, node: &str, index: usize) -> Option<&ConstructOrigin> {
        self.origins.iter().find(|o| o.node == node && o.equations.contains(&index))
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
                dst_pkg.activations.extend(src_pkg.activations);
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
                            let first = node.equations.len();
                            node.locals.extend(low.node.locals);
                            node.equations.extend(low.node.equations);
                            self.origins.push(ConstructOrigin {
                                node: op.clone(),
                                kind: ConstructKind::StateMachine,
                                name: sm.name.clone(),
                                equations: first..node.equations.len(),
                                branches: vec![],
                            });
                        }
                        None => errors
                            .push(LowerError::UnknownOwner(sm.name.clone(), op.clone())),
                    },
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Merge every [`crate::ActivationDef`] into its owner operator's body:
    /// the branch-selected flags become locals, and each activation output
    /// gains its selection-chain equation. After this call, downstream tools
    /// see only ordinary dataflow. Call it after
    /// [`Project::lower_state_machines`] — both constructs are owned sugar
    /// over the same operator body.
    pub fn lower_activations(&mut self) -> Result<(), Vec<crate::ActLowerError>> {
        let mut errors = Vec::new();
        // Each enum's first variant: where a `last(v)` of that type starts.
        let first_variant: std::collections::HashMap<String, String> = self
            .packages
            .iter()
            .flat_map(|p| &p.types)
            .filter_map(|t| match &t.body {
                TypeBody::Enum(e) => e.variants.first().map(|v| (e.name.clone(), v.clone())),
                _ => None,
            })
            .collect();
        for pkg in &mut self.packages {
            let activations = std::mem::take(&mut pkg.activations);
            for act in &activations {
                let Some(idx) = pkg.nodes.iter().position(|n| n.name == act.owner) else {
                    errors.push(crate::ActLowerError::UnknownOwner(act.name.clone(), act.owner.clone()));
                    continue;
                };
                // The activation defines variables the owner declares. If one
                // has since been removed (or turned into an input), lowering
                // still merges and the type checker reports it on the equation
                // (E0020 / E0022) — visible on the canvas, never a load failure.
                match crate::activation::lower(act, &pkg.nodes[idx], &first_variant) {
                    Ok(low) => {
                        let node = &mut pkg.nodes[idx];
                        let first = node.equations.len();
                        node.locals.extend(low.locals);
                        node.equations.extend(low.equations);
                        self.origins.push(ConstructOrigin {
                            node: act.owner.clone(),
                            kind: ConstructKind::Activation,
                            name: act.name.clone(),
                            equations: first..node.equations.len(),
                            branches: act
                                .branches
                                .iter()
                                .map(|b| b.name.clone())
                                .chain(std::iter::once("else".to_string()))
                                .collect(),
                        });
                    }
                    Err(e) => errors.push(e),
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}
