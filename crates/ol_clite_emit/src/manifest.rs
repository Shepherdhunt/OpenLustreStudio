//! Imported C operator manifest (Phase 5, plan Task 14).
//!
//! Each imported operator is described by a YAML manifest stored alongside the
//! model. The manifest declares the C symbol, types, and a contract — the IDE
//! treats it just like any other operator after loading.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use ol_ir::Type;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedPort {
    pub name: String,
    #[serde(rename = "type")]
    pub ty_str: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ImportedContract {
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub guarantees: Vec<String>,
    #[serde(default)]
    pub properties: ImportedProperties,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ImportedProperties {
    #[serde(default)]
    pub pure: bool,
    #[serde(default)]
    pub deterministic: bool,
    #[serde(default)]
    pub no_dynamic_memory: bool,
    #[serde(default)]
    pub no_global_write: bool,
    #[serde(default)]
    pub bounded_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedOperator {
    pub name: String,
    #[serde(default = "default_imported_kind")]
    pub kind: String,
    pub language: String,
    pub header: String,
    pub source: String,
    pub symbol: String,
    pub inputs: Vec<ImportedPort>,
    pub outputs: Vec<ImportedPort>,
    #[serde(default)]
    pub contract: ImportedContract,
}

fn default_imported_kind() -> String {
    "imported_operator".into()
}

impl ImportedOperator {
    pub fn input_types(&self) -> Vec<Type> {
        self.inputs.iter().map(|p| parse_type(&p.ty_str)).collect()
    }
    pub fn output_types(&self) -> Vec<Type> {
        self.outputs.iter().map(|p| parse_type(&p.ty_str)).collect()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.symbol.is_empty() {
            return Err(format!("imported operator `{}` has no C symbol", self.name));
        }
        if !self.contract.properties.pure {
            return Err(format!(
                "imported operator `{}` is not declared pure; OpenLustre requires purity",
                self.name
            ));
        }
        if !self.contract.properties.no_dynamic_memory {
            return Err(format!(
                "imported operator `{}` is not declared free of dynamic memory",
                self.name
            ));
        }
        Ok(())
    }
}

/// Parse a textual type from the manifest. Supports the subset declared in
/// the implementation plan plus fixed arrays like `uint8[32]`.
pub fn parse_type(s: &str) -> Type {
    let s = s.trim();
    if let Some(open) = s.find('[') {
        let elem_s = &s[..open];
        if let Some(close) = s.find(']') {
            let len: u32 = s[open + 1..close].parse().unwrap_or(0);
            return Type::Array {
                elem: Box::new(parse_type(elem_s)),
                len,
            };
        }
    }
    match s {
        "bool" => Type::Bool,
        "int8" => Type::Int8,
        "int16" => Type::Int16,
        "int32" => Type::Int32,
        "int64" => Type::Int64,
        "uint8" => Type::Uint8,
        "uint16" => Type::Uint16,
        "uint32" => Type::Uint32,
        "uint64" => Type::Uint64,
        "float32" => Type::Float32,
        "float64" => Type::Float64,
        other => Type::Named(other.into()),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("I/O error reading manifest {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("YAML parse error in manifest {0}: {1}")]
    Yaml(PathBuf, serde_yaml::Error),
}

pub fn load_manifest(path: &Path) -> Result<ImportedOperator, ManifestError> {
    let data = std::fs::read_to_string(path).map_err(|e| ManifestError::Io(path.into(), e))?;
    serde_yaml::from_str(&data).map_err(|e| ManifestError::Yaml(path.into(), e))
}

pub fn load_manifest_dir(dir: &Path) -> Vec<(PathBuf, Result<ImportedOperator, ManifestError>)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("yaml")
            || p.extension().and_then(|s| s.to_str()) == Some("yml")
        {
            let m = load_manifest(&p);
            out.push((p, m));
        }
    }
    out
}
