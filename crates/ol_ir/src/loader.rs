use std::collections::HashSet;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::project::Project;

#[derive(Debug, Error)]
pub enum LoadError {
    #[error("I/O error reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("YAML parse error in {path}: {source}")]
    Yaml {
        path: String,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("JSON parse error in {path}: {source}")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("Unsupported model file extension for {path}; expected .ols, .yaml, .yml, or .json")]
    UnsupportedExtension { path: String },
    #[error("cyclic include detected at {0}")]
    CyclicInclude(String),
    #[error("native project resolution failed: {0}")]
    Native(String),
}

/// Load a `Project` from disk.
///
/// * `.ols` / `.yaml` / `.yml` parse as YAML; `.json` parses as JSON.
/// * If `path` is a directory, every supported file directly inside it is
///   loaded and merged into one project (sorted by name for deterministic
///   merge order). The directory's name is used as the project's `name`.
/// * Each loaded project may declare an `includes:` list of relative paths;
///   those are loaded recursively and merged. Self-references and cycles
///   produce [`LoadError::CyclicInclude`] rather than diverging.
/// * A canonical file is merged only once per load. Independent projects
///   may share transitive dependencies without duplicating their definitions.
pub fn load_project(path: &Path) -> Result<Project, LoadError> {
    if is_native_manifest(path) {
        return crate::native_project::load_native_project(path).map_err(LoadError::Native);
    }
    load_recursive(path, &mut LoadState::default())
}

fn is_native_manifest(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case("olproj"))
}

#[derive(Default)]
struct LoadState {
    active: HashSet<PathBuf>,
    loaded: HashSet<PathBuf>,
}

fn load_recursive(path: &Path, state: &mut LoadState) -> Result<Project, LoadError> {
    if is_native_manifest(path) {
        return Err(LoadError::Native("legacy includes cannot import .olproj manifests; use explicit native dependencies".into()));
    }
    let canonical = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf());
    if state.active.contains(&canonical) {
        return Err(LoadError::CyclicInclude(path.display().to_string()));
    }
    if state.loaded.contains(&canonical) {
        return Ok(Project::default());
    }
    state.active.insert(canonical.clone());
    let result = if path.is_dir() {
        load_directory(path, state)
    } else {
        load_file_and_includes(path, state)
    };
    state.active.remove(&canonical);
    if result.is_ok() {
        state.loaded.insert(canonical);
    }
    result
}

fn load_directory(dir: &Path, state: &mut LoadState) -> Result<Project, LoadError> {
    let rd = std::fs::read_dir(dir).map_err(|e| LoadError::Io {
        path: dir.display().to_string(),
        source: e,
    })?;
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in rd {
        let entry = entry.map_err(|e| LoadError::Io {
            path: dir.display().to_string(),
            source: e,
        })?;
        let p = entry.path();
        if p.is_file() && is_native_manifest(&p) {
            return Err(LoadError::Native("directory merge cannot consume a native project; select its .olproj manifest explicitly".into()));
        }
        if p.is_file()
            && matches!(
                p.extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_ascii_lowercase())
                    .as_deref(),
                Some("ols") | Some("yaml") | Some("yml") | Some("json") | Some("wksc")
            )
        {
            files.push(p);
        }
    }
    files.sort();
    let mut merged = Project {
        name: dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("project")
            .to_string(),
        ..Default::default()
    };
    for f in files {
        let child = load_recursive(&f, state)?;
        merged.merge(child);
    }
    Ok(merged)
}

fn load_file_and_includes(
    path: &Path,
    state: &mut LoadState,
) -> Result<Project, LoadError> {
    let mut project = parse_single_file(path)?;
    let parent = path.parent().unwrap_or(Path::new("."));
    let includes = std::mem::take(&mut project.includes);
    for inc in includes {
        let inc_path = if Path::new(&inc).is_absolute() {
            PathBuf::from(&inc)
        } else {
            parent.join(&inc)
        };
        let child = load_recursive(&inc_path, state)?;
        project.merge(child);
    }
    Ok(project)
}

fn parse_single_file(path: &Path) -> Result<Project, LoadError> {
    let data = std::fs::read_to_string(path).map_err(|e| LoadError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    match path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("ols") | Some("yaml") | Some("yml") => {
            serde_yaml::from_str(&data).map_err(|e| LoadError::Yaml {
                path: path.display().to_string(),
                source: e,
            })
        }
        // `.wksc` is the workspace file — JSON content, same `Project` schema.
        Some("json") | Some("wksc") => serde_json::from_str(&data).map_err(|e| LoadError::Json {
            path: path.display().to_string(),
            source: e,
        }),
        _ => Err(LoadError::UnsupportedExtension {
            path: path.display().to_string(),
        }),
    }
}
