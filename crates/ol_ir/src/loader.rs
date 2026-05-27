use std::path::Path;

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
}

/// Load a `Project` from disk. `.ols` and `.yaml`/`.yml` are treated as YAML;
/// `.json` is treated as JSON.
pub fn load_project(path: &Path) -> Result<Project, LoadError> {
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
        Some("json") => serde_json::from_str(&data).map_err(|e| LoadError::Json {
            path: path.display().to_string(),
            source: e,
        }),
        _ => Err(LoadError::UnsupportedExtension {
            path: path.display().to_string(),
        }),
    }
}
