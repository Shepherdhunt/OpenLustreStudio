//! Docker + QEMU emulated-target run — a *third* equivalence backend beside the
//! IR simulator and the host-compiled C.
//!
//! The model's generated C-Lite is cross-compiled for **armhf** and run under
//! **QEMU user-mode** inside a Docker image, then its trace is checked against
//! `ol_sim` cell-for-cell. The host needs only Docker: the image installs the
//! ARM cross-toolchain and `qemu-user-static`, static-links the model, and runs
//! it under `qemu-arm-static`. The binary stays the CSV driver — a vector on
//! stdin, the per-cycle trace on stdout — so the same scenario drives all three
//! backends.
//!
//! Generation is self-contained (the emitted Docker context runs on any Docker
//! host); orchestration (`build_and_run`) is best-effort: where Docker is
//! present it builds, runs, and compares; otherwise the harness + the IR-sim
//! reference are emitted with instructions.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

/// The Docker image tag for an entry operator.
fn image_tag(entry: &str) -> String {
    format!("openlustre-emu-{}", entry.to_lowercase())
}

/// The Dockerfile: install the ARM cross-toolchain + qemu-user, static-link the
/// generated sources for armhf, and run the binary under `qemu-arm-static`.
pub(crate) fn dockerfile(entry: &str, sources: &[&str]) -> String {
    let tag = image_tag(entry);
    let mut copy: Vec<&str> = vec!["openlustre_generated.h"];
    if sources.iter().any(|s| *s == "openlustre_monitors.c") {
        copy.push("openlustre_monitors.h");
    }
    copy.extend_from_slice(sources);
    format!(
        "# OpenLustre Studio — emulated ARM (Linux) run harness for `{entry}`.
# Cross-compiles the generated C-Lite for armhf and runs it under QEMU user-mode,
# so any Docker host can exercise the model on an emulated target — no local
# cross-toolchain, no board. The binary is the CSV driver:
#   docker build -t {tag} .
#   docker run --rm -i {tag} < scenario.csv      # CSV vector in, trace out
FROM debian:stable-slim
RUN apt-get update \\
 && apt-get install -y --no-install-recommends gcc-arm-linux-gnueabihf qemu-user-static \\
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY {copy} ./
# Static link so QEMU user-mode needs no ARM sysroot at runtime.
RUN arm-linux-gnueabihf-gcc -std=c11 -O2 -static -o /app/{entry} {srcs} -lm
# driver.c reads a CSV input vector on stdin and prints the per-cycle trace.
ENTRYPOINT [\"qemu-arm-static\", \"/app/{entry}\"]
",
        entry = entry,
        tag = tag,
        copy = copy.join(" "),
        srcs = sources.join(" "),
    )
}

/// The README dropped in the Docker context.
pub(crate) fn readme(entry: &str) -> String {
    let tag = image_tag(entry);
    format!(
        "# Emulated ARM (Linux) run for `{entry}`

A self-contained Docker + QEMU harness: it cross-compiles the generated C-Lite
for **armhf** and runs it under **QEMU user-mode**, so any machine with Docker
can run the model on an emulated target — no local cross-toolchain.

## Run

```sh
docker build -t {tag} .
docker run --rm -i {tag} < scenario.csv > emulated_trace.csv
```

`scenario.csv` is a CSV input vector (header = the operator's inputs, one row
per cycle). The container prints the per-cycle trace in the same shape
`openlustre simulate` produces, so it diffs directly against the IR simulator —
a third equivalence backend beside the IR sim and the host-compiled C.
`openlustre clite-emulate <model> --scenario scenario.csv` runs this build and
comparison for you when Docker is on PATH.

## Files

- `openlustre_generated.{{h,c}}` — the model as portable C-Lite.
- `driver.c` — reads the CSV vector on stdin, prints the trace.
- `Dockerfile` — ARM cross-toolchain + `qemu-user-static`, static link, qemu run.
",
        entry = entry,
        tag = tag,
    )
}

/// Emit the C-Lite + Docker context for `entry` into `out_dir`. Returns the
/// file names written.
pub(crate) fn emit_harness(
    out_dir: &Path,
    project: &ol_ir::Project,
    entry_name: &str,
) -> Result<Vec<String>> {
    let sliced = project
        .slice_for_root(entry_name)
        .map_err(|e| anyhow::anyhow!("slicing `{entry_name}` for emulation: {e}"))?;
    let entry = sliced
        .find_node(entry_name)
        .with_context(|| format!("operator `{entry_name}` not found"))?
        .clone();
    std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;

    let bundle = ol_clite_emit::emit_project(&sliced);
    let has_contract = entry.contract.is_some();
    let driver = if has_contract {
        ol_clite_emit::harness::emit_csv_driver_with_monitor(&entry, entry.contract.as_deref())
    } else {
        ol_clite_emit::harness::emit_csv_driver(&entry)
    };

    let mut sources = vec!["openlustre_generated.c".to_string(), "driver.c".to_string()];
    let mut files: Vec<(String, String)> = vec![
        ("openlustre_generated.h".into(), bundle.header),
        ("openlustre_generated.c".into(), bundle.source),
        ("driver.c".into(), driver),
    ];
    if has_contract {
        let mon = ol_clite_emit::monitor::emit_monitors(&sliced);
        files.push(("openlustre_monitors.h".into(), mon.header));
        files.push(("openlustre_monitors.c".into(), mon.source));
        sources.push("openlustre_monitors.c".into());
    }
    let src_refs: Vec<&str> = sources.iter().map(|s| s.as_str()).collect();
    files.push(("Dockerfile".into(), dockerfile(entry_name, &src_refs)));
    files.push(("README.md".into(), readme(entry_name)));

    let mut written = Vec::new();
    for (name, text) in &files {
        std::fs::write(out_dir.join(name), text).with_context(|| format!("writing {name}"))?;
        written.push(name.clone());
    }
    Ok(written)
}

/// Is the Docker CLI on PATH and responsive?
pub(crate) fn docker_available() -> bool {
    Command::new("docker")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `docker build` the context in `out_dir`, then `docker run` it with
/// `scenario_csv` on stdin; returns the captured trace (stdout).
pub(crate) fn build_and_run(out_dir: &Path, entry: &str, scenario_csv: &str) -> Result<String> {
    use std::io::Write;
    let tag = image_tag(entry);

    let build = Command::new("docker")
        .args(["build", "-t", &tag, "."])
        .current_dir(out_dir)
        .status()
        .context("running `docker build` (is the Docker daemon running?)")?;
    if !build.success() {
        bail!("`docker build` failed (exit {:?})", build.code());
    }

    let mut run = Command::new("docker")
        .args(["run", "--rm", "-i", &tag])
        .current_dir(out_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning `docker run`")?;
    run.stdin
        .take()
        .expect("piped stdin")
        .write_all(scenario_csv.as_bytes())
        .context("piping the scenario into the container")?;
    let out = run.wait_with_output().context("waiting for `docker run`")?;
    if !out.status.success() {
        bail!(
            "`docker run` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Compare the emulated trace against the IR-sim trace cell-for-cell, on the
/// columns the C driver emits (cycle + outputs [+ monitor columns]). The IR
/// trace additionally carries inputs/locals, so we match by column *name* and
/// only require the emulated columns to agree — the dual-backend convention.
pub(crate) fn traces_match(expected_ir: &str, emulated: &str) -> std::result::Result<(), String> {
    fn parse(t: &str) -> (Vec<String>, Vec<Vec<String>>) {
        let mut lines = t.lines().filter(|l| !l.trim().is_empty());
        let header = lines
            .next()
            .unwrap_or("")
            .split(',')
            .map(|s| s.trim().to_string())
            .collect();
        let rows = lines
            .map(|l| l.split(',').map(|s| s.trim().to_string()).collect())
            .collect();
        (header, rows)
    }
    let (eh, er) = parse(expected_ir);
    let (ah, ar) = parse(emulated);
    if ah.is_empty() || ah == [""] {
        return Err("emulated trace is empty (the container produced no output)".into());
    }
    // Map each emulated column to the IR column of the same name.
    let mut pairs: Vec<(usize, usize, String)> = Vec::new();
    for (ai, col) in ah.iter().enumerate() {
        match eh.iter().position(|h| h == col) {
            Some(ei) => pairs.push((ai, ei, col.clone())),
            None => return Err(format!("emulated trace has column `{col}` absent from the IR-sim trace")),
        }
    }
    if er.len() != ar.len() {
        return Err(format!(
            "row count differs: IR sim {} cycles vs emulated {}",
            er.len(),
            ar.len()
        ));
    }
    for (cycle, (erow, arow)) in er.iter().zip(ar.iter()).enumerate() {
        for (ai, ei, col) in &pairs {
            let a = arow.get(*ai).map(String::as_str).unwrap_or("");
            let e = erow.get(*ei).map(String::as_str).unwrap_or("");
            if a != e {
                return Err(format!(
                    "cycle {cycle}, column `{col}`: IR sim `{e}` vs emulated `{a}`"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dockerfile_cross_compiles_and_runs_under_qemu() {
        let d = dockerfile("Doubler", &["openlustre_generated.c", "driver.c"]);
        assert!(d.contains("gcc-arm-linux-gnueabihf"), "installs the cross-toolchain");
        assert!(d.contains("qemu-user-static"), "installs qemu-user");
        assert!(d.contains("arm-linux-gnueabihf-gcc -std=c11 -O2 -static -o /app/Doubler"), "static cross-link:\n{d}");
        assert!(d.contains("ENTRYPOINT [\"qemu-arm-static\", \"/app/Doubler\"]"), "runs under qemu:\n{d}");
        assert!(d.contains("COPY openlustre_generated.h openlustre_generated.c driver.c ./"));
    }

    #[test]
    fn dockerfile_includes_monitors_when_present() {
        let d = dockerfile("Wd", &["openlustre_generated.c", "driver.c", "openlustre_monitors.c"]);
        assert!(d.contains("openlustre_monitors.h"), "copies the monitor header");
        assert!(d.contains("openlustre_monitors.c"));
    }

    #[test]
    fn readme_documents_the_build_and_run() {
        let r = readme("Doubler");
        assert!(r.contains("docker build -t openlustre-emu-doubler"));
        assert!(r.contains("docker run --rm -i openlustre-emu-doubler < scenario.csv"));
    }

    #[test]
    fn traces_match_on_shared_columns() {
        // IR trace carries an extra input column `x`; the emulated trace is a
        // subset (cycle + output `y`) and must still match.
        let ir = "cycle,x,y\n0,3,6\n1,4,8\n";
        let emu = "cycle,y\n0,6\n1,8\n";
        assert!(traces_match(ir, emu).is_ok());
    }

    #[test]
    fn traces_mismatch_is_located() {
        let ir = "cycle,y\n0,6\n1,8\n";
        let emu = "cycle,y\n0,6\n1,9\n";
        let err = traces_match(ir, emu).unwrap_err();
        assert!(err.contains("cycle 1"), "{err}");
        assert!(err.contains("`8`") && err.contains("`9`"), "{err}");
    }

    #[test]
    fn traces_reject_unknown_column_and_row_mismatch() {
        assert!(traces_match("cycle,y\n0,6\n", "cycle,z\n0,6\n").is_err());
        assert!(traces_match("cycle,y\n0,6\n1,8\n", "cycle,y\n0,6\n").unwrap_err().contains("row count"));
        assert!(traces_match("cycle,y\n0,6\n", "").is_err());
    }
}
