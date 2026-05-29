//! OpenLustre Studio: standard block library loader (Phase 9 / plan Task 15).
//!
//! The built-in block library is authored as compact YAML files under
//! `libraries/`. Each file holds one block or a list of blocks in the schema
//! described by [`block::LibBlock`]. This crate parses those files, lowers them
//! to strict IR via [`parser`], assembles them into an [`ol_ir::Project`], and
//! runs the same type and contract checks the CLI applies to user models — so a
//! malformed library block fails loudly instead of silently shipping.

pub mod block;
pub mod parser;

use std::path::{Path, PathBuf};

use ol_contract_ir::ContractDef;
use ol_ir::{Diagnostic, NodeDef, Package, Project};

pub use block::{LibBlock, LoweredBlock, LowerError};
pub use parser::{parse_expr, parse_type, ParseError};

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("I/O error reading {0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("YAML parse error in {0}: {1}")]
    Yaml(PathBuf, #[source] serde_yaml::Error),
    #[error("{0}")]
    Lower(#[from] LowerError),
}

/// A loaded block plus the file it came from.
pub struct LibraryEntry {
    pub source: PathBuf,
    pub block: LoweredBlock,
}

/// All blocks loaded from a library tree, ready to be assembled or inspected.
#[derive(Default)]
pub struct Library {
    pub entries: Vec<LibraryEntry>,
}

impl Library {
    pub fn nodes(&self) -> impl Iterator<Item = &NodeDef> {
        self.entries.iter().map(|e| &e.block.node)
    }

    pub fn contracts(&self) -> impl Iterator<Item = &ContractDef> {
        self.entries
            .iter()
            .filter_map(|e| e.block.contract.as_ref())
    }

    /// Assemble every loaded block into a single-package project named
    /// `package_name`. Contracts are serialized to the raw JSON form the IR
    /// stores them in.
    pub fn to_project(&self, package_name: &str) -> Project {
        let nodes: Vec<NodeDef> = self.nodes().cloned().collect();
        let contracts: Vec<serde_json::Value> = self
            .contracts()
            .map(|c| serde_json::to_value(c).expect("ContractDef serializes"))
            .collect();
        let pkg = Package {
            name: package_name.to_string(),
            nodes,
            contracts,
            ..Default::default()
        };
        Project {
            name: package_name.to_string(),
            packages: vec![pkg],
            main: None,
        }
    }

    /// Type-check and contract-check the assembled library, returning all
    /// diagnostics from both passes.
    pub fn check(&self) -> Vec<Diagnostic> {
        let project = self.to_project("stdlib");
        let mut diags = ol_typecheck::check_project(&project).diagnostics;
        diags.extend(ol_contract_check::check_project(&project).diagnostics);
        diags
    }

    /// Append every library block and contract into `project` as an additional
    /// package named `package_name`. Node names that already exist in the
    /// project are skipped, so user definitions always win over the stdlib.
    pub fn merge_into(&self, project: &mut Project, package_name: &str) {
        let existing: std::collections::HashSet<String> = project
            .all_nodes()
            .map(|n| n.name.clone())
            .collect();
        let nodes: Vec<NodeDef> = self
            .nodes()
            .filter(|n| !existing.contains(&n.name))
            .cloned()
            .collect();
        let kept_contract_names: std::collections::HashSet<String> = nodes
            .iter()
            .filter_map(|n| n.contract.clone())
            .collect();
        let contracts: Vec<serde_json::Value> = self
            .contracts()
            .filter(|c| kept_contract_names.contains(&c.name))
            .map(|c| serde_json::to_value(c).expect("ContractDef serializes"))
            .collect();
        project.packages.push(Package {
            name: package_name.to_string(),
            nodes,
            contracts,
            ..Default::default()
        });
    }
}

/// Parse one library YAML document into one or more blocks. A document may be a
/// single block mapping or a sequence of block mappings.
pub fn parse_blocks(yaml: &str) -> Result<Vec<LibBlock>, serde_yaml::Error> {
    let value: serde_yaml::Value = serde_yaml::from_str(yaml)?;
    if value.is_sequence() {
        serde_yaml::from_value(value)
    } else {
        Ok(vec![serde_yaml::from_value(value)?])
    }
}

/// Load and lower every block in a single file.
pub fn load_file(path: &Path) -> Result<Vec<LibraryEntry>, LoadError> {
    let text = std::fs::read_to_string(path).map_err(|e| LoadError::Io(path.to_path_buf(), e))?;
    let raw = parse_blocks(&text).map_err(|e| LoadError::Yaml(path.to_path_buf(), e))?;
    let mut out = Vec::new();
    for b in raw {
        let lowered = b.lower()?;
        out.push(LibraryEntry {
            source: path.to_path_buf(),
            block: lowered,
        });
    }
    Ok(out)
}

/// Recursively load every `.yaml`/`.yml` file under `dir` into a [`Library`].
/// Files are visited in sorted order so the assembled project is deterministic.
pub fn load_dir(dir: &Path) -> Result<Library, LoadError> {
    let mut files = Vec::new();
    collect_yaml(dir, &mut files)?;
    files.sort();
    let mut lib = Library::default();
    for f in files {
        lib.entries.extend(load_file(&f)?);
    }
    Ok(lib)
}

fn collect_yaml(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), LoadError> {
    let rd = std::fs::read_dir(dir).map_err(|e| LoadError::Io(dir.to_path_buf(), e))?;
    for entry in rd {
        let entry = entry.map_err(|e| LoadError::Io(dir.to_path_buf(), e))?;
        let path = entry.path();
        if path.is_dir() {
            collect_yaml(&path, out)?;
        } else if matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("yaml") | Some("yml")
        ) {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const AND_OR: &str = r#"
- name: And
  kind: function
  category: logic
  inputs:  [{ name: a, type: bool }, { name: b, type: bool }]
  outputs: [{ name: y, type: bool }]
  equations: [{ lhs: [y], body: "a and b" }]
  contract:
    guarantees: ["y = (a and b)"]
"#;

    const RISING: &str = r#"
name: RisingEdge
kind: operator
category: temporal
inputs:  [{ name: x, type: bool }]
outputs: [{ name: edge, type: bool }]
equations:
  - lhs: [edge]
    body: "x and not (false -> pre x)"
contract:
  modes:
    - { name: Rising, requires: ["x and not pre_x"], ensures: ["edge"] }
"#;

    #[test]
    fn parses_block_list() {
        let blocks = parse_blocks(AND_OR).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].name, "And");
    }

    #[test]
    fn parses_single_block_document() {
        let blocks = parse_blocks(RISING).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].name, "RisingEdge");
    }

    #[test]
    fn lowers_and_typechecks() {
        let mut lib = Library::default();
        for b in parse_blocks(AND_OR).unwrap() {
            lib.entries.push(LibraryEntry {
                source: PathBuf::from("inline"),
                block: b.lower().unwrap(),
            });
        }
        for b in parse_blocks(RISING).unwrap() {
            lib.entries.push(LibraryEntry {
                source: PathBuf::from("inline"),
                block: b.lower().unwrap(),
            });
        }
        let errors: Vec<_> = lib
            .check()
            .into_iter()
            .filter(|d| matches!(d.severity, ol_ir::Severity::Error))
            .collect();
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn rising_edge_lowers_to_arrow_pre() {
        let block = parse_blocks(RISING).unwrap().pop().unwrap();
        let lowered = block.lower().unwrap();
        let eq = &lowered.node.equations[0];
        assert!(eq.rhs.contains_temporal());
        assert_eq!(lowered.node.kind, ol_ir::NodeKind::Operator);
    }
}
