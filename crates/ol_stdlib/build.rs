//! Embed the standard block library into the compiled crate so a deployed
//! `openlustre` binary carries its full 41-block palette with no on-disk
//! `libraries/` checkout. Generates `$OUT_DIR/embedded_libraries.rs` with a
//! `(relative_path, contents)` table that `ol_stdlib::load_embedded` parses
//! through the exact same loader as `load_dir`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let libraries = manifest.join("../../libraries");
    let libraries = libraries.canonicalize().unwrap_or(libraries);
    println!("cargo:rerun-if-changed={}", libraries.display());

    let mut files: Vec<PathBuf> = Vec::new();
    collect_yaml(&libraries, &mut files);
    files.sort();

    let mut out = String::new();
    let _ = writeln!(
        out,
        "/// `(relative_path, yaml_contents)` for every standard-library file,\n\
         /// embedded at compile time."
    );
    let _ = writeln!(
        out,
        "pub static EMBEDDED_LIBRARIES: &[(&str, &str)] = &["
    );
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
        let rel = f
            .strip_prefix(&libraries)
            .unwrap_or(f)
            .to_string_lossy()
            .replace('\\', "/");
        let _ = writeln!(out, "    ({rel:?}, include_str!({:?})),", f.display().to_string());
    }
    let _ = writeln!(out, "];");

    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("embedded_libraries.rs");
    std::fs::write(dest, out).unwrap();
}

fn collect_yaml(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_yaml(&p, out);
        } else if matches!(
            p.extension().and_then(|s| s.to_str()),
            Some("yaml") | Some("yml")
        ) {
            out.push(p);
        }
    }
}
