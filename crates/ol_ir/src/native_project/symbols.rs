//! Resolve authored names without mutating any project source. A declaration's
//! identity is its owner project, declaration kind, and local name; consumer
//! aliases only select an owner at reference sites.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{Expr, NodeDef, Package, Port, Project, Type, TypeBody};

use super::{NativeResolution, NativeSource, NativeSymbol, NativeUnit};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Node,
    Type,
    Constant,
    Contract,
}

impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Self::Node => "n",
            Self::Type => "t",
            Self::Constant => "c",
            Self::Contract => "k",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Type => "type",
            Self::Constant => "constant",
            Self::Contract => "contract",
        }
    }
}

#[derive(Default)]
struct Inventory {
    declarations: BTreeMap<(Kind, String), String>,
    /// Local bare enum variants retain the legacy per-project uniqueness rule.
    variants: BTreeMap<String, (String, String)>,
    enum_names: BTreeSet<String>,
    enum_variants: BTreeMap<(String, String), String>,
    exported: BTreeSet<(Kind, String)>,
    aliases: BTreeMap<String, String>,
}

/// Use complete, separately delimited UTF-8 hex fields, rather than truncated
/// hashes or ambiguous concatenation. All resulting names are valid C/Lustre
/// identifiers, and the encoding is injective for the complete symbol tuple.
fn backend_name(owner: &str, kind: Kind, local: &str) -> String {
    format!("olp_{}_{}_{}", hex(owner), kind.tag(), hex(local))
}

fn variant_name(owner: &str, enum_name: &str, variant: &str) -> String {
    format!("olp_{}_v_{}_{}", hex(owner), hex(enum_name), hex(variant))
}

fn hex(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.as_bytes() {
        write!(&mut out, "{b:02x}").expect("writing to a String cannot fail");
    }
    out
}

fn inventory(owner: &str, unit: &NativeUnit) -> Result<Inventory, String> {
    let mut inv = Inventory::default();
    let mut add = |kind: Kind, name: &str| -> Result<(), String> {
        if name.is_empty() || name.contains("::") {
            return Err(format!(
                "project `{owner}` declares invalid local {} name `{name}`",
                kind.label()
            ));
        }
        if inv
            .declarations
            .insert((kind, name.to_owned()), backend_name(owner, kind, name))
            .is_some()
        {
            return Err(format!(
                "project `{owner}` declares duplicate {} `{name}` across its packages",
                kind.label()
            ));
        }
        Ok(())
    };
    for pkg in &unit.project.packages {
        for node in &pkg.nodes {
            add(Kind::Node, &node.name)?;
        }
        for ty in &pkg.types {
            add(Kind::Type, ty.name())?;
        }
        for constant in &pkg.constants {
            add(Kind::Constant, &constant.name)?;
        }
        for contract in &pkg.contracts {
            let name = contract.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
                format!("project `{owner}` has a contract without a string name")
            })?;
            add(Kind::Contract, name)?;
        }
        if !pkg.imported_operators.is_empty() || pkg.nodes.iter().any(NodeDef::is_imported) {
            return Err(format!(
                "project `{owner}` uses imported C operators, which native project format 1 does not support"
            ));
        }
        if !pkg.state_machines.is_empty() || !pkg.activations.is_empty() {
            return Err(format!(
                "project `{owner}` contains constructs that were not locally lowered"
            ));
        }
    }
    for pkg in &unit.project.packages {
        for ty in &pkg.types {
            if let TypeBody::Enum(def) = &ty.body {
                inv.enum_names.insert(def.name.clone());
                for variant in &def.variants {
                    if variant.is_empty() || variant.contains("::") {
                        return Err(format!(
                            "project `{owner}` declares invalid enum variant `{variant}`"
                        ));
                    }
                    let resolved = variant_name(owner, &def.name, variant);
                    if inv
                        .variants
                        .insert(variant.clone(), (def.name.clone(), resolved.clone()))
                        .is_some()
                    {
                        return Err(format!(
                            "project `{owner}` declares duplicate enum variant `{variant}` across its enums"
                        ));
                    }
                    inv.enum_variants.insert((def.name.clone(), variant.clone()), resolved);
                }
            }
        }
    }
    for (kind, names) in [
        (Kind::Node, &unit.manifest.exports.nodes),
        (Kind::Type, &unit.manifest.exports.types),
        (Kind::Constant, &unit.manifest.exports.constants),
        (Kind::Contract, &unit.manifest.exports.contracts),
    ] {
        for name in names {
            let key = (kind, name.clone());
            if !inv.declarations.contains_key(&key) {
                return Err(format!(
                    "project `{owner}` exports unknown {} `{name}`",
                    kind.label()
                ));
            }
            if !inv.exported.insert(key) {
                return Err(format!(
                    "project `{owner}` exports {} `{name}` more than once",
                    kind.label()
                ));
            }
        }
    }
    for dependency in &unit.manifest.dependencies {
        if inv.enum_names.contains(&dependency.alias) {
            return Err(format!(
                "project `{owner}` dependency alias `{}` conflicts with a local enum type",
                dependency.alias
            ));
        }
        if inv.aliases.insert(dependency.alias.clone(), dependency.project_id.clone()).is_some() {
            return Err(format!(
                "project `{owner}` repeats dependency alias `{}`",
                dependency.alias
            ));
        }
    }
    if !inv.declarations.contains_key(&(Kind::Node, unit.manifest.entrypoint.clone())) {
        return Err(format!(
            "project `{owner}` entrypoint `{}` is not a local node",
            unit.manifest.entrypoint
        ));
    }
    Ok(inv)
}

struct Resolver<'a> {
    owner: &'a str,
    inventories: &'a BTreeMap<String, Inventory>,
}

impl Resolver<'_> {
    fn own(&self) -> &Inventory {
        &self.inventories[self.owner]
    }

    fn dependency(&self, alias: &str) -> Result<(&str, &Inventory), String> {
        let id = self.own().aliases.get(alias).ok_or_else(|| {
            format!("project `{}` refers to unknown dependency alias `{alias}`", self.owner)
        })?;
        let target = self.inventories.get(id).ok_or_else(|| {
            format!("project `{}` dependency `{alias}` has no resolved owner `{id}`", self.owner)
        })?;
        Ok((id, target))
    }

    fn declaration(&self, kind: Kind, name: &str) -> Result<String, String> {
        let parts: Vec<_> = name.split("::").collect();
        match parts.as_slice() {
            [local] => self
                .own()
                .declarations
                .get(&(kind, (*local).to_owned()))
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "project `{}` refers to unknown local {} `{name}`; dependency references require an alias",
                        self.owner,
                        kind.label()
                    )
                }),
            [alias, local] => {
                let (id, target) = self.dependency(alias)?;
                let key = (kind, (*local).to_owned());
                let resolved = target.declarations.get(&key).ok_or_else(|| {
                    format!("dependency `{alias}` ({id}) has no {} `{local}`", kind.label())
                })?;
                if !target.exported.contains(&key) {
                    return Err(format!(
                        "dependency `{alias}` ({id}) does not export {} `{local}`",
                        kind.label()
                    ));
                }
                Ok(resolved.clone())
            }
            _ => Err(format!(
                "project `{}` has malformed {} reference `{name}`",
                self.owner,
                kind.label()
            )),
        }
    }

    fn value(&self, name: &str, locals: &BTreeSet<String>) -> Result<String, String> {
        let parts: Vec<_> = name.split("::").collect();
        match parts.as_slice() {
            [local] => {
                if locals.contains(*local) {
                    return Ok(name.to_owned());
                }
                if let Some(resolved) = self.own().declarations.get(&(Kind::Constant, name.to_owned())) {
                    return Ok(resolved.clone());
                }
                self.own().variants.get(*local).map(|(_, resolved)| resolved.clone()).ok_or_else(|| {
                    format!(
                        "project `{}` refers to unknown value `{name}`; dependency references require an alias",
                        self.owner
                    )
                })
            }
            [first, second] => {
                // An authored local enum qualifier wins over a consumer alias,
                // consistently with local declarations taking precedence.
                if let Some(resolved) = self.own().enum_variants.get(&((*first).to_owned(), (*second).to_owned())) {
                    return Ok(resolved.clone());
                }
                if self.own().enum_names.contains(*first) {
                    return Err(format!(
                        "project `{}` has no local enum variant `{name}`",
                        self.owner
                    ));
                }
                self.declaration(Kind::Constant, name)
            }
            [alias, enum_name, variant] => {
                let (id, target) = self.dependency(alias)?;
                if !target.exported.contains(&(Kind::Type, (*enum_name).to_owned())) {
                    return Err(format!(
                        "dependency `{alias}` ({id}) does not export enum type `{enum_name}`"
                    ));
                }
                target
                    .enum_variants
                    .get(&((*enum_name).to_owned(), (*variant).to_owned()))
                    .cloned()
                    .ok_or_else(|| format!("dependency `{alias}` ({id}) has no enum variant `{enum_name}::{variant}`"))
            }
            _ => Err(format!("project `{}` has malformed value reference `{name}`", self.owner)),
        }
    }

    fn ty(&self, ty: &mut Type) -> Result<(), String> {
        match ty {
            Type::Named { name } => *name = self.declaration(Kind::Type, name)?,
            Type::Array { elem, .. } => self.ty(elem)?,
            _ => {}
        }
        Ok(())
    }

    fn validate_locals(&self, locals: &BTreeSet<String>) -> Result<(), String> {
        // Authored ports and locals stay unchanged. A local that happens to
        // spell another declaration's backend name would otherwise capture a
        // qualified constant/enum reference after rewriting (or shadow a C
        // function/typedef). Reject that exact collision before rewriting.
        let generated = self.generated_identifiers();
        for local in locals {
            if generated.contains(local) {
                return Err(format!(
                    "project `{}` local/port/ghost `{local}` collides with a generated symbol",
                    self.owner
                ));
            }
        }
        Ok(())
    }

    fn generated_identifiers(&self) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        let mut node_artifacts = |prefix: &str| {
            names.insert(prefix.to_owned());
            for suffix in ["_step", "_init", "_Input", "_Output", "_State"] {
                names.insert(format!("{prefix}{suffix}"));
            }
        };
        for inventory in self.inventories.values() {
            for ((kind, _), name) in &inventory.declarations {
                if *kind == Kind::Node {
                    node_artifacts(name);
                } else if *kind == Kind::Contract {
                    node_artifacts(&format!("{name}_observer"));
                }
            }
        }
        for inventory in self.inventories.values() {
            names.extend(inventory.declarations.values().cloned());
            names.extend(inventory.enum_variants.values().cloned());
            for ((kind, _), name) in &inventory.declarations {
                if *kind == Kind::Contract {
                    for suffix in ["_monitor", "_monitor_State", "_monitor_reset", "_monitor_check"] {
                        names.insert(format!("{name}{suffix}"));
                    }
                }
            }
        }
        names
    }

    fn validate_field(&self, field: &str) -> Result<(), String> {
        // A C preprocessor macro expands even after `record.` or inside a
        // struct declaration. Member namespaces do not protect a record
        // field from a generated scalar/enum constant macro.
        if self.inventories.values().any(|inventory| {
            inventory.declarations.iter().any(|((kind, _), name)| {
                *kind == Kind::Constant && name == field
            })
        }) {
            return Err(format!(
                "project `{}` record field `{field}` collides with a generated constant macro",
                self.owner
            ));
        }
        Ok(())
    }

    fn expr(&self, expr: &mut Expr, locals: &BTreeSet<String>) -> Result<(), String> {
        match expr {
            Expr::Var { name } => *name = self.value(name, locals)?,
            Expr::Call { node, .. } | Expr::Iterate { node, .. } => {
                *node = self.declaration(Kind::Node, node)?;
            }
            Expr::Struct { ty, .. } => *ty = self.declaration(Kind::Type, ty)?,
            Expr::Cast { to, .. } => self.ty(to)?,
            Expr::When { clock, .. } | Expr::Merge { clock, .. } => {
                *clock = self.value(clock, locals)?;
            }
            _ => {}
        }
        let mut result = Ok(());
        expr.for_each_child_mut(&mut |child| {
            if result.is_ok() {
                result = self.expr(child, locals);
            }
        });
        result
    }

    fn node(&self, node: &mut NodeDef) -> Result<(), String> {
        let locals: BTreeSet<_> = node
            .inputs
            .iter()
            .chain(node.outputs.iter())
            .map(|port| port.name.clone())
            .chain(node.locals.iter().map(|local| local.name.clone()))
            .collect();
        self.validate_locals(&locals)?;
        for port in node.inputs.iter_mut().chain(node.outputs.iter_mut()) {
            self.ty(&mut port.ty)?;
        }
        for local in &mut node.locals {
            self.ty(&mut local.ty)?;
        }
        for equation in &mut node.equations {
            self.expr(&mut equation.rhs, &locals)?;
        }
        if let Some(contract) = &mut node.contract {
            *contract = self.declaration(Kind::Contract, contract)?;
        }
        node.name = self.declaration(Kind::Node, &node.name)?;
        Ok(())
    }

    fn contract(&self, raw: &mut serde_json::Value) -> Result<(), String> {
        // Mirroring the data-only contract schema avoids a dependency cycle
        // (ol_contract_ir already depends on ol_ir). Flattened maps preserve
        // extension fields that are unrelated to symbol resolution.
        let mut contract: Contract = serde_json::from_value(raw.clone()).map_err(|error| {
            format!("project `{}` has an invalid contract: {error}", self.owner)
        })?;
        let locals: BTreeSet<_> = contract
            .inputs
            .iter()
            .chain(contract.outputs.iter())
            .map(|port| port.name.clone())
            .chain(contract.ghost_vars.iter().map(|ghost| ghost.name.clone()))
            .collect();
        self.validate_locals(&locals)?;
        for port in contract.inputs.iter_mut().chain(contract.outputs.iter_mut()) {
            self.ty(&mut port.ty)?;
        }
        for ghost in &mut contract.ghost_vars {
            self.ty(&mut ghost.ty)?;
            self.expr(&mut ghost.definition, &locals)?;
        }
        for clause in contract.assumptions.iter_mut().chain(contract.guarantees.iter_mut()) {
            self.expr(&mut clause.expr, &locals)?;
        }
        for mode in &mut contract.modes {
            for expr in mode.requires.iter_mut().chain(mode.ensures.iter_mut()) {
                self.expr(expr, &locals)?;
            }
        }
        for import in &mut contract.imports {
            import.contract = self.declaration(Kind::Contract, &import.contract)?;
            for (_, expr) in &mut import.input_map {
                self.expr(expr, &locals)?;
            }
            // output_map names are local port names on both interfaces.
        }
        contract.name = self.declaration(Kind::Contract, &contract.name)?;
        *raw = serde_json::to_value(contract).map_err(|error| error.to_string())?;
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
struct Contract {
    name: String,
    inputs: Vec<Port>,
    outputs: Vec<Port>,
    #[serde(default)]
    ghost_vars: Vec<Ghost>,
    #[serde(default)]
    assumptions: Vec<Clause>,
    #[serde(default)]
    guarantees: Vec<Clause>,
    #[serde(default)]
    modes: Vec<Mode>,
    #[serde(default)]
    imports: Vec<Import>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize, Serialize)]
struct Ghost {
    name: String,
    ty: Type,
    definition: Expr,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize, Serialize)]
struct Clause {
    name: Option<String>,
    expr: Expr,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize, Serialize)]
struct Mode {
    name: String,
    #[serde(default)]
    requires: Vec<Expr>,
    #[serde(default)]
    ensures: Vec<Expr>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize, Serialize)]
struct Import {
    contract: String,
    input_map: Vec<(String, Expr)>,
    output_map: Vec<(String, String)>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

/// Evaluators initialize constants and emitters declare by-value types in
/// declaration order. Ownership traversal order cannot establish those
/// dependencies. Sort the transient native package after rewriting, leaving
/// legacy projects and authored library files untouched.
fn order_declarations(package: &mut Package, symbols: &[NativeSymbol]) -> Result<(), String> {
    let constant_names: Vec<_> = package.constants.iter().map(|item| item.name.clone()).collect();
    let constant_dependencies: Vec<_> = package.constants.iter().map(|item| item.value.free_vars()).collect();
    let order = dependency_order(&constant_names, &constant_dependencies, "constant", symbols)?;
    reorder(&mut package.constants, order);

    let type_names: Vec<_> = package.types.iter().map(|item| item.name().to_owned()).collect();
    let type_dependencies: Vec<_> = package.types.iter().map(|item| {
        let mut names = Vec::new();
        match &item.body {
            TypeBody::Record { fields, .. } => {
                for field in fields { named_dependencies(&field.ty, &mut names); }
            }
            TypeBody::Alias { target, .. } => named_dependencies(target, &mut names),
            TypeBody::Enum(_) => {}
        }
        names
    }).collect();
    let order = dependency_order(&type_names, &type_dependencies, "type", symbols)?;
    reorder(&mut package.types, order);
    Ok(())
}

fn named_dependencies(ty: &Type, out: &mut Vec<String>) {
    match ty {
        Type::Named { name } => out.push(name.clone()),
        Type::Array { elem, .. } => named_dependencies(elem, out),
        _ => {}
    }
}

fn dependency_order(
    names: &[String],
    dependencies: &[Vec<String>],
    kind: &str,
    symbols: &[NativeSymbol],
) -> Result<Vec<usize>, String> {
    let indices: BTreeMap<_, _> = names.iter().enumerate().map(|(index, name)| (name.as_str(), index)).collect();
    let mut indegrees = vec![0; names.len()];
    let mut consumers = vec![Vec::new(); names.len()];
    for (index, refs) in dependencies.iter().enumerate() {
        let dependencies: BTreeSet<_> = refs.iter().filter_map(|name| indices.get(name.as_str()).copied()).collect();
        indegrees[index] = dependencies.len();
        for dependency in dependencies { consumers[dependency].push(index); }
    }
    let mut ready: BTreeSet<_> = indegrees.iter().enumerate().filter_map(|(index, degree)| (*degree == 0).then_some(index)).collect();
    let mut order = Vec::with_capacity(names.len());
    while let Some(index) = ready.pop_first() {
        order.push(index);
        for consumer in &consumers[index] {
            indegrees[*consumer] -= 1;
            if indegrees[*consumer] == 0 { ready.insert(*consumer); }
        }
    }
    if order.len() != names.len() {
        let blocked: Vec<_> = names.iter().enumerate().filter(|(index, _)| indegrees[*index] > 0).map(|(_, name)| {
            symbols.iter().find(|symbol| symbol.kind == kind && symbol.resolved_name == *name)
                .map(|symbol| format!("{}::{}", symbol.project_id, symbol.local_name))
                .unwrap_or_else(|| name.clone())
        }).collect();
        return Err(format!("cyclic {kind} dependency prevents declaration of: {}", blocked.join(", ")));
    }
    Ok(order)
}

fn reorder<T>(items: &mut Vec<T>, order: Vec<usize>) {
    let mut original: Vec<_> = std::mem::take(items).into_iter().map(Some).collect();
    items.extend(order.into_iter().map(|index| {
        original[index].take().expect("each declaration appears once in the topological order")
    }));
}

pub(super) fn resolve_graph(
    root_id: &str,
    units: &BTreeMap<String, NativeUnit>,
) -> Result<Project, String> {
    let root = units.get(root_id).ok_or_else(|| format!("missing root project `{root_id}`"))?;
    let inventories: BTreeMap<_, _> = units
        .iter()
        .map(|(owner, unit)| inventory(owner, unit).map(|inv| (owner.clone(), inv)))
        .collect::<Result<_, _>>()?;
    let mut package = Package { name: "native_resolved".to_owned(), ..Default::default() };
    let mut symbols = Vec::new();
    let mut sources = Vec::new();
    let mut origins = Vec::new();
    for (owner, unit) in units {
        let resolver = Resolver { owner, inventories: &inventories };
        let inv = &inventories[owner];
        for ((kind, local), resolved) in &inv.declarations {
            symbols.push(NativeSymbol {
                project_id: owner.clone(),
                kind: kind.label().to_owned(),
                local_name: local.clone(),
                resolved_name: resolved.clone(),
                source_path: unit
                    .sources
                    .get(&format!("{}:{local}", kind.label()))
                    .cloned()
                    .unwrap_or_else(|| unit.manifest_path.clone()),
                exported: inv.exported.contains(&(*kind, local.clone())),
                artifact_basename: None,
            });
        }
        for ((enum_name, variant), resolved) in &inv.enum_variants {
            symbols.push(NativeSymbol {
                project_id: owner.clone(),
                kind: "enum_variant".to_owned(),
                local_name: format!("{enum_name}::{variant}"),
                resolved_name: resolved.clone(),
                source_path: unit
                    .sources
                    .get(&format!("type:{enum_name}"))
                    .cloned()
                    .unwrap_or_else(|| unit.manifest_path.clone()),
                exported: inv.exported.contains(&(Kind::Type, enum_name.clone())),
                artifact_basename: None,
            });
        }
        // Lowered constructs need their own authored source evidence: an
        // activation or machine may live in an owned include different from
        // its operator. These IDs describe constructs rather than C exports.
        for (key, source_path) in &unit.sources {
            if let Some((kind, local_name)) = key.split_once(':') {
                let tag = match kind {
                    "state_machine" => "sm",
                    "activation" => "act",
                    _ => continue,
                };
                symbols.push(NativeSymbol {
                    project_id: owner.clone(),
                    kind: kind.to_owned(),
                    local_name: local_name.to_owned(),
                    resolved_name: format!("olp_{}_{}_{}", hex(owner), tag, hex(local_name)),
                    source_path: source_path.clone(),
                    exported: false,
                    artifact_basename: None,
                });
            }
        }
        sources.push(NativeSource {
            project_id: owner.clone(),
            snapshot_sha256: unit.snapshot_sha256.clone(),
            manifest_path: unit.manifest_path.clone(),
            model_files: unit.model_files.clone(),
            dependencies: unit.manifest.dependencies.clone(),
        });
        for mut origin in unit.project.origins.clone() {
            origin.node = resolver.declaration(Kind::Node, &origin.node)?;
            origins.push(origin);
        }
        for mut pkg in unit.project.packages.clone() {
            for ty in &mut pkg.types {
                match &mut ty.body {
                    TypeBody::Enum(def) => {
                        for variant in &mut def.variants {
                            *variant = inv.enum_variants[&(def.name.clone(), variant.clone())].clone();
                        }
                        def.name = resolver.declaration(Kind::Type, &def.name)?;
                    }
                    TypeBody::Record { name, fields } => {
                        for field in fields {
                            resolver.validate_field(&field.name)?;
                            resolver.ty(&mut field.ty)?;
                        }
                        *name = resolver.declaration(Kind::Type, name)?;
                    }
                    TypeBody::Alias { name, target } => {
                        resolver.ty(target)?;
                        *name = resolver.declaration(Kind::Type, name)?;
                    }
                }
            }
            for constant in &mut pkg.constants {
                resolver.ty(&mut constant.ty)?;
                resolver.expr(&mut constant.value, &BTreeSet::new())?;
                constant.name = resolver.declaration(Kind::Constant, &constant.name)?;
            }
            for node in &mut pkg.nodes {
                resolver.node(node)?;
            }
            for contract in &mut pkg.contracts {
                resolver.contract(contract)?;
            }
            package.types.extend(pkg.types);
            package.constants.extend(pkg.constants);
            package.nodes.extend(pkg.nodes);
            package.contracts.extend(pkg.contracts);
        }
    }
    order_declarations(&mut package, &symbols)?;
    let mut resolution = NativeResolution { root_project_id: root_id.to_owned(), projects: sources, symbols };
    super::assign_artifact_names(&mut resolution)?;
    let resolver = Resolver { owner: root_id, inventories: &inventories };
    Ok(Project {
        name: root.project.name.clone(),
        packages: vec![package],
        main: Some(resolver.declaration(Kind::Node, &root.manifest.entrypoint)?),
        includes: Vec::new(),
        origins,
        resolution: Some(resolution),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_tuple_encoding_is_safe_and_injective() {
        let names = [
            backend_name("a_b", Kind::Node, "c"),
            backend_name("a", Kind::Node, "b_c"),
            backend_name("a", Kind::Type, "b_c"),
            backend_name("a", Kind::Node, "B_c"),
            backend_name("a", Kind::Node, "日本語"),
            variant_name("a", "b", "c_d"),
            variant_name("a", "b_c", "d"),
        ];
        assert_eq!(names.iter().collect::<BTreeSet<_>>().len(), names.len());
        for name in names {
            assert!(name.starts_with("olp_"));
            assert!(name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'));
        }
    }

    fn example_inventories() -> BTreeMap<String, Inventory> {
        let mut owner = Inventory::default();
        owner.declarations.insert((Kind::Constant, "signal".into()), backend_name("root", Kind::Constant, "signal"));
        owner.aliases.insert("public".into(), "shared".into());
        owner.aliases.insert("other_alias".into(), "shared".into());
        let mut shared = Inventory::default();
        for (kind, name) in [(Kind::Node, "Step"), (Kind::Constant, "Limit"), (Kind::Type, "Status"), (Kind::Node, "Private")] {
            shared.declarations.insert((kind, name.into()), backend_name("shared", kind, name));
        }
        for (kind, name) in [(Kind::Node, "Step"), (Kind::Constant, "Limit"), (Kind::Type, "Status")] {
            shared.exported.insert((kind, name.into()));
        }
        shared.enum_names.insert("Status".into());
        shared.variants.insert("Idle".into(), ("Status".into(), variant_name("shared", "Status", "Idle")));
        shared.enum_variants.insert(("Status".into(), "Idle".into()), variant_name("shared", "Status", "Idle"));
        BTreeMap::from([("root".into(), owner), ("shared".into(), shared)])
    }

    #[test]
    fn aliases_do_not_change_identity_or_allow_private_access() {
        let inventories = example_inventories();
        let resolver = Resolver { owner: "root", inventories: &inventories };
        assert_eq!(resolver.declaration(Kind::Node, "public::Step").unwrap(), resolver.declaration(Kind::Node, "other_alias::Step").unwrap());
        assert_eq!(resolver.declaration(Kind::Type, "public::Status").unwrap(), resolver.declaration(Kind::Type, "other_alias::Status").unwrap());
        assert!(resolver.declaration(Kind::Node, "public::Private").unwrap_err().contains("does not export"));
        assert!(resolver.declaration(Kind::Node, "Step").is_err());
        assert!(resolver.declaration(Kind::Node, &backend_name("shared", Kind::Node, "Private")).is_err());
        assert!(resolver.declaration(Kind::Node, "missing::Step").unwrap_err().contains("unknown dependency alias"));
    }

    #[test]
    fn ports_shadow_own_constants_and_external_enum_values_require_the_type() {
        let mut inventories = example_inventories();
        let locals = BTreeSet::from(["signal".into()]);
        let resolver = Resolver { owner: "root", inventories: &inventories };
        assert_eq!(resolver.value("signal", &locals).unwrap(), "signal");
        assert_eq!(resolver.value("signal", &BTreeSet::new()).unwrap(), backend_name("root", Kind::Constant, "signal"));
        assert_eq!(resolver.value("public::Status::Idle", &locals).unwrap(), variant_name("shared", "Status", "Idle"));
        assert!(resolver.value("public::Idle", &locals).is_err());
        inventories.get_mut("shared").unwrap().exported.remove(&(Kind::Type, "Status".into()));
        let resolver = Resolver { owner: "root", inventories: &inventories };
        assert!(resolver.value("public::Status::Idle", &locals).unwrap_err().contains("does not export enum type"));
    }

    #[test]
    fn authored_local_cannot_capture_a_rewritten_dependency_constant() {
        let inventories = example_inventories();
        let resolver = Resolver { owner: "root", inventories: &inventories };
        let collision = backend_name("shared", Kind::Constant, "Limit");
        let mut node = NodeDef {
            name: "Root".into(),
            kind: crate::NodeKind::Operator,
            inputs: vec![Port { name: collision, ty: Type::Int32 }],
            outputs: vec![Port { name: "y".into(), ty: Type::Int32 }],
            locals: Vec::new(),
            equations: vec![crate::Equation {
                lhs: vec!["y".into()], rhs: Expr::var("public::Limit"),
            }],
            contract: None,
            diagram: Default::default(),
            probes: Vec::new(),
        };
        assert!(resolver.node(&mut node).unwrap_err().contains("collides with a generated symbol"));
        assert_eq!(node.equations[0].rhs, Expr::var("public::Limit"), "rejection happens before rewriting can create capture");
        assert!(resolver.validate_locals(&BTreeSet::from(["olp_unrelated_authored_name".into()])).is_ok(), "the prefix is not broadly reserved");
    }

    #[test]
    fn derived_c_artifacts_and_constant_macros_cannot_capture_authored_names() {
        let mut inventories = example_inventories();
        inventories.get_mut("shared").unwrap().declarations.insert(
            (Kind::Contract, "Bounds".into()), backend_name("shared", Kind::Contract, "Bounds"),
        );
        let resolver = Resolver { owner: "root", inventories: &inventories };
        let step = backend_name("shared", Kind::Node, "Step");
        let contract = backend_name("shared", Kind::Contract, "Bounds");
        for name in [
            format!("{step}_step"), format!("{step}_init"), format!("{step}_Input"),
            format!("{step}_Output"), format!("{step}_State"),
            format!("{contract}_observer_step"), format!("{contract}_observer_State"),
            format!("{contract}_monitor_check"), format!("{contract}_monitor_State"),
        ] {
            assert!(resolver.validate_locals(&BTreeSet::from([name])).unwrap_err().contains("collides with a generated symbol"));
        }
        let constant = backend_name("shared", Kind::Constant, "Limit");
        assert!(resolver.validate_field(&constant).unwrap_err().contains("constant macro"));
        assert!(resolver.validate_field("olp_unrelated_authored_field").is_ok());
        assert!(resolver.validate_locals(&BTreeSet::from(["olp_unrelated_authored_name".into()])).is_ok());
    }

    #[test]
    fn constants_follow_references_and_reject_cycles() {
        let constant = |name: &str, value| crate::ConstDef { name: name.into(), ty: Type::Int32, value };
        let mut package = Package {
            constants: vec![
                constant("consumer", Expr::bin(crate::BinOp::Add, Expr::var("provider"), Expr::var("provider"))),
                constant("independent", Expr::int_lit(3)),
                constant("provider", Expr::int_lit(8)),
                constant("chained", Expr::var("consumer")),
            ],
            ..Default::default()
        };
        order_declarations(&mut package, &[]).unwrap();
        assert_eq!(package.constants.iter().map(|constant| constant.name.as_str()).collect::<Vec<_>>(),
            ["independent", "provider", "consumer", "chained"]);
        package.constants = vec![constant("left", Expr::var("right")), constant("right", Expr::var("left"))];
        assert!(order_declarations(&mut package, &[]).unwrap_err().contains("cyclic constant dependency"));
        package.constants = vec![constant("self", Expr::var("self"))];
        assert!(order_declarations(&mut package, &[]).is_err());
    }

    #[test]
    fn record_array_and_alias_types_follow_dependencies_and_reject_recursion() {
        let mut package = Package { types: vec![
            crate::TypeDef { body: TypeBody::Record { name: "Wrap".into(), fields: vec![crate::RecordField {
                name: "nested".into(), ty: Type::Array { elem: Box::new(Type::named("Alias")), len: 2 },
            }] } },
            crate::TypeDef { body: TypeBody::Alias { name: "Alias".into(), target: Type::named("Box") } },
            crate::TypeDef { body: TypeBody::Record { name: "Box".into(), fields: vec![crate::RecordField {
                name: "value".into(), ty: Type::Int32,
            }] } },
        ], ..Default::default() };
        order_declarations(&mut package, &[]).unwrap();
        assert_eq!(package.types.iter().map(|ty| ty.name()).collect::<Vec<_>>(), ["Box", "Alias", "Wrap"]);
        package.types = vec![
            crate::TypeDef { body: TypeBody::Alias { name: "A".into(), target: Type::named("B") } },
            crate::TypeDef { body: TypeBody::Alias { name: "B".into(), target: Type::named("A") } },
        ];
        assert!(order_declarations(&mut package, &[]).unwrap_err().contains("cyclic type dependency"));
        package.types = vec![crate::TypeDef { body: TypeBody::Record { name: "Recursive".into(), fields: vec![crate::RecordField {
            name: "child".into(), ty: Type::named("Recursive"),
        }] } }];
        assert!(order_declarations(&mut package, &[]).is_err());
    }
}
