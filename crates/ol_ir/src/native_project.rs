//! Opt-in local reusable-project manifests and exact dependency snapshots.
//!
//! Authored model files stay separate. Loading a `.olproj` validates the
//! dependency graph, then resolves it into transient compiler IR. Legacy
//! model loading does not use this module and retains its existing behavior.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{NodeKind, Project};

mod sha256;
mod symbols;

pub const NATIVE_PROJECT_FORMAT: &str = "openlustre.project/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeManifest {
    pub format: String,
    pub project_id: String,
    pub model: String,
    pub entrypoint: String,
    pub dependencies: Vec<NativeDependency>,
    pub exports: NativeExports,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeDependency {
    /// A consumer-local qualifier. It does not form part of symbol identity.
    pub alias: String,
    /// A relative `.olproj` path. Explicit sibling project references are allowed.
    pub project: String,
    pub project_id: String,
    pub snapshot_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeExports {
    pub nodes: Vec<String>,
    pub types: Vec<String>,
    pub constants: Vec<String>,
    pub contracts: Vec<String>,
}

/// Serializable provenance of transient resolution. Owned model/source
/// paths are project-relative; manifest locations are local inspection
/// evidence, not portable dependency identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeResolution {
    pub root_project_id: String,
    pub projects: Vec<NativeSource>,
    pub symbols: Vec<NativeSymbol>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeSource {
    pub project_id: String,
    pub snapshot_sha256: String,
    pub manifest_path: String,
    pub model_files: Vec<String>,
    pub dependencies: Vec<NativeDependency>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeSymbol {
    pub project_id: String,
    pub kind: String,
    pub local_name: String,
    pub resolved_name: String,
    pub source_path: String,
    pub exported: bool,
    /// Bounded output-file stem for node artifacts; semantic identity stays
    /// in `resolved_name`. Assigned and collision-checked for the whole graph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_basename: Option<String>,
}

impl NativeResolution {
    pub fn artifact_basename(&self, resolved_node: &str) -> Option<&str> {
        self.symbols.iter().find(|s| s.kind == "node" && s.resolved_name == resolved_node)
            .and_then(|s| s.artifact_basename.as_deref())
    }
}

pub(crate) fn assign_artifact_names(resolution: &mut NativeResolution) -> Result<(), String> {
    assign_artifact_names_with(resolution, sha256::digest)
}

fn assign_artifact_names_with(resolution: &mut NativeResolution, digest: impl Fn(&[u8]) -> String) -> Result<(), String> {
    let mut names = BTreeMap::new();
    for symbol in &mut resolution.symbols {
        if symbol.kind != "node" { continue; }
        let mut bytes = b"OpenLustre native artifact name v1\0".to_vec();
        bytes.extend_from_slice(symbol.resolved_name.as_bytes());
        let basename = format!("ol_artifact_{}", digest(&bytes));
        if let Some(other) = names.insert(basename.clone(), symbol.resolved_name.clone()) {
            if other != symbol.resolved_name {
                return Err(format!("native artifact basename collision between `{other}` and `{}`", symbol.resolved_name));
            }
        }
        symbol.artifact_basename = Some(basename);
    }
    Ok(())
}

pub(crate) struct NativeUnit {
    pub manifest: NativeManifest,
    pub project: Project,
    pub snapshot_sha256: String,
    pub manifest_path: String,
    pub model_files: Vec<String>,
    /// Declaration kind/name to its authored source file. Lowered construct
    /// declarations retain the construct's source rather than the root file.
    pub sources: BTreeMap<String, String>,
}

/// Load and resolve an exact local dependency graph. Every edge is checked,
/// including a repeated diamond edge and edges in otherwise unused projects.
pub fn load_native_project(path: &Path) -> Result<Project, String> {
    let mut graph = Graph::default();
    let root_path = canonical_manifest(path)?;
    let root_id = graph.visit(&root_path, true)?;
    symbols::resolve_graph(&root_id, &graph.units)
}

/// Compute one project's content snapshot. The graph must exist and be
/// acyclic with matching project IDs; expected edge pins are intentionally not verified, so snapshots
/// can be refreshed in dependency order after an intentional local edit.
/// Loading the resulting manifest always verifies every expected pin.
/// A successful fingerprint therefore does not imply the graph is loadable:
/// pin conflicts, exports, references and supported operators are load checks.
///
/// Encoding: SHA-256 of the ASCII domain `OpenLustre native project snapshot
/// v1\0`, followed by u64 big-endian length-framed canonical manifest JSON,
/// then length-framed relative file path and raw bytes for each owned model
/// file, sorted by path. Canonical JSON uses the field order declared above;
/// pin hex is lowercase. The manifest includes ordered dependency identities
/// and expected pins, making recursive dependency integrity explicit.
pub fn snapshot_native_project(path: &Path) -> Result<String, String> {
    let mut graph = Graph::default();
    let root_path = canonical_manifest(path)?;
    graph.visit(&root_path, false)?;
    graph.paths.get(&root_path).map(|(_, snapshot)| snapshot.clone())
        .ok_or_else(|| "native snapshot root was not loaded".to_string())
}

#[derive(Default)]
struct Graph {
    active: Vec<PathBuf>,
    paths: BTreeMap<PathBuf, (String, String)>,
    units: BTreeMap<String, NativeUnit>,
}

impl Graph {
    fn visit(&mut self, path: &Path, verify: bool) -> Result<String, String> {
        if let Some(start) = self.active.iter().position(|p| p == path) {
            let chain = self.active[start..].iter().chain(std::iter::once(&path.to_path_buf()))
                .map(|p| p.display().to_string()).collect::<Vec<_>>().join(" -> ");
            return Err(format!("cyclic project dependency: {chain}"));
        }
        if let Some((id, _)) = self.paths.get(path) {
            return Ok(id.clone());
        }
        self.active.push(path.to_path_buf());
        let result = self.visit_active(path, verify);
        self.active.pop();
        result
    }

    fn visit_active(&mut self, path: &Path, verify: bool) -> Result<String, String> {
        let manifest = read_manifest(path)?;
        let root = path.parent().ok_or_else(|| format!("manifest has no parent: {}", path.display()))?;
        let mut model = ModelFiles::new(root);
        let model_path = owned_file(root, &manifest.model, "model")?;
        let mut project = model.visit(&model_path)?;
        project.main = Some(manifest.entrypoint.clone());
        let snapshot = snapshot(&manifest, &model.files)?;
        for dep in &manifest.dependencies {
            validate_relative(&dep.project, "dependency project")?;
            let dep_path = canonical_manifest(&root.join(&dep.project))?;
            let actual_id = self.visit(&dep_path, verify)?;
            let (_, actual_snapshot) = self.paths.get(&dep_path)
                .ok_or_else(|| format!("dependency was not loaded: {}", dep_path.display()))?;
            if dep.project_id != actual_id {
                return Err(format!("dependency identity mismatch in {} for alias '{}': expected '{}', found '{}'",
                    path.display(), dep.alias, dep.project_id, actual_id));
            }
            if verify && dep.snapshot_sha256 != *actual_snapshot {
                return Err(format!("snapshot mismatch in {} for alias '{}' (project '{}'): expected {}, found {}",
                    path.display(), dep.alias, dep.project_id, dep.snapshot_sha256, actual_snapshot));
            }
        }
        if verify {
            reject_external(&project, path)?;
            let model_relative = model_path.strip_prefix(root).expect("owned model is inside root").to_string_lossy();
            lower_owned_constructs(&mut project, &mut model.sources, &model_relative)?;
        }
        let project_id = manifest.project_id.clone();
        if let Some(old) = self.units.get(&project_id) {
            if verify && old.snapshot_sha256 != snapshot {
                return Err(format!("conflicting snapshots for project '{}': {} at {}, {} at {}",
                    project_id, old.snapshot_sha256, old.manifest_path, snapshot, path.display()));
            }
        } else {
            self.units.insert(project_id.clone(), NativeUnit {
                manifest,
                project,
                snapshot_sha256: snapshot.clone(),
                manifest_path: path.display().to_string(),
                model_files: model.files.keys().cloned().collect(),
                sources: model.sources,
            });
        }
        self.paths.insert(path.to_path_buf(), (project_id.clone(), snapshot));
        Ok(project_id)
    }
}

fn canonical_manifest(path: &Path) -> Result<PathBuf, String> {
    if path.extension().and_then(|s| s.to_str()) != Some("olproj") {
        return Err(format!("native project path must end in .olproj: {}", path.display()));
    }
    canonical_regular_file(path, "manifest")
}

fn canonical_regular_file(path: &Path, label: &str) -> Result<PathBuf, String> {
    let canonical = path.canonicalize()
        .map_err(|e| format!("cannot read {label} {}: {e}", path.display()))?;
    if !canonical.is_file() {
        return Err(format!("{label} must be a regular file: {}", path.display()));
    }
    Ok(canonical)
}

fn read_manifest(path: &Path) -> Result<NativeManifest, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read manifest {}: {e}", path.display()))?;
    let mut manifest: NativeManifest = serde_json::from_slice(&bytes)
        .map_err(|e| format!("invalid native manifest {}: {e}", path.display()))?;
    if manifest.format != NATIVE_PROJECT_FORMAT {
        return Err(format!("unsupported native manifest format '{}' in {}", manifest.format, path.display()));
    }
    validate_id(&manifest.project_id)?;
    validate_local_name(&manifest.entrypoint, "entrypoint")?;
    validate_relative(&manifest.model, "model")?;
    let mut aliases = BTreeSet::new();
    for dep in &mut manifest.dependencies {
        validate_local_name(&dep.alias, "dependency alias")?;
        if !aliases.insert(dep.alias.clone()) {
            return Err(format!("duplicate dependency alias '{}' in {}", dep.alias, path.display()));
        }
        validate_relative(&dep.project, "dependency project")?;
        validate_id(&dep.project_id)?;
        if dep.snapshot_sha256.len() != 64 || !dep.snapshot_sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("snapshot_sha256 must be exactly 64 hexadecimal digits for alias '{}' in {}", dep.alias, path.display()));
        }
        dep.snapshot_sha256.make_ascii_lowercase();
    }
    for (kind, names) in [
        ("node", &manifest.exports.nodes), ("type", &manifest.exports.types),
        ("constant", &manifest.exports.constants), ("contract", &manifest.exports.contracts),
    ] {
        let mut unique = BTreeSet::new();
        for name in names {
            validate_local_name(name, &format!("exported {kind}"))?;
            if !unique.insert(name) {
                return Err(format!("duplicate exported {kind} '{name}' in {}", path.display()));
            }
        }
    }
    Ok(manifest)
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 128 || !id.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("project_id must be 1..128 printable non-space ASCII characters".to_string());
    }
    Ok(())
}

fn validate_local_name(name: &str, label: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let first = chars.next();
    if !matches!(first, Some(c) if c.is_ascii_alphabetic() || c == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!("{label} must be an unqualified ASCII identifier: '{name}'"));
    }
    Ok(())
}

fn validate_relative(value: &str, label: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.is_empty() || path.is_absolute() || value.contains('\\')
        || value.bytes().any(|b| b == 0) || matches!(path.components().next(), Some(Component::Prefix(_))) {
        return Err(format!("{label} must be relative: '{value}'"));
    }
    Ok(())
}

fn owned_file(root: &Path, relative: &str, label: &str) -> Result<PathBuf, String> {
    validate_relative(relative, label)?;
    let canonical = canonical_regular_file(&root.join(relative), label)?;
    if !canonical.starts_with(root) {
        return Err(format!("{label} is outside project root {}: '{relative}'", root.display()));
    }
    if !matches!(canonical.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()).as_deref(),
        Some("json") | Some("wksc") | Some("ols") | Some("yaml") | Some("yml")) {
        return Err(format!("{label} must be a native JSON/YAML/.wksc model file; native manifests cannot be fragment includes: {}", canonical.display()));
    }
    Ok(canonical)
}

struct OwnedFile {
    bytes: Vec<u8>,
}

struct ModelFiles<'a> {
    root: &'a Path,
    active: Vec<PathBuf>,
    files: BTreeMap<String, OwnedFile>,
    sources: BTreeMap<String, String>,
}

impl<'a> ModelFiles<'a> {
    fn new(root: &'a Path) -> Self {
        Self { root, active: vec![], files: BTreeMap::new(), sources: BTreeMap::new() }
    }

    fn visit(&mut self, path: &Path) -> Result<Project, String> {
        if self.active.iter().any(|p| p == path) {
            return Err(format!("cyclic model include detected at {}", path.display()));
        }
        let relative = path.strip_prefix(self.root).map_err(|_| format!("model is outside project root: {}", path.display()))?
            .to_str().ok_or_else(|| format!("model path is not UTF-8: {}", path.display()))?.replace('\\', "/");
        if self.files.contains_key(&relative) {
            return Ok(Project::default());
        }
        self.active.push(path.to_path_buf());
        let result = self.visit_active(path, &relative);
        self.active.pop();
        result
    }

    fn visit_active(&mut self, path: &Path, relative: &str) -> Result<Project, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("cannot read model {}: {e}", path.display()))?;
        let mut project: Project = match path.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("json") | Some("wksc") => serde_json::from_slice(&bytes)
                .map_err(|e| format!("invalid model JSON {}: {e}", path.display()))?,
            Some("ols") | Some("yaml") | Some("yml") => serde_yaml::from_slice(&bytes)
                .map_err(|e| format!("invalid model YAML {}: {e}", path.display()))?,
            _ => return Err(format!("unsupported native model extension: {}", path.display())),
        };
        record_sources(&project, relative, &mut self.sources)?;
        // An included fragment's main never changes the native entrypoint.
        project.main = None;
        let includes = std::mem::take(&mut project.includes);
        let parent = path.parent().unwrap_or(self.root);
        for inc in includes {
            validate_relative(&inc, "model include")?;
            let inc_path = owned_file(self.root, &parent.join(&inc).strip_prefix(self.root)
                .map_err(|_| format!("model include is outside project root: '{inc}'"))?
                .to_string_lossy(), "model include")?;
            project.merge(self.visit(&inc_path)?);
        }
        self.files.insert(relative.to_string(), OwnedFile { bytes });
        Ok(project)
    }
}

fn record_sources(project: &Project, relative: &str, sources: &mut BTreeMap<String, String>) -> Result<(), String> {
    for pkg in &project.packages {
        let declarations = pkg.nodes.iter().map(|n| ("node", n.name.as_str()))
            .chain(pkg.types.iter().map(|t| ("type", t.name())))
            .chain(pkg.constants.iter().map(|c| ("constant", c.name.as_str())))
            .chain(pkg.state_machines.iter().map(|s| ("state_machine", s.name.as_str())))
            .chain(pkg.activations.iter().map(|a| ("activation", a.name.as_str())))
            .chain(pkg.contracts.iter().filter_map(|c| c.get("name").and_then(|n| n.as_str()).map(|n| ("contract", n))));
        for (kind, name) in declarations {
            let key = format!("{kind}:{name}");
            if let Some(old) = sources.insert(key, relative.to_string()) {
                return Err(format!("duplicate local {kind} '{name}' in {old} and {relative}"));
            }
        }
    }
    Ok(())
}

fn reject_external(project: &Project, path: &Path) -> Result<(), String> {
    if project.packages.iter().any(|p| !p.imported_operators.is_empty() || p.nodes.iter().any(|n| n.kind == NodeKind::Imported)) {
        return Err(format!("Imported nodes and imported operators are unsupported in native manifest v1 (known separate C ABI/check-exit defects): {}", path.display()));
    }
    Ok(())
}

fn lower_owned_constructs(project: &mut Project, sources: &mut BTreeMap<String, String>, model_relative: &str) -> Result<(), String> {
    let old_types: BTreeSet<String> = project.packages.iter().flat_map(|p| p.types.iter().map(|t| t.name().to_string())).collect();
    let machines: Vec<String> = project.packages.iter().flat_map(|p| p.state_machines.iter().map(|m| m.name.clone())).collect();
    project.lower_state_machines().map_err(|errors| format!("native state-machine lowering failed: {}", errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")))?;
    for machine in &machines {
        let source = sources.get(&format!("state_machine:{machine}")).cloned().unwrap_or_else(|| model_relative.to_string());
        if project.find_node(machine).is_some() {
            sources.entry(format!("node:{machine}")).or_insert_with(|| source.clone());
        }
        for ty in project.packages.iter().flat_map(|p| &p.types) {
            let name = ty.name();
            if !old_types.contains(name) && (name == format!("{machine}_StateEnum")
                || name.strip_prefix(&format!("{machine}_r")).and_then(|s| s.strip_suffix("_StateEnum")).is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))) {
                sources.insert(format!("type:{name}"), source.clone());
            }
        }
    }
    project.lower_activations().map_err(|errors| format!("native activation lowering failed: {}", errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")))?;
    Ok(())
}

fn snapshot(manifest: &NativeManifest, files: &BTreeMap<String, OwnedFile>) -> Result<String, String> {
    fn frame(buffer: &mut Vec<u8>, data: &[u8]) {
        buffer.extend_from_slice(&(data.len() as u64).to_be_bytes());
        buffer.extend_from_slice(data);
    }
    let canonical = serde_json::to_vec(manifest).map_err(|e| format!("cannot canonicalize manifest: {e}"))?;
    let mut bytes = b"OpenLustre native project snapshot v1\0".to_vec();
    frame(&mut bytes, &canonical);
    for (relative, file) in files {
        frame(&mut bytes, relative.as_bytes());
        frame(&mut bytes, &file.bytes);
    }
    Ok(sha256::digest(&bytes))
}

#[cfg(test)]
mod artifact_tests {
    use super::*;
    fn graph() -> NativeResolution {
        NativeResolution { root_project_id: "x".repeat(128), projects: vec![], symbols: ["x".repeat(128), format!("{}y", "x".repeat(127))].into_iter().map(|id| NativeSymbol {
            project_id: id.clone(), kind: "node".into(), local_name: "Root".into(),
            resolved_name: format!("olp_{}_n_526f6f74", id.bytes().map(|b| format!("{b:02x}")).collect::<String>()),
            source_path: "model.json".into(), exported: true, artifact_basename: None,
        }).collect() }
    }
    #[test]
    fn long_semantic_ids_have_stable_bounded_distinct_artifacts() {
        let mut resolution = graph();
        let original = resolution.symbols.iter().map(|s| s.resolved_name.clone()).collect::<Vec<_>>();
        assign_artifact_names(&mut resolution).unwrap();
        let names = resolution.symbols.iter().map(|s| s.artifact_basename.clone().unwrap()).collect::<Vec<_>>();
        assert_ne!(names[0], names[1]);
        assert!(names.iter().all(|name| name.len() == 76 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')));
        assert_eq!(resolution.symbols.iter().map(|s| s.resolved_name.clone()).collect::<Vec<_>>(), original);
        assign_artifact_names(&mut resolution).unwrap();
        assert_eq!(resolution.symbols[0].artifact_basename.as_ref(), Some(&names[0]));
    }
    #[test]
    fn an_artifact_digest_collision_is_rejected_instead_of_overwriting() {
        assert!(assign_artifact_names_with(&mut graph(), |_| "0".repeat(64)).unwrap_err().contains("basename collision"));
    }
}
