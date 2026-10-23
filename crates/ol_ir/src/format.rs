//! The model file format's version.
//!
//! Every model file — a workspace `.wksc`, a `project.json`, `types.json`,
//! an included file, a YAML `.ols` — starts with `format_version`. A build
//! reads every format up to its own [`FORMAT_VERSION`]: an older file is
//! upgraded in memory, one [`Migration`] at a time, and a newer one is
//! refused rather than misread (a field it does not know would otherwise be
//! dropped on the next save). Files written before the format had a version
//! (OpenLustre Studio 0.1.0) are format 1. Files are always written in the
//! current format, and [`backup_before_upgrade`] keeps a copy of a file
//! before the save that upgrades it.
//!
//! Changing the format: bump [`FORMAT_VERSION`], add the migration from the
//! previous version to [`MIGRATIONS`], and add a file in the previous format
//! under `tests/fixtures/format/` (see `docs/model-format.md`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::project::Project;

/// The model file format this build reads and writes.
pub const FORMAT_VERSION: u32 = 1;

/// The format of a file that names none (written before formats had
/// versions).
pub const UNVERSIONED: u32 = 1;

/// Upgrades a model file, held as JSON, from format `from` to `from + 1`.
pub struct Migration {
    pub from: u32,
    /// What changed, for the release notes and error messages.
    pub summary: &'static str,
    pub apply: fn(&mut serde_json::Value) -> Result<(), String>,
}

/// Every migration, oldest first: none while there is only format 1.
pub const MIGRATIONS: &[Migration] = &[];

/// A [`Project`] is always in the current format: this field serializes as
/// `"format_version": FORMAT_VERSION` — first, as the project's first field —
/// so every file written says which format it is in. Reading ignores the
/// value; [`parse`] checks it before the project is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CurrentFormat;

impl Serialize for CurrentFormat {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u32(FORMAT_VERSION)
    }
}

impl<'de> Deserialize<'de> for CurrentFormat {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(d)?;
        Ok(CurrentFormat)
    }
}

#[derive(Debug, Error)]
pub enum FormatError {
    #[error(
        "written by a newer OpenLustre Studio (model format {found}); this one reads model formats \
         up to {supported} — update OpenLustre Studio to open it"
    )]
    Newer { found: u32, supported: u32 },
    #[error("`format_version` must be a whole number from 1 up, not {0}")]
    Invalid(String),
    #[error("cannot upgrade from model format {from}: {message}")]
    Migration { from: u32, message: String },
}

/// Why a model file's text could not become a project.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("{0}")]
    Format(#[from] FormatError),
}

/// How a model file is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    Json,
    Yaml,
}

impl Syntax {
    /// By extension: `.json` / `.wksc` are JSON, `.ols` / `.yaml` / `.yml`
    /// YAML.
    pub fn of(path: &Path) -> Option<Syntax> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "json" | "wksc" => Some(Syntax::Json),
            "ols" | "yaml" | "yml" => Some(Syntax::Yaml),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct Header {
    #[serde(default)]
    format_version: Option<serde_json::Value>,
}

/// The format a model file's text says it is in.
pub fn declared_version(text: &str, syntax: Syntax) -> Result<u32, ParseError> {
    let header: Header = match syntax {
        Syntax::Json => serde_json::from_str(text)?,
        Syntax::Yaml => serde_yaml::from_str(text)?,
    };
    Ok(version_from(header.format_version)?)
}

fn version_from(v: Option<serde_json::Value>) -> Result<u32, FormatError> {
    match v {
        None => Ok(UNVERSIONED),
        Some(v) => v
            .as_u64()
            .filter(|n| *n >= 1)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| FormatError::Invalid(v.to_string())),
    }
}

/// A model file's text as a project: its format checked, upgraded when
/// older.
pub fn parse(text: &str, syntax: Syntax) -> Result<Project, ParseError> {
    parse_with(text, syntax, FORMAT_VERSION, MIGRATIONS)
}

fn parse_with(text: &str, syntax: Syntax, current: u32, migrations: &[Migration]) -> Result<Project, ParseError> {
    let version = declared_version(text, syntax)?;
    if version > current {
        return Err(FormatError::Newer { found: version, supported: current }.into());
    }
    if version == current {
        // Straight from the text, so a mistake is reported at its line.
        return Ok(match syntax {
            Syntax::Json => serde_json::from_str(text)?,
            Syntax::Yaml => serde_yaml::from_str(text)?,
        });
    }
    let mut doc: serde_json::Value = match syntax {
        Syntax::Json => serde_json::from_str(text)?,
        Syntax::Yaml => serde_yaml::from_str(text)?,
    };
    upgrade(&mut doc, version, current, migrations)?;
    Ok(serde_json::from_value(doc)?)
}

/// Apply the migrations from format `from` up to `to`.
fn upgrade(doc: &mut serde_json::Value, from: u32, to: u32, migrations: &[Migration]) -> Result<(), FormatError> {
    for v in from..to {
        let m = migrations.iter().find(|m| m.from == v).ok_or_else(|| FormatError::Migration {
            from: v,
            message: "this build has no migration from it".into(),
        })?;
        (m.apply)(doc).map_err(|message| FormatError::Migration { from: v, message })?;
    }
    if let Some(map) = doc.as_object_mut() {
        map.insert("format_version".into(), to.into());
    }
    Ok(())
}

/// Before a project is saved over `path`: if the file there is in an older
/// format, keep a copy as `<file>.format<N>.bak` (once), since the save
/// rewrites it in the current one. Returns the copy's path when one was made.
pub fn backup_before_upgrade(path: &Path) -> std::io::Result<Option<PathBuf>> {
    backup_with(path, FORMAT_VERSION)
}

fn backup_with(path: &Path, current: u32) -> std::io::Result<Option<PathBuf>> {
    let (Some(syntax), Ok(text)) = (Syntax::of(path), std::fs::read_to_string(path)) else {
        return Ok(None);
    };
    let Ok(version) = declared_version(&text, syntax) else {
        return Ok(None);
    };
    if version >= current {
        return Ok(None);
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("model");
    let backup = path.with_file_name(format!("{name}.format{version}.bak"));
    if backup.exists() {
        return Ok(None);
    }
    std::fs::write(&backup, text)?;
    Ok(Some(backup))
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNVERSIONED_JSON: &str = r#"{"name": "m", "packages": [{"name": "user"}], "main": "Top"}"#;

    /// A pretend format 2 that renamed `main` to `entry`, to exercise the
    /// upgrade path before a real one exists.
    fn rename_main(doc: &mut serde_json::Value) -> Result<(), String> {
        let map = doc.as_object_mut().ok_or("not an object")?;
        if let Some(main) = map.remove("main") {
            map.insert("entry".into(), main);
        }
        Ok(())
    }

    fn entry_back(doc: &mut serde_json::Value) -> Result<(), String> {
        let map = doc.as_object_mut().ok_or("not an object")?;
        if let Some(e) = map.remove("entry") {
            map.insert("main".into(), e);
        }
        Ok(())
    }

    #[test]
    fn a_file_without_a_version_is_format_1_and_reads_as_before() {
        assert_eq!(declared_version(UNVERSIONED_JSON, Syntax::Json).unwrap(), 1);
        let p = parse(UNVERSIONED_JSON, Syntax::Json).unwrap();
        assert_eq!(p.main.as_deref(), Some("Top"));
        let yaml = "name: m\npackages:\n  - name: user\nmain: Top\n";
        assert_eq!(parse(yaml, Syntax::Yaml).unwrap().main.as_deref(), Some("Top"));
    }

    #[test]
    fn every_file_written_starts_with_the_current_format() {
        let p = parse(UNVERSIONED_JSON, Syntax::Json).unwrap();
        let json = serde_json::to_string_pretty(&p).unwrap();
        assert!(json.starts_with(&format!("{{\n  \"format_version\": {FORMAT_VERSION},\n  \"name\"")), "{json}");
        let yaml = serde_yaml::to_string(&Project::default()).unwrap();
        assert!(yaml.starts_with(&format!("format_version: {FORMAT_VERSION}\n")), "{yaml}");
        // And reads back.
        assert_eq!(parse(&json, Syntax::Json).unwrap(), p);
        assert_eq!(declared_version(&yaml, Syntax::Yaml).unwrap(), FORMAT_VERSION);
    }

    #[test]
    fn a_newer_format_is_refused_not_misread() {
        let newer = format!(r#"{{"format_version": {}, "name": "m", "future_field": 1}}"#, FORMAT_VERSION + 1);
        let e = parse(&newer, Syntax::Json).unwrap_err().to_string();
        assert!(e.contains("newer OpenLustre Studio") && e.contains("update OpenLustre Studio"), "{e}");
        for bad in ["0", "-1", "1.5", "\"1\""] {
            let text = format!(r#"{{"format_version": {bad}, "name": "m"}}"#);
            let e = parse(&text, Syntax::Json).unwrap_err().to_string();
            assert!(e.contains("format_version"), "{bad}: {e}");
        }
    }

    #[test]
    fn syntax_errors_keep_their_line() {
        let e = parse("{\n  \"name\": \"m\",\n  \"packages\": [,]\n}", Syntax::Json).unwrap_err().to_string();
        assert!(e.contains("line 3"), "{e}");
    }

    #[test]
    fn an_older_format_is_upgraded_one_migration_at_a_time() {
        // Pretend the current format is 3: 1 → 2 renames `main` to `entry`,
        // 2 → 3 renames it back, so the result must have `main` again.
        let steps = [
            Migration { from: 1, summary: "main → entry", apply: rename_main },
            Migration { from: 2, summary: "entry → main", apply: entry_back },
        ];
        let p = parse_with(UNVERSIONED_JSON, Syntax::Json, 3, &steps).unwrap();
        assert_eq!(p.main.as_deref(), Some("Top"));
        // A missing step, or one that fails, is a clear error.
        let e = parse_with(UNVERSIONED_JSON, Syntax::Json, 3, &steps[..1]).unwrap_err().to_string();
        assert!(e.contains("from model format 2"), "{e}");
        let failing = [Migration { from: 1, summary: "fails", apply: |_| Err("broken file".into()) }];
        let e = parse_with(UNVERSIONED_JSON, Syntax::Json, 2, &failing).unwrap_err().to_string();
        assert!(e.contains("from model format 1: broken file"), "{e}");
    }

    #[test]
    fn the_save_that_upgrades_a_file_keeps_the_old_one_once() {
        let dir = std::env::temp_dir().join(format!("ol-format-backup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let model = dir.join("m.wksc");
        std::fs::write(&model, UNVERSIONED_JSON).unwrap();
        // In the current format: nothing to keep.
        assert_eq!(backup_with(&model, 1).unwrap(), None);
        // If this build wrote format 2, the format-1 file is kept, once.
        let kept = backup_with(&model, 2).unwrap().expect("a backup");
        assert_eq!(kept, dir.join("m.wksc.format1.bak"));
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), UNVERSIONED_JSON);
        std::fs::write(&model, "{\"name\": \"m\", \"packages\": []}").unwrap();
        assert_eq!(backup_with(&model, 2).unwrap(), None, "the first copy is kept");
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), UNVERSIONED_JSON);
        // No file yet, or not a model file: nothing to do.
        assert_eq!(backup_with(&dir.join("new.wksc"), 2).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
