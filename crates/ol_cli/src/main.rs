//! `openlustre` — the OpenLustre Studio command-line driver.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

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
    },
    /// Emit Directional C-Lite + contract monitors to a directory.
    EmitClite {
        model: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long, value_name = "DIR")]
        with_stdlib: Option<PathBuf>,
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
        } => cmd_emit_lustre(&model, &out, legacy, with_stdlib.as_deref()),
        Cmd::EmitClite {
            model,
            out,
            with_stdlib,
        } => cmd_emit_clite(&model, &out, with_stdlib.as_deref()),
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
        } => cmd_prove(
            &model,
            node.as_deref(),
            mode,
            &kind2,
            workdir.as_deref(),
            with_stdlib.as_deref(),
        ),
        Cmd::ContractCheck { model, with_stdlib } => {
            cmd_contract_check(&model, with_stdlib.as_deref())
        }
        Cmd::LibCheck { dir } => cmd_lib_check(&dir),
    }
}

fn load(model: &Path) -> Result<ol_ir::Project> {
    ol_ir::load_project(model).with_context(|| format!("loading model {}", model.display()))
}

fn load_with_stdlib(model: &Path, stdlib: Option<&Path>) -> Result<ol_ir::Project> {
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

fn cmd_emit_lustre(
    model: &Path,
    out: &Path,
    legacy: bool,
    with_stdlib: Option<&Path>,
) -> Result<()> {
    let project = load_with_stdlib(model, with_stdlib)?;
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

fn cmd_emit_clite(model: &Path, out: &Path, with_stdlib: Option<&Path>) -> Result<()> {
    let project = load_with_stdlib(model, with_stdlib)?;
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

    println!(
        "emit-clite: wrote {} (sources) and {} (monitors)",
        clite_dir.display(),
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
        }
    }
    Ok(())
}
