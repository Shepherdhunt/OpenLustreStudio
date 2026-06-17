//! `openlustre` — the OpenLustre Studio command-line driver.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

mod lustre_import;
mod scenario;
mod studio_server;

use ol_clite_emit::{load_manifest_dir, monitor};
use ol_cocospec_emit::Target;
use ol_kind2::{Kind2Options, SerMode};

#[derive(Parser, Debug)]
#[command(
    name = "openlustre",
    version,
    about = "OpenLustre Studio CLI — strict Lustre/CoCoSpec workbench"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Type-check and contract-check a model.
    Check {
        model: PathBuf,
        /// Also load imported-operator manifests from this directory.
        #[arg(long)]
        imports: Option<PathBuf>,
        /// Fold the standard block library at this path into the project.
        #[arg(long, value_name = "DIR")]
        with_stdlib: Option<PathBuf>,
    },
    /// Emit Lustre + CoCoSpec to a directory.
    EmitLustre {
        model: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        /// Use legacy `(*@contract ... @*)` syntax instead of modern `con/noc`.
        #[arg(long)]
        legacy: bool,
        #[arg(long, value_name = "DIR")]
        with_stdlib: Option<PathBuf>,
        /// Generate only this operator and everything it transitively uses
        /// (SCADE-style selected-root generation) instead of the whole project.
        #[arg(long, value_name = "NODE")]
        root: Option<String>,
    },
    /// Emit Directional C-Lite + contract monitors to a directory.
    EmitClite {
        model: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long, value_name = "DIR")]
        with_stdlib: Option<PathBuf>,
        /// Also emit a CSV driver that drives the named (or main) node, so
        /// `cc *.c -o trace_driver` produces an executable that reads inputs
        /// on stdin in the same shape as `openlustre simulate`.
        #[arg(long)]
        driver: bool,
        /// Generate C wrappers for imported-operator manifests in this
        /// directory, plus a build manifest listing external sources to link.
        #[arg(long, value_name = "DIR")]
        imports: Option<PathBuf>,
        /// Generate only this operator and everything it transitively uses
        /// (SCADE-style selected-root generation) instead of the whole project.
        #[arg(long, value_name = "NODE")]
        root: Option<String>,
    },
    /// Run the IR simulator against a CSV input vector.
    Simulate {
        model: PathBuf,
        #[arg(long)]
        node: Option<String>,
        #[arg(long)]
        inputs: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, value_name = "DIR")]
        with_stdlib: Option<PathBuf>,
    },
    /// Invoke Kind 2 against the generated Lustre.
    Prove {
        model: PathBuf,
        #[arg(long)]
        node: Option<String>,
        #[arg(long, value_enum, default_value_t = ProveMode::BmcInd)]
        mode: ProveMode,
        /// Path to the kind2 binary; defaults to `kind2` on PATH.
        #[arg(long, default_value = "kind2")]
        kind2: String,
        /// Directory to keep generated artifacts in.
        #[arg(long)]
        workdir: Option<PathBuf>,
        #[arg(long, value_name = "DIR")]
        with_stdlib: Option<PathBuf>,
        /// Wall-clock seconds before Kind 2 gives up on a property.
        #[arg(long, value_name = "SECS")]
        timeout: Option<u32>,
        /// Restrict the prover to these named properties. Repeat the flag
        /// or pass a comma-separated list.
        #[arg(long, value_name = "NAME", value_delimiter = ',')]
        property: Vec<String>,
        /// For each falsifiable property, render its counterexample as a
        /// per-cycle waveform table instead of raw JSON.
        #[arg(long)]
        waveform: bool,
    },
    /// Contract-check only.
    ContractCheck {
        model: PathBuf,
        #[arg(long, value_name = "DIR")]
        with_stdlib: Option<PathBuf>,
    },
    /// Load the standard block library and type/contract-check every block.
    LibCheck {
        /// Directory of library YAML files (e.g. `libraries`).
        dir: PathBuf,
    },
    /// GUI / IDE integration commands. Emit stable JSON describing the loaded
    /// project so a future Tauri / web / VS Code front end can drive every
    /// headless tool through a single language-agnostic IPC surface.
    Studio {
        #[command(subcommand)]
        cmd: StudioCmd,
    },
    /// Project test scenarios: record golden traces from the IR simulator,
    /// then re-run them against the IR simulator AND the compiled generated C
    /// to prove the model and the auto-generated code behave identically.
    Test {
        #[command(subcommand)]
        cmd: TestCmd,
    },
    /// Create a new workspace folder: project.json, types.json (named type
    /// definitions), and scenarios/. Open it with
    /// `openlustre studio launch <dir>`.
    New {
        /// Workspace directory (created if missing).
        dir: PathBuf,
        /// Start with no operators or functions (an empty project). Without
        /// this, a small starter operator is created.
        #[arg(long)]
        empty: bool,
    },
}

#[derive(Subcommand, Debug)]
enum TestCmd {
    /// Capture (or refresh) golden traces for every scenario input vector.
    Record {
        model: PathBuf,
        /// Directory of scenario input vectors (*.csv). Goldens are written
        /// alongside as *.golden.csv.
        #[arg(long, default_value = "scenarios")]
        scenarios: PathBuf,
        #[arg(long, value_name = "DIR")]
        with_stdlib: Option<PathBuf>,
        /// Node to drive; defaults to the project's `main`.
        #[arg(long)]
        node: Option<String>,
    },
    /// Run every scenario and compare against the goldens.
    Run {
        model: PathBuf,
        #[arg(long, default_value = "scenarios")]
        scenarios: PathBuf,
        #[arg(long, value_name = "DIR")]
        with_stdlib: Option<PathBuf>,
        #[arg(long)]
        node: Option<String>,
        /// Which backends to verify: the IR simulator, the compiled
        /// generated C, or both (default).
        #[arg(long, value_enum, default_value_t = TestBackend::Both)]
        backend: TestBackend,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum TestBackend {
    Ir,
    C,
    Both,
}

#[derive(Subcommand, Debug)]
enum StudioCmd {
    /// Print a structured JSON inspection of the model — packages, nodes,
    /// types, constants, contract summaries, and diagnostics. Stable schema
    /// versioned via the top-level `schema_version` field.
    Inspect {
        model: PathBuf,
        #[arg(long, value_name = "DIR")]
        with_stdlib: Option<PathBuf>,
        /// Pretty-print the JSON for human inspection. Without this the output
        /// is compact, the form a programmatic consumer wants.
        #[arg(long)]
        pretty: bool,
    },
    /// Serve the Studio web UI on http://127.0.0.1:<port>. The page hits the
    /// same `inspect` / `lustre` / `clite` / `simulate` JSON endpoints the
    /// rest of the CLI exposes; the model is re-loaded on every request so
    /// external edits are picked up by a page refresh.
    Serve {
        model: PathBuf,
        #[arg(long, default_value_t = 8181)]
        port: u16,
        /// Use this on-disk block library instead of the one embedded in the
        /// binary. Without this flag the embedded 41-block palette is used.
        #[arg(long, value_name = "DIR")]
        with_stdlib: Option<PathBuf>,
        /// Serve the model alone, without any standard-library palette.
        #[arg(long)]
        no_stdlib: bool,
        /// Directory of test scenarios for the Tests tab. Defaults to a
        /// `scenarios` directory next to the model file.
        #[arg(long, value_name = "DIR")]
        scenarios: Option<PathBuf>,
    },
    /// Desktop entry point: start the Studio and open it in the default
    /// browser. With no model argument, opens (creating on first run) a
    /// starter project in `~/OpenLustre/welcome.json`. This is the command
    /// installer shortcuts run.
    Launch {
        /// Model to open; defaults to ~/OpenLustre/welcome.json.
        model: Option<PathBuf>,
        /// Port to serve on; 0 picks a free port.
        #[arg(long, default_value_t = 0)]
        port: u16,
        #[arg(long, value_name = "DIR")]
        with_stdlib: Option<PathBuf>,
        #[arg(long)]
        no_stdlib: bool,
        /// Print the URL without opening a browser (CI / headless use).
        #[arg(long)]
        no_open: bool,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum ProveMode {
    BmcInd,
    Realizability,
    ModeCoverage,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Check {
            model,
            imports,
            with_stdlib,
        } => cmd_check(&model, imports.as_deref(), with_stdlib.as_deref()),
        Cmd::EmitLustre {
            model,
            out,
            legacy,
            with_stdlib,
            root,
        } => cmd_emit_lustre(&model, &out, legacy, with_stdlib.as_deref(), root.as_deref()),
        Cmd::EmitClite {
            model,
            out,
            with_stdlib,
            driver,
            imports,
            root,
        } => cmd_emit_clite(
            &model,
            &out,
            with_stdlib.as_deref(),
            driver,
            imports.as_deref(),
            root.as_deref(),
        ),
        Cmd::Simulate {
            model,
            node,
            inputs,
            out,
            with_stdlib,
        } => cmd_simulate(
            &model,
            node.as_deref(),
            &inputs,
            out.as_deref(),
            with_stdlib.as_deref(),
        ),
        Cmd::Prove {
            model,
            node,
            mode,
            kind2,
            workdir,
            with_stdlib,
            timeout,
            property,
            waveform,
        } => cmd_prove(
            &model,
            node.as_deref(),
            mode,
            &kind2,
            workdir.as_deref(),
            with_stdlib.as_deref(),
            timeout,
            &property,
            waveform,
        ),
        Cmd::ContractCheck { model, with_stdlib } => {
            cmd_contract_check(&model, with_stdlib.as_deref())
        }
        Cmd::LibCheck { dir } => cmd_lib_check(&dir),
        Cmd::Studio { cmd } => match cmd {
            StudioCmd::Inspect {
                model,
                with_stdlib,
                pretty,
            } => cmd_studio_inspect(&model, with_stdlib.as_deref(), pretty),
            StudioCmd::Serve {
                model,
                port,
                with_stdlib,
                no_stdlib,
                scenarios,
            } => serve_studio(&model, port, with_stdlib, no_stdlib, scenarios, false),
            StudioCmd::Launch {
                model,
                port,
                with_stdlib,
                no_stdlib,
                no_open,
            } => cmd_studio_launch(model, port, with_stdlib, no_stdlib, no_open),
        },
        Cmd::New { dir, empty } => {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("creating {}", dir.display()))?;
            let project = resolve_workspace(&dir, empty)?;
            println!(
                "new: workspace ready at {} ({})",
                dir.display(),
                if empty { "empty — add operators in the Studio" } else { "with a starter operator" }
            );
            println!("new: project file {}", project.display());
            println!("new: open it with `openlustre studio launch {}`", dir.display());
            Ok(())
        }
        Cmd::Test { cmd } => match cmd {
            TestCmd::Record {
                model,
                scenarios,
                with_stdlib,
                node,
            } => cmd_test_record(&model, &scenarios, with_stdlib.as_deref(), node.as_deref()),
            TestCmd::Run {
                model,
                scenarios,
                with_stdlib,
                node,
                backend,
            } => cmd_test_run(
                &model,
                &scenarios,
                with_stdlib.as_deref(),
                node.as_deref(),
                backend,
            ),
        },
    }
}

fn load(model: &Path) -> Result<ol_ir::Project> {
    ol_ir::load_project(model).with_context(|| format!("loading model {}", model.display()))
}

pub(crate) fn load_with_stdlib(model: &Path, stdlib: Option<&Path>) -> Result<ol_ir::Project> {
    let mut project = load(model)?;
    if let Some(dir) = stdlib {
        let lib = ol_stdlib::load_dir(dir)
            .with_context(|| format!("loading stdlib from {}", dir.display()))?;
        let errors: Vec<String> = lib
            .check()
            .into_iter()
            .filter(|d| matches!(d.severity, ol_ir::Severity::Error))
            .map(|d| d.render())
            .collect();
        if !errors.is_empty() {
            anyhow::bail!("stdlib failed validation:\n{}", errors.join("\n"));
        }
        lib.merge_into(&mut project, "stdlib");
    }
    // Lower any state machines to dataflow before downstream tools see the
    // project, so they can treat the lowered nodes as ordinary operators.
    if let Err(errs) = project.lower_state_machines() {
        let joined = errs
            .into_iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::bail!("state-machine lowering failed:\n{joined}");
    }
    Ok(project)
}

fn cmd_check(
    model: &Path,
    imports: Option<&Path>,
    with_stdlib: Option<&Path>,
) -> Result<()> {
    let project = load_with_stdlib(model, with_stdlib)?;
    let report = ol_typecheck::check_project(&project);
    for d in &report.diagnostics {
        println!("{}", d.render());
    }
    let creport = ol_contract_check::check_project(&project);
    for d in &creport.diagnostics {
        println!("{}", d.render());
    }

    if let Some(dir) = imports {
        for (p, m) in load_manifest_dir(dir) {
            match m {
                Ok(op) => match op.validate() {
                    Ok(()) => println!("info[I0001]: imported operator `{}` OK ({})", op.name, p.display()),
                    Err(e) => println!("error[I0002]: imported operator `{}`: {e}", op.name),
                },
                Err(e) => println!("error[I0003]: {e}"),
            }
        }
    }

    let errors = report.has_errors() || creport.has_errors();
    if errors {
        anyhow::bail!("check failed");
    }
    println!("check: OK ({} nodes)", project.all_nodes().count());
    Ok(())
}

fn cmd_contract_check(model: &Path, with_stdlib: Option<&Path>) -> Result<()> {
    let project = load_with_stdlib(model, with_stdlib)?;
    let creport = ol_contract_check::check_project(&project);
    for d in &creport.diagnostics {
        println!("{}", d.render());
    }
    if creport.has_errors() {
        anyhow::bail!("contract-check failed");
    }
    println!("contract-check: OK ({} contracts)", creport.contracts.len());
    Ok(())
}

fn cmd_lib_check(dir: &Path) -> Result<()> {
    let lib = ol_stdlib::load_dir(dir)
        .with_context(|| format!("loading library from {}", dir.display()))?;
    let diags = lib.check();
    for d in &diags {
        println!("{}", d.render());
    }
    let errors = diags
        .iter()
        .filter(|d| matches!(d.severity, ol_ir::Severity::Error))
        .count();
    if errors > 0 {
        anyhow::bail!("lib-check failed: {errors} error(s)");
    }
    println!(
        "lib-check: OK ({} blocks, {} contracts)",
        lib.entries.len(),
        lib.contracts().count()
    );
    Ok(())
}

/// Emit a stable JSON inspection of the loaded project — what a GUI Project
/// Explorer + diagnostics panel needs in one round-trip. The schema is
/// versioned via the top-level `schema_version` field so future additions
/// stay backward-compatible.
fn cmd_studio_inspect(
    model: &Path,
    with_stdlib: Option<&Path>,
    pretty: bool,
) -> Result<()> {
    let project = load_with_stdlib(model, with_stdlib)?;
    let typecheck = ol_typecheck::check_project(&project);
    let contract = ol_contract_check::check_project(&project);

    let mut diagnostics: Vec<serde_json::Value> = Vec::new();
    for d in &typecheck.diagnostics {
        diagnostics.push(diag_to_json(d, "typecheck"));
    }
    for d in &contract.diagnostics {
        diagnostics.push(diag_to_json(d, "contract"));
    }

    let packages: Vec<serde_json::Value> = project
        .packages
        .iter()
        .map(package_to_json)
        .collect();

    let report = serde_json::json!({
        "schema_version": 1,
        "tool": "openlustre studio inspect",
        "project": {
            "name": project.name,
            "main": project.main,
            "package_count": project.packages.len(),
            "node_count": project.all_nodes().count(),
            "packages": packages,
        },
        "diagnostics": diagnostics,
        "summary": {
            "errors": diagnostics.iter()
                .filter(|d| d.get("severity").and_then(|s| s.as_str()) == Some("Error"))
                .count(),
            "warnings": diagnostics.iter()
                .filter(|d| d.get("severity").and_then(|s| s.as_str()) == Some("Warning"))
                .count(),
        }
    });

    let text = if pretty {
        serde_json::to_string_pretty(&report)?
    } else {
        serde_json::to_string(&report)?
    };
    println!("{text}");
    Ok(())
}

pub(crate) fn diag_to_json(d: &ol_ir::Diagnostic, source: &str) -> serde_json::Value {
    let severity = match d.severity {
        ol_ir::Severity::Error => "Error",
        ol_ir::Severity::Warning => "Warning",
        ol_ir::Severity::Info => "Info",
    };
    serde_json::json!({
        "severity": severity,
        "code": d.code,
        "message": d.message,
        "context": d.context,
        "source": source,
    })
}

pub(crate) fn package_to_json(pkg: &ol_ir::Package) -> serde_json::Value {
    let nodes: Vec<serde_json::Value> = pkg
        .nodes
        .iter()
        .map(|n| {
            let kind = match n.kind {
                ol_ir::NodeKind::Function => "Function",
                ol_ir::NodeKind::Operator => "Operator",
                ol_ir::NodeKind::Imported => "Imported",
            };
            serde_json::json!({
                "name": n.name,
                "kind": kind,
                "inputs": n.inputs.iter().map(port_to_json).collect::<Vec<_>>(),
                "outputs": n.outputs.iter().map(port_to_json).collect::<Vec<_>>(),
                "locals": n.locals.iter().map(|l| serde_json::json!({
                    "name": l.name,
                    "type": l.ty,
                })).collect::<Vec<_>>(),
                "equation_count": n.equations.len(),
                "contract": n.contract,
            })
        })
        .collect();
    let (contracts, _) = ol_contract_ir::parse_contracts(&pkg.contracts);
    let contract_summary: Vec<serde_json::Value> = contracts
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "assumption_count": c.assumptions.len(),
                "guarantee_count": c.guarantees.len(),
                "mode_count": c.modes.len(),
                "modes": c.modes.iter().map(|m| &m.name).collect::<Vec<_>>(),
                "import_count": c.imports.len(),
            })
        })
        .collect();
    serde_json::json!({
        "name": pkg.name,
        "types": pkg.types.iter().map(|t| serde_json::json!({
            "name": t.name(),
            "body": t.body,
        })).collect::<Vec<_>>(),
        "constants": pkg.constants.iter().map(|c| serde_json::json!({
            "name": c.name,
            "type": c.ty,
            "value": ol_lustre_emit::format_expr(&c.value),
        })).collect::<Vec<_>>(),
        "nodes": nodes,
        "contracts": contract_summary,
        "state_machine_count": pkg.state_machines.len(),
    })
}

fn port_to_json(p: &ol_ir::Port) -> serde_json::Value {
    serde_json::json!({
        "name": p.name,
        "type": p.ty,
    })
}

fn cmd_emit_lustre(
    model: &Path,
    out: &Path,
    legacy: bool,
    with_stdlib: Option<&Path>,
    root: Option<&str>,
) -> Result<()> {
    let mut project = load_with_stdlib(model, with_stdlib)?;
    if let Some(root) = root {
        project = project
            .slice_for_root(root)
            .map_err(|e| anyhow::anyhow!(e))?;
        println!(
            "emit-lustre: selected root `{root}` — generating {} used node(s)",
            project.all_nodes().count()
        );
    }
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let lus = ol_lustre_emit::emit_project(&project);
    std::fs::write(out.join("model.lus"), &lus)?;
    let target = if legacy { Target::Legacy } else { Target::Modern };
    let con = ol_cocospec_emit::emit_project(&project, target);
    std::fs::write(out.join("contracts.lus"), &con)?;
    println!(
        "emit-lustre: wrote {} and {}",
        out.join("model.lus").display(),
        out.join("contracts.lus").display()
    );
    Ok(())
}

fn cmd_emit_clite(
    model: &Path,
    out: &Path,
    with_stdlib: Option<&Path>,
    driver: bool,
    imports: Option<&Path>,
    root: Option<&str>,
) -> Result<()> {
    let mut project = load_with_stdlib(model, with_stdlib)?;
    if let Some(root) = root {
        project = project
            .slice_for_root(root)
            .map_err(|e| anyhow::anyhow!(e))?;
        println!(
            "emit-clite: selected root `{root}` — generating {} used node(s)",
            project.all_nodes().count()
        );
    }
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let clite_dir = out.join("clite");
    let mon_dir = out.join("monitors");
    std::fs::create_dir_all(&clite_dir)?;
    std::fs::create_dir_all(&mon_dir)?;
    let bundle = ol_clite_emit::emit_project(&project);
    std::fs::write(clite_dir.join("openlustre_generated.h"), bundle.header)?;
    std::fs::write(clite_dir.join("openlustre_generated.c"), bundle.source)?;

    let mon = monitor::emit_monitors(&project);
    std::fs::write(mon_dir.join("openlustre_monitors.h"), mon.header)?;
    std::fs::write(mon_dir.join("openlustre_monitors.c"), mon.source)?;

    if driver {
        let entry_name = project
            .main
            .clone()
            .context("--driver requires the project to declare a `main` node")?;
        let entry = project
            .find_node(&entry_name)
            .with_context(|| format!("no node named `{entry_name}`"))?;
        let driver_src = ol_clite_emit::harness::emit_csv_driver(entry);
        std::fs::write(clite_dir.join("driver.c"), driver_src)?;
        // A Makefile so the generated tree builds with one command. This is
        // the "user-defined main operator becomes the entry point of the
        // standalone executable" OpenLustre-vs-SCADE differentiator made
        // concrete: the project's `main:` field names the operator, and
        // the Makefile produces a binary named after it.
        std::fs::write(clite_dir.join("Makefile"), makefile_for_entry(&entry_name))?;
    }

    let mut wrapper_count = 0usize;
    if let Some(dir) = imports {
        let imp_dir = out.join("imported");
        std::fs::create_dir_all(&imp_dir)?;
        let mut build_lines: Vec<String> = Vec::new();
        for (p, m) in load_manifest_dir(dir) {
            let op = match m {
                Ok(op) => op,
                Err(e) => {
                    println!("error[I0003]: {e}");
                    continue;
                }
            };
            if let Err(e) = op.validate() {
                anyhow::bail!("imported operator `{}` ({}): {e}", op.name, p.display());
            }
            let w = ol_clite_emit::emit_wrapper(&op);
            std::fs::write(imp_dir.join(format!("{}_wrapper.h", op.name)), &w.header)?;
            std::fs::write(imp_dir.join(&w.build.wrapper_source), &w.source)?;
            build_lines.push(format!(
                "# {name}: link {ext} + {wrap} (header: {hdr})\n{wrap}\n{ext}",
                name = op.name,
                ext = w.build.external_source,
                wrap = w.build.wrapper_source,
                hdr = w.build.external_header,
            ));
            wrapper_count += 1;
        }
        let build_manifest = format!(
            "# OpenLustre imported-operator build manifest.\n\
             # Compile each wrapper alongside its external C source, with the\n\
             # imported manifest directory on the include path.\n\n{}\n",
            build_lines.join("\n\n")
        );
        std::fs::write(imp_dir.join("BUILD.txt"), build_manifest)?;
    }

    println!(
        "emit-clite: wrote {} (sources){}{} and {} (monitors)",
        clite_dir.display(),
        if driver { " + driver.c" } else { "" },
        if wrapper_count > 0 {
            format!(" + {wrapper_count} imported wrapper(s)")
        } else {
            String::new()
        },
        mon_dir.display()
    );
    Ok(())
}

fn cmd_simulate(
    model: &Path,
    node: Option<&str>,
    inputs: &Path,
    out: Option<&Path>,
    with_stdlib: Option<&Path>,
) -> Result<()> {
    let project = load_with_stdlib(model, with_stdlib)?;
    let node_name = node
        .map(|s| s.to_string())
        .or_else(|| project.main.clone())
        .context("no --node specified and project has no `main`")?;
    let mut sim = ol_sim::Sim::new(&project, &node_name)?;
    let csv = std::fs::read_to_string(inputs)?;
    let trace = sim.run_csv(&csv)?;
    let csv_out = trace.to_csv();
    match out {
        Some(p) => {
            std::fs::write(p, &csv_out)?;
            println!("simulate: wrote {}", p.display());
        }
        None => {
            print!("{csv_out}");
        }
    }
    Ok(())
}

fn cmd_prove(
    model: &Path,
    node: Option<&str>,
    mode: ProveMode,
    kind2: &str,
    workdir: Option<&Path>,
    with_stdlib: Option<&Path>,
    timeout: Option<u32>,
    properties: &[String],
    waveform: bool,
) -> Result<()> {
    let project = load_with_stdlib(model, with_stdlib)?;
    let work = match workdir {
        Some(p) => p.to_path_buf(),
        None => std::env::temp_dir().join("openlustre_prove"),
    };
    std::fs::create_dir_all(&work)?;
    let lus = ol_lustre_emit::emit_project(&project);
    let con = ol_cocospec_emit::emit_project(&project, Target::Modern);
    let combined = format!("{lus}\n{con}");
    let lus_path = work.join("model_with_contracts.lus");
    std::fs::write(&lus_path, &combined)?;
    let opts = Kind2Options {
        kind2_binary: kind2.to_string(),
        mode: match mode {
            ProveMode::BmcInd => SerMode::BmcInd,
            ProveMode::Realizability => SerMode::Realizability,
            ProveMode::ModeCoverage => SerMode::ModeCoverage,
        },
        main_node: node.map(|s| s.to_string()).or_else(|| project.main.clone()),
        extra_args: vec![],
        timeout_seconds: timeout,
        properties: properties.to_vec(),
    };
    let result = ol_kind2::run_kind2(&lus_path, &opts)?;
    println!("prove: invoked {}", result.invocation.join(" "));
    println!("exit code: {}", result.exit_code);
    if !result.stderr.is_empty() {
        eprintln!("stderr:\n{}", result.stderr);
    }
    if result.properties.is_empty() {
        println!("(no parseable property results — raw stdout follows)");
        println!("{}", result.stdout);
    } else {
        for p in &result.properties {
            println!("  {}: {}", p.name, p.status);
            if let Some(cex) = &p.counterexample {
                if waveform {
                    if let Some(table) = ol_kind2::render_counterexample_waveform(cex) {
                        println!("    counterexample:");
                        for line in table.lines() {
                            println!("      {line}");
                        }
                        continue;
                    }
                }
                println!(
                    "    counterexample (raw): {}",
                    serde_json::to_string(cex).unwrap_or_default()
                );
            }
        }
    }
    Ok(())
}

/// Load a project the way the Studio does: an explicit `--with-stdlib DIR`
/// wins; otherwise the library embedded in this binary is merged (the
/// deployed-app default) unless stdlib use is disabled entirely.
pub(crate) fn load_for_studio(
    model: &Path,
    with_stdlib: Option<&Path>,
    use_embedded: bool,
) -> Result<ol_ir::Project> {
    if with_stdlib.is_some() {
        return load_with_stdlib(model, with_stdlib);
    }
    let mut project = load(model)?;
    if use_embedded {
        let lib = ol_stdlib::load_embedded()
            .map_err(|e| anyhow::anyhow!("embedded stdlib: {e}"))?;
        lib.merge_into(&mut project, "stdlib");
    }
    if let Err(errs) = project.lower_state_machines() {
        let joined = errs
            .into_iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::bail!("state-machine lowering failed:\n{joined}");
    }
    Ok(project)
}

fn serve_studio(
    model: &Path,
    port: u16,
    with_stdlib: Option<PathBuf>,
    no_stdlib: bool,
    scenarios: Option<PathBuf>,
    open_browser: bool,
) -> Result<()> {
    // Opening a directory opens it as a workspace: `<dir>/project.json`
    // (created on first open along with types.json and scenarios/). Serving a
    // not-yet-created workspace seeds the starter operator so a double-clicked
    // shortcut always opens something runnable; `openlustre new --empty` is the
    // path to a blank project.
    let model = &resolve_workspace(model, false)?;
    let use_embedded = with_stdlib.is_none() && !no_stdlib;
    // Eagerly load once to surface configuration errors before starting the
    // server — better to fail fast than to spin up a UI that only ever shows
    // errors.
    let _ = load_for_studio(model, with_stdlib.as_deref(), use_embedded)
        .with_context(|| format!("loading model {}", model.display()))?;

    let scenarios = scenarios.unwrap_or_else(|| {
        model
            .parent()
            .unwrap_or(Path::new("."))
            .join("scenarios")
    });

    let listener = studio_server::bind(studio_server::loopback(port))
        .with_context(|| format!("binding 127.0.0.1:{port}"))?;
    let local = listener.local_addr()?;
    let url = format!("http://{local}");
    println!("studio: serving {url} (model: {})", model.display());
    println!("studio: ctrl-c to stop.");
    if open_browser {
        open_in_browser(&url);
    }
    // A `types.json` next to the model is the workspace types file: named
    // type definitions created in the GUI are saved there.
    let types_file = model
        .parent()
        .map(|p| p.join("types.json"))
        .filter(|p| p.exists() && Some(p.as_path()) != Some(model));

    let ctx = studio_server::ServerCtx {
        model: model.to_path_buf(),
        with_stdlib,
        use_embedded,
        scenarios,
        types_file,
        history: Default::default(),
    };
    studio_server::serve(listener, ctx)?;
    Ok(())
}

/// Resolve a model argument that may be a workspace directory. Opening a
/// directory creates the project skeleton on first open — `project.json`
/// (with a starter operator, including `types.json`), the types file, and a
/// `scenarios/` directory — and resolves to the project file.
fn resolve_workspace(path: &Path, empty: bool) -> Result<PathBuf> {
    if !path.is_dir() {
        return Ok(path.to_path_buf());
    }
    let types_path = path.join("types.json");
    if !types_path.exists() {
        let types_doc = ol_ir::Project {
            name: "types".into(),
            packages: vec![ol_ir::Package {
                name: "user".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        std::fs::write(&types_path, serde_json::to_string_pretty(&types_doc)?)
            .with_context(|| format!("writing {}", types_path.display()))?;
    }
    std::fs::create_dir_all(path.join("scenarios"))
        .with_context(|| format!("creating {}", path.join("scenarios").display()))?;

    // The workspace file ties together every operator/function in the project.
    // Prefer an existing `.wksc`; fall back to a legacy `project.json`;
    // otherwise create a new `<dirname>.wksc`.
    if let Some(wksc) = first_wksc(path) {
        return Ok(wksc);
    }
    let legacy = path.join("project.json");
    if legacy.exists() {
        return Ok(legacy);
    }
    let dirname = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("workspace")
        .to_string();
    let wksc = path.join(format!("{dirname}.wksc"));
    let mut project = if empty { empty_project() } else { starter_project() };
    project.name = dirname;
    project.includes = vec!["types.json".into()];
    std::fs::write(&wksc, serde_json::to_string_pretty(&project)?)
        .with_context(|| format!("writing {}", wksc.display()))?;
    println!("studio: created workspace {}", wksc.display());
    Ok(wksc)
}

/// The first `*.wksc` workspace file directly inside `dir` (lexicographic), if
/// any — so opening a workspace folder finds its `.wksc`.
fn first_wksc(dir: &Path) -> Option<PathBuf> {
    let mut hits: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("wksc"))
                    .unwrap_or(false)
        })
        .collect();
    hits.sort();
    hits.into_iter().next()
}

fn cmd_studio_launch(
    model: Option<PathBuf>,
    port: u16,
    with_stdlib: Option<PathBuf>,
    no_stdlib: bool,
    no_open: bool,
) -> Result<()> {
    let model = match model {
        Some(m) => m,
        None => welcome_project_path()?,
    };
    serve_studio(&model, port, with_stdlib, no_stdlib, None, !no_open)
}

/// `~/OpenLustre/welcome.json`, created from a starter model on first run so
/// double-clicking the installed shortcut always opens something editable.
fn welcome_project_path() -> Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .context("could not determine the home directory (HOME / USERPROFILE)")?;
    let dir = home.join("OpenLustre");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    // Back-compat: keep using an existing welcome.json; otherwise the default
    // workspace is a `welcome.wksc` file.
    let legacy = dir.join("welcome.json");
    if legacy.exists() {
        return Ok(legacy);
    }
    let path = dir.join("welcome.wksc");
    if !path.exists() {
        let project = starter_project();
        std::fs::write(&path, serde_json::to_string_pretty(&project)?)
            .with_context(|| format!("writing {}", path.display()))?;
        println!("studio: created starter workspace at {}", path.display());
    }
    Ok(path)
}

/// An empty project: one `user` package, no operators, no `main`. The Studio
/// opens it as a blank canvas — add operators from Insert ▸ Operator or by
/// right-clicking in the workspace tree.
fn empty_project() -> ol_ir::Project {
    ol_ir::Project {
        packages: vec![ol_ir::Package {
            name: "user".into(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// A small self-contained model: `Heartbeat(enable) -> beat` toggles every
/// cycle while enabled. Uses `->`/`pre`, if-logic, and is set as `main`, so
/// the Step / Build / Tests tabs all work out of the box.
fn starter_project() -> ol_ir::Project {
    use ol_ir::{Equation, Expr, NodeDef, NodeKind, Package, Port, Project, Type};
    let node = NodeDef {
        name: "Heartbeat".into(),
        kind: NodeKind::Operator,
        inputs: vec![Port { name: "enable".into(), ty: Type::Bool }],
        outputs: vec![Port { name: "beat".into(), ty: Type::Bool }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["beat".into()],
            rhs: Expr::and(
                Expr::var("enable"),
                Expr::arrow(Expr::bool_lit(true), Expr::not(Expr::pre(Expr::var("beat")))),
            ),
        }],
        contract: None,
        diagram: Default::default(),
        probes: vec![],
    };
    Project {
        name: "welcome".into(),
        packages: vec![Package {
            name: "user".into(),
            nodes: vec![node],
            ..Default::default()
        }],
        main: Some("Heartbeat".into()),
        ..Default::default()
    }
}

/// Best-effort: open `url` in the platform's default browser. Failure is
/// non-fatal — the URL is already printed.
fn open_in_browser(url: &str) {
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    if result.is_err() {
        eprintln!("studio: could not open a browser automatically; open {url} manually");
    }
}

/// Render a single-command Makefile that builds the standalone executable
/// for a project. The user-defined main operator names both the entry-point
/// `_step` function the driver calls and the produced binary; this is the
/// concrete shape of the OpenLustre-vs-SCADE differentiator — any operator
/// in the project can be designated as `main:` and become the entry point.
pub(crate) fn makefile_for_entry(entry: &str) -> String {
    format!(
"# Generated by OpenLustre Studio.
# The user-defined main operator `{entry}` is the entry point of this build.
# Run `make` to produce `{entry}`; the executable reads inputs from stdin
# (CSV with one header row matching the operator's inputs) and prints the
# per-cycle trace on stdout in the same shape `openlustre simulate` writes.

TARGET ?= {entry}
CC ?= cc
CFLAGS ?= -std=c11 -Wall -Wextra -O2

SOURCES = openlustre_generated.c driver.c

$(TARGET): $(SOURCES)
\t$(CC) $(CFLAGS) -o $@ $(SOURCES)

run: $(TARGET)
\t./$(TARGET)

clean:
\trm -f $(TARGET) *.o

.PHONY: run clean
"
    )
}

fn cmd_test_record(
    model: &Path,
    scenarios: &Path,
    with_stdlib: Option<&Path>,
    node: Option<&str>,
) -> Result<()> {
    let project = load_with_stdlib(model, with_stdlib)?;
    let node_name = node
        .map(|s| s.to_string())
        .or_else(|| project.main.clone())
        .context("no --node specified and project has no `main`")?;
    let recorded = scenario::record_goldens(&project, scenarios, &node_name)
        .map_err(|e| anyhow::anyhow!(e))?;
    for (name, path) in &recorded {
        println!("recorded golden for `{name}` -> {}", path.display());
    }
    println!("test record: {} golden trace(s) captured", recorded.len());
    Ok(())
}

fn cmd_test_run(
    model: &Path,
    scenarios: &Path,
    with_stdlib: Option<&Path>,
    node: Option<&str>,
    backend: TestBackend,
) -> Result<()> {
    let project = load_with_stdlib(model, with_stdlib)?;
    let node_name = node
        .map(|s| s.to_string())
        .or_else(|| project.main.clone())
        .context("no --node specified and project has no `main`")?;
    let backends: Vec<scenario::Backend> = match backend {
        TestBackend::Ir => vec![scenario::Backend::Ir],
        TestBackend::C => vec![scenario::Backend::C],
        TestBackend::Both => vec![scenario::Backend::Ir, scenario::Backend::C],
    };
    let outcome = scenario::run_scenarios(&project, scenarios, &node_name, &backends);
    if outcome.results.is_empty() {
        anyhow::bail!(
            "no scenarios found in {} (expected *.csv input vectors)",
            scenarios.display()
        );
    }
    print!("{}", scenario::render_report(&outcome.results));
    if let Some(cov) = &outcome.coverage {
        println!(
            "decision coverage: {}/{} if-conditions driven both ways",
            cov.covered, cov.total
        );
        for u in &cov.uncovered {
            println!(
                "  uncovered: {}::{} `{}` (missing {})",
                u.node, u.context, u.condition, u.missing
            );
        }
    }
    if let Some(m) = &outcome.mcdc {
        println!(
            "MC/DC: {}/{} conditions independent ({}/{} decisions fully covered)",
            m.covered_conditions, m.total_conditions, m.covered_decisions, m.total_decisions
        );
        for u in &m.uncovered {
            println!(
                "  uncovered: {}::{} `{}` in `{}` ({})",
                u.node, u.context, u.condition, u.decision, u.reason
            );
        }
    }
    if !scenario::all_green(&outcome.results) {
        anyhow::bail!("test run failed");
    }
    Ok(())
}
