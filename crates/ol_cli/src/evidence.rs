//! The evidence report: one reviewable document per operator gathering what
//! the tool chain can show about it — the plan's "evidence layer".
//!
//! Sections, each with a status (pass / gaps / fail / not run):
//! 1. static checks — type, clock and contract diagnostics of the operator's
//!    slice;
//! 2. contract — its CoCoSpec (assumptions, guarantees, modes);
//! 3. formal proof — Kind 2 on the contract, when requested and available;
//! 4. tests — the recorded scenarios on the IR simulator;
//! 5. structural coverage — decision coverage and MC/DC measured by them;
//! 6. model ≡ code — the same scenarios on the compiled generated C,
//!    compared cycle by cycle with the model's traces;
//! 7. traceability — every equation of the generated C traced to the model,
//!    with the generated files' SHA-256.
//!
//! The overall verdict is FAIL if any section fails, PASS WITH GAPS if any
//! is incomplete or not run, PASS otherwise. The report is JSON and a
//! standalone HTML page (print it to PDF for a review package).

use std::path::Path;

use ol_clite_emit::trace::{sha256_hex, GenerationReport, TraceEntry};
use ol_ir::{Diagnostic, Project, Severity};
use serde::Serialize;

use crate::scenario::{self, Backend, CoverageSummary, McdcSummary, ScenarioResult, Status as RunStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Gaps,
    Fail,
    NotRun,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Gaps => "gaps",
            Status::Fail => "fail",
            Status::NotRun => "not run",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Status::Pass => "✔",
            Status::Gaps => "▲",
            Status::Fail => "✖",
            Status::NotRun => "—",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub id: &'static str,
    pub title: &'static str,
    pub status: Status,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileHash {
    pub name: String,
    pub bytes: usize,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Identity {
    pub project: String,
    pub operator: String,
    pub kind: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub contract: Option<String>,
    /// SHA-256 of the operator's slice (it and everything it uses), layout
    /// excluded: changes exactly when its behaviour can change.
    pub fingerprint: String,
    pub model_files: Vec<FileHash>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContractInfo {
    pub name: String,
    pub assumptions: usize,
    pub guarantees: usize,
    pub modes: usize,
    pub cocospec: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProofInfo {
    pub note: String,
    pub properties: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioRow {
    pub name: String,
    pub cycles: usize,
    pub ir: String,
    pub c: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Equivalence {
    pub compiler: Option<String>,
    pub flags: String,
    pub scenarios_compared: usize,
    pub cycles_compared: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    pub schema_version: u32,
    pub tool: String,
    pub generated_at: String,
    pub verdict: String,
    pub identity: Identity,
    pub sections: Vec<Section>,
    pub diagnostics: Vec<Diagnostic>,
    pub contract: Option<ContractInfo>,
    pub proof: ProofInfo,
    pub scenarios: Vec<ScenarioRow>,
    pub coverage: Option<CoverageSummary>,
    pub mcdc: Option<McdcSummary>,
    pub equivalence: Equivalence,
    pub generation: GenerationReport,
    pub trace: Vec<TraceEntry>,
}

/// How to run Kind 2 for the proof section.
pub struct Prove {
    pub binary: String,
    pub timeout: Option<u32>,
}

pub struct Request<'a> {
    /// The loaded project (stdlib merged, constructs lowered).
    pub project: &'a Project,
    pub root: &'a str,
    pub scenarios: &'a Path,
    /// The model files as read, for their fingerprints.
    pub model_files: Vec<(String, Vec<u8>)>,
    /// None: the proof section is "not run" (not requested).
    pub prove: Option<Prove>,
}

pub fn collect(req: &Request) -> Result<Evidence, String> {
    let slice = req.project.slice_for_root(req.root)?;
    let node = slice
        .find_node(req.root)
        .ok_or_else(|| format!("operator `{}` not found", req.root))?
        .clone();
    if node.is_imported() {
        return Err(format!("`{}` is an imported C operator — there is no model to evidence", req.root));
    }
    let mut sections = Vec::new();

    // Identity.
    let identity = Identity {
        project: req.project.name.clone(),
        operator: node.name.clone(),
        kind: format!("{:?}", node.kind),
        inputs: node.inputs.iter().map(|p| format!("{}: {}", p.name, p.ty.lustre_name())).collect(),
        outputs: node.outputs.iter().map(|p| format!("{}: {}", p.name, p.ty.lustre_name())).collect(),
        contract: node.contract.clone(),
        fingerprint: fingerprint(&slice),
        model_files: req
            .model_files
            .iter()
            .map(|(n, b)| FileHash { name: n.clone(), bytes: b.len(), sha256: sha256_hex(b) })
            .collect(),
    };

    // 1. Static checks.
    let tc = ol_typecheck::check_project(&slice);
    let cc = ol_contract_check::check_project(&slice);
    let diagnostics: Vec<Diagnostic> = tc
        .diagnostics
        .iter()
        .chain(cc.diagnostics.iter())
        .filter(|d| d.severity != Severity::Info)
        .cloned()
        .collect();
    let errors = diagnostics.iter().filter(|d| d.severity == Severity::Error).count();
    let warnings = diagnostics.len() - errors;
    sections.push(Section {
        id: "checks",
        title: "Static checks (types, clocks, contracts)",
        status: if errors > 0 { Status::Fail } else if warnings > 0 { Status::Gaps } else { Status::Pass },
        summary: format!("{errors} error(s), {warnings} warning(s) across {} operator(s)", slice.all_nodes().count()),
    });

    // 2. Contract.
    let contract = node.contract.as_ref().map(|name| {
        let def = cc.contracts.iter().find(|c| &c.name == name);
        let text = ol_cocospec_emit::emit_project(&slice, ol_cocospec_emit::Target::Modern);
        ContractInfo {
            name: name.clone(),
            assumptions: def.map(|d| d.assumptions.len()).unwrap_or(0),
            guarantees: def.map(|d| d.guarantees.len()).unwrap_or(0),
            modes: def.map(|d| d.modes.len()).unwrap_or(0),
            cocospec: contract_block(&text, name),
        }
    });
    sections.push(Section {
        id: "contract",
        title: "Contract",
        status: if contract.is_some() { Status::Pass } else { Status::Gaps },
        summary: match &contract {
            Some(c) => format!(
                "{}: {} assumption(s), {} guarantee(s), {} mode(s)",
                c.name, c.assumptions, c.guarantees, c.modes
            ),
            None => "no contract attached — nothing to prove or monitor".into(),
        },
    });

    // 3. Formal proof.
    let (proof_status, proof) = prove(&slice, &node.name, contract.is_some(), req.prove.as_ref());
    sections.push(Section {
        id: "proof",
        title: "Formal proof (Kind 2)",
        status: proof_status,
        summary: proof.note.clone(),
    });

    // 4–6. Tests, coverage, model ≡ code.
    let has_scenarios = !scenario::list_scenarios(req.scenarios).is_empty();
    let outcome = if has_scenarios {
        Some(scenario::run_scenarios(req.project, req.scenarios, &node.name, &[Backend::Ir, Backend::C]))
    } else {
        None
    };
    let mut rows = Vec::new();
    let (mut ir_results, mut c_results): (Vec<&ScenarioResult>, Vec<&ScenarioResult>) = (vec![], vec![]);
    if let Some(o) = &outcome {
        for r in &o.results {
            match r.backend {
                Backend::Ir => ir_results.push(r),
                Backend::C => c_results.push(r),
            }
        }
        for tr in &o.traces {
            let status_of = |set: &[&ScenarioResult]| {
                set.iter().find(|r| r.name == tr.name).map(|r| run_status(&r.status)).unwrap_or("—").to_string()
            };
            let detail = ir_results
                .iter()
                .chain(c_results.iter())
                .filter(|r| r.name == tr.name && r.status != RunStatus::Pass)
                .map(|r| {
                    let d = r.diffs.first().map(|d| {
                        format!("cycle {}: `{}` expected {} got {}", d.cycle, d.column, d.expected, d.actual)
                    });
                    format!("{:?}: {}", r.backend, d.unwrap_or_else(|| r.message.clone()))
                })
                .collect::<Vec<_>>()
                .join("; ");
            rows.push(ScenarioRow {
                name: tr.name.clone(),
                cycles: tr.golden.as_ref().map(|g| g.rows.len()).unwrap_or(0),
                ir: status_of(&ir_results),
                c: status_of(&c_results),
                detail,
            });
        }
    }
    let count = |set: &[&ScenarioResult], s: RunStatus| set.iter().filter(|r| r.status == s).count();
    let ir_pass = count(&ir_results, RunStatus::Pass);
    let ir_bad = ir_results.len() - ir_pass - count(&ir_results, RunStatus::NoGolden);
    sections.push(Section {
        id: "tests",
        title: "Tests (scenarios on the model)",
        status: if outcome.is_none() {
            Status::NotRun
        } else if ir_bad > 0 {
            Status::Fail
        } else if ir_pass < ir_results.len() {
            Status::Gaps
        } else {
            Status::Pass
        },
        summary: match &outcome {
            None => format!("no scenarios in {}", req.scenarios.display()),
            Some(_) => format!(
                "{ir_pass} of {} scenario(s) match their recorded traces{}",
                ir_results.len(),
                if ir_pass < ir_results.len() && ir_bad == 0 { " (the rest have no golden trace yet)" } else { "" }
            ),
        },
    });
    let coverage = outcome.as_ref().and_then(|o| o.coverage.clone());
    let mcdc = outcome.as_ref().and_then(|o| o.mcdc.clone());
    sections.push(Section {
        id: "coverage",
        title: "Structural coverage (decision, MC/DC)",
        status: match (&outcome, &coverage, &mcdc) {
            (None, ..) => Status::NotRun,
            (_, Some(c), Some(m)) if c.covered == c.total && m.covered_conditions == m.total_conditions => Status::Pass,
            (_, None, None) => Status::Pass,
            _ => Status::Gaps,
        },
        summary: match (&outcome, &coverage, &mcdc) {
            (None, ..) => "needs scenarios".into(),
            (_, None, None) => "no decisions to cover".into(),
            _ => {
                let mut parts = Vec::new();
                if let Some(c) = coverage.as_ref().filter(|c| c.total > 0) {
                    parts.push(format!("if-decisions {}/{}", c.covered, c.total));
                }
                if let Some(m) = mcdc.as_ref().filter(|m| m.total_conditions > 0) {
                    parts.push(format!("MC/DC conditions {}/{}", m.covered_conditions, m.total_conditions));
                }
                if parts.is_empty() { "no decisions to cover".into() } else { parts.join(" · ") }
            }
        },
    });
    let c_pass: Vec<&&ScenarioResult> = c_results.iter().filter(|r| r.status == RunStatus::Pass).collect();
    let c_bad = c_results.iter().filter(|r| matches!(r.status, RunStatus::Fail | RunStatus::Error)).count();
    let c_skipped = c_results.iter().any(|r| r.status == RunStatus::Skipped);
    let cycles_compared: usize = rows.iter().filter(|r| c_pass.iter().any(|c| c.name == r.name)).map(|r| r.cycles).sum();
    let equivalence = Equivalence {
        compiler: scenario::compiler_identity(),
        flags: scenario::C_FLAGS.join(" "),
        scenarios_compared: c_pass.len(),
        cycles_compared,
    };
    sections.push(Section {
        id: "equivalence",
        title: "Model ≡ generated code",
        status: if outcome.is_none() || c_skipped || c_results.is_empty() {
            Status::NotRun
        } else if c_bad > 0 {
            Status::Fail
        } else if c_pass.len() < c_results.len() {
            Status::Gaps
        } else {
            Status::Pass
        },
        summary: if outcome.is_none() {
            "needs scenarios".into()
        } else if c_skipped {
            "no C compiler found — the generated C was not run".into()
        } else {
            format!(
                "compiled C matches the model on {} of {} scenario(s), {} cycles compared",
                c_pass.len(),
                c_results.len(),
                cycles_compared
            )
        },
    });

    // 7. Traceability.
    let bundle = ol_clite_emit::emit_project(&slice);
    let driver = ol_clite_emit::harness::emit_csv_driver(&node);
    let files = [
        ("openlustre_generated.h", bundle.header.as_str()),
        ("openlustre_generated.c", bundle.source.as_str()),
        ("driver.c", driver.as_str()),
    ];
    let generation = ol_clite_emit::trace::report(&slice, Some(&node.name), &files, &bundle.trace);
    sections.push(Section {
        id: "traceability",
        title: "Model-to-code traceability",
        status: if generation.traced == generation.equations { Status::Pass } else { Status::Gaps },
        summary: format!(
            "{} of {} equations traced to the model; {} generated file(s) fingerprinted",
            generation.traced,
            generation.equations,
            generation.files.len()
        ),
    });

    let verdict = if sections.iter().any(|s| s.status == Status::Fail) {
        "FAIL"
    } else if sections.iter().any(|s| matches!(s.status, Status::Gaps | Status::NotRun)) {
        "PASS WITH GAPS"
    } else {
        "PASS"
    };
    Ok(Evidence {
        schema_version: 1,
        tool: format!("OpenLustre Studio {}", env!("CARGO_PKG_VERSION")),
        generated_at: utc_now(),
        verdict: verdict.into(),
        identity,
        sections,
        diagnostics,
        contract,
        proof,
        scenarios: rows,
        coverage,
        mcdc,
        equivalence,
        generation,
        trace: bundle.trace,
    })
}

fn run_status(s: &RunStatus) -> &'static str {
    match s {
        RunStatus::Pass => "pass",
        RunStatus::Fail => "fail",
        RunStatus::NoGolden => "no golden",
        RunStatus::Skipped => "skipped",
        RunStatus::Error => "error",
    }
}

/// SHA-256 of the slice with diagram layout stripped.
fn fingerprint(slice: &Project) -> String {
    let mut p = slice.clone();
    for pkg in &mut p.packages {
        for n in &mut pkg.nodes {
            n.diagram = Default::default();
        }
    }
    sha256_hex(serde_json::to_string(&p).unwrap_or_default().as_bytes())
}

/// The `contract Name(…) … tel` block of CoCoSpec text.
fn contract_block(text: &str, name: &str) -> String {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with(&format!("contract {name}(")) || line.starts_with(&format!("contract {name} (")) {
            inside = true;
        }
        if inside {
            out.push(line);
            if line.trim() == "tel" || line.trim() == "tel;" {
                break;
            }
        }
    }
    out.join("\n")
}

/// Run Kind 2 on the slice's Lustre + CoCoSpec, when asked and possible.
fn prove(slice: &Project, root: &str, has_contract: bool, opts: Option<&Prove>) -> (Status, ProofInfo) {
    let info = |note: &str| ProofInfo { note: note.into(), properties: vec![] };
    let Some(opts) = opts else {
        return (Status::NotRun, info("not requested (run with proving enabled to include Kind 2 results)"));
    };
    if !has_contract {
        return (Status::NotRun, info("no contract to prove"));
    }
    let lus = ol_lustre_emit::emit_project(slice);
    let con = ol_cocospec_emit::emit_project(slice, ol_cocospec_emit::Target::Modern);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let work = std::env::temp_dir().join(format!("openlustre_evidence_{stamp}"));
    if std::fs::create_dir_all(&work).is_err() {
        return (Status::NotRun, info("could not create a working directory for Kind 2"));
    }
    let lus_path = work.join("model_with_contracts.lus");
    let _ = std::fs::write(&lus_path, format!("{lus}\n{con}"));
    let result = ol_kind2::run_kind2(
        &lus_path,
        &ol_kind2::Kind2Options {
            kind2_binary: opts.binary.clone(),
            mode: ol_kind2::SerMode::BmcInd,
            main_node: Some(root.to_string()),
            extra_args: vec![],
            timeout_seconds: opts.timeout,
            properties: vec![],
        },
    );
    let _ = std::fs::remove_dir_all(&work);
    let result = match result {
        Ok(r) => r,
        Err(e) => return (Status::NotRun, info(&format!("Kind 2 could not run: {e}"))),
    };
    if result.exit_code == -1 && result.stderr.contains("could not launch") {
        return (Status::NotRun, info(&format!("Kind 2 not found (`{}`) — install it to include proofs", opts.binary)));
    }
    let properties: Vec<(String, String)> = result.properties.iter().map(|p| (p.name.clone(), p.status.clone())).collect();
    if properties.is_empty() {
        return (Status::Gaps, ProofInfo { note: "Kind 2 ran but reported no properties".into(), properties });
    }
    let valid = properties.iter().filter(|(_, s)| s.eq_ignore_ascii_case("valid")).count();
    let falsified = properties
        .iter()
        .filter(|(_, s)| s.eq_ignore_ascii_case("falsifiable") || s.eq_ignore_ascii_case("invalid"))
        .count();
    let status = if falsified > 0 {
        Status::Fail
    } else if valid < properties.len() {
        Status::Gaps
    } else {
        Status::Pass
    };
    let note = format!(
        "{valid} of {} {} valid{}",
        properties.len(),
        if properties.len() == 1 { "property" } else { "properties" },
        if falsified > 0 { format!(", {falsified} falsified") } else { String::new() }
    );
    (status, ProofInfo { note, properties })
}

/// The current time as ISO 8601 UTC (`2026-09-29T14:03:00Z`).
fn utc_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

// --- HTML ----------------------------------------------------------------------

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn chip(s: Status) -> String {
    format!(r#"<span class="chip {}">{} {}</span>"#, s.label().replace(' ', "-"), s.icon(), s.label())
}

fn status_of_verdict(v: &str) -> Status {
    match v {
        "PASS" => Status::Pass,
        "FAIL" => Status::Fail,
        _ => Status::Gaps,
    }
}

impl Evidence {
    /// A standalone, print-ready HTML page.
    pub fn to_html(&self) -> String {
        let id = &self.identity;
        let mut h = String::new();
        let sec_status = |key: &str| self.sections.iter().find(|s| s.id == key).map(|s| s.status).unwrap_or(Status::NotRun);
        h.push_str(&format!(
            r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Evidence — {op}</title><style>{css}</style></head><body><main>
<header><div class="eyebrow">Verification evidence</div><h1>{op}</h1>
<p class="sub">{project} · generated {at} by {tool}</p>
<div class="verdict {vcls}">{vicon} {verdict}</div></header>"#,
            op = esc(&id.operator),
            css = CSS,
            project = esc(&id.project),
            at = esc(&self.generated_at),
            tool = esc(&self.tool),
            vcls = status_of_verdict(&self.verdict).label().replace(' ', "-"),
            vicon = status_of_verdict(&self.verdict).icon(),
            verdict = esc(&self.verdict),
        ));

        // Summary.
        h.push_str("<section><h2>Summary</h2><table><thead><tr><th>evidence</th><th>status</th><th>result</th></tr></thead><tbody>");
        for s in &self.sections {
            h.push_str(&format!(
                r##"<tr><td><a href="#{}">{}</a></td><td>{}</td><td>{}</td></tr>"##,
                s.id,
                esc(s.title),
                chip(s.status),
                esc(&s.summary)
            ));
        }
        h.push_str("</tbody></table></section>");

        // Identification.
        h.push_str("<section><h2>Identification</h2><table class=\"kv\"><tbody>");
        let kv = |k: &str, v: String| format!("<tr><th>{k}</th><td>{v}</td></tr>");
        h.push_str(&kv("operator", format!("{} ({})", esc(&id.operator), esc(&id.kind))));
        h.push_str(&kv("inputs", esc(&id.inputs.join(", "))));
        h.push_str(&kv("outputs", esc(&id.outputs.join(", "))));
        h.push_str(&kv("contract", esc(id.contract.as_deref().unwrap_or("—"))));
        h.push_str(&kv(
            "model fingerprint",
            format!("<code>{}</code><br><span class=\"note\">SHA-256 of the operator and everything it uses, layout excluded</span>", id.fingerprint),
        ));
        for f in &id.model_files {
            h.push_str(&kv("model file", format!("{} — {} bytes<br><code>{}</code>", esc(&f.name), f.bytes, f.sha256)));
        }
        h.push_str("</tbody></table>");

        // 1. Checks.
        h.push_str(&section_head("checks", "Static checks (types, clocks, contracts)", sec_status("checks")));
        if self.diagnostics.is_empty() {
            h.push_str("<p>No errors or warnings.</p>");
        } else {
            h.push_str("<table><thead><tr><th>severity</th><th>code</th><th>message</th><th>where</th></tr></thead><tbody>");
            for d in &self.diagnostics {
                h.push_str(&format!(
                    "<tr><td>{:?}</td><td><code>{}</code></td><td>{}</td><td>{}</td></tr>",
                    d.severity,
                    esc(&d.code),
                    esc(&d.message),
                    esc(&d.context.join(" · "))
                ));
            }
            h.push_str("</tbody></table>");
        }

        // 2. Contract.
        h.push_str(&section_head("contract", "Contract", sec_status("contract")));
        match &self.contract {
            Some(c) => h.push_str(&format!(
                "<p>{} assumption(s), {} guarantee(s), {} mode(s).</p><pre>{}</pre>",
                c.assumptions,
                c.guarantees,
                c.modes,
                esc(&c.cocospec)
            )),
            None => h.push_str("<p>No contract is attached to this operator.</p>"),
        }

        // 3. Proof.
        h.push_str(&section_head("proof", "Formal proof (Kind 2)", sec_status("proof")));
        h.push_str(&format!("<p>{}</p>", esc(&self.proof.note)));
        if !self.proof.properties.is_empty() {
            h.push_str("<table><thead><tr><th>property</th><th>result</th></tr></thead><tbody>");
            for (n, s) in &self.proof.properties {
                h.push_str(&format!("<tr><td>{}</td><td>{}</td></tr>", esc(n), esc(s)));
            }
            h.push_str("</tbody></table>");
        }

        // 4. Tests.
        h.push_str(&section_head("tests", "Tests (scenarios on the model)", sec_status("tests")));
        if self.scenarios.is_empty() {
            h.push_str("<p>No scenarios.</p>");
        } else {
            h.push_str("<table><thead><tr><th>scenario</th><th>cycles</th><th>model (IR)</th><th>generated C</th><th>detail</th></tr></thead><tbody>");
            for r in &self.scenarios {
                h.push_str(&format!(
                    "<tr><td>{}</td><td class=\"num\">{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    esc(&r.name),
                    r.cycles,
                    esc(&r.ir),
                    esc(&r.c),
                    esc(&r.detail)
                ));
            }
            h.push_str("</tbody></table>");
        }

        // 5. Coverage.
        h.push_str(&section_head("coverage", "Structural coverage (decision, MC/DC)", sec_status("coverage")));
        if let Some(c) = &self.coverage {
            h.push_str(&format!("<p>Decision coverage: {} of {} if-conditions driven both true and false.</p>", c.covered, c.total));
            if !c.uncovered.is_empty() {
                h.push_str("<table><thead><tr><th>operator</th><th>equation</th><th>condition</th><th>missing</th></tr></thead><tbody>");
                for u in &c.uncovered {
                    h.push_str(&format!(
                        "<tr><td>{}</td><td>{}</td><td><code>{}</code></td><td>{}</td></tr>",
                        esc(&u.node),
                        esc(&u.context),
                        esc(&u.condition),
                        esc(&u.missing)
                    ));
                }
                h.push_str("</tbody></table>");
            }
        }
        if let Some(m) = &self.mcdc {
            h.push_str(&format!(
                "<p>MC/DC (DO-178C level A): {} of {} conditions shown to independently affect their decision; {} of {} decisions fully covered.</p>",
                m.covered_conditions, m.total_conditions, m.covered_decisions, m.total_decisions
            ));
            if !m.uncovered.is_empty() {
                h.push_str("<table><thead><tr><th>operator</th><th>equation</th><th>decision</th><th>condition</th><th>why</th></tr></thead><tbody>");
                for u in &m.uncovered {
                    h.push_str(&format!(
                        "<tr><td>{}</td><td>{}</td><td><code>{}</code></td><td><code>{}</code></td><td>{}</td></tr>",
                        esc(&u.node),
                        esc(&u.context),
                        esc(&u.decision),
                        esc(&u.condition),
                        esc(&u.reason)
                    ));
                }
                h.push_str("</tbody></table>");
            }
        }
        if self.coverage.is_none() && self.mcdc.is_none() {
            h.push_str(&format!("<p>{}</p>", esc(&self.sections.iter().find(|s| s.id == "coverage").map(|s| s.summary.clone()).unwrap_or_default())));
        }

        // 6. Equivalence.
        h.push_str(&section_head("equivalence", "Model ≡ generated code", sec_status("equivalence")));
        let eq = &self.equivalence;
        h.push_str(&format!(
            "<p>Each scenario's recorded model trace is replayed through the compiled generated C and compared cell by cell. {} scenario(s) and {} cycles compared.</p><table class=\"kv\"><tbody>{}{}</tbody></table>",
            eq.scenarios_compared,
            eq.cycles_compared,
            kv("compiler", esc(eq.compiler.as_deref().unwrap_or("none found"))),
            kv("flags", format!("<code>{}</code>", esc(&eq.flags))),
        ));

        // 7. Traceability.
        h.push_str(&section_head("traceability", "Model-to-code traceability", sec_status("traceability")));
        h.push_str("<h3>Generated files</h3><table><thead><tr><th>file</th><th>lines</th><th>bytes</th><th>SHA-256</th></tr></thead><tbody>");
        for f in &self.generation.files {
            h.push_str(&format!(
                "<tr><td><code>{}</code></td><td class=\"num\">{}</td><td class=\"num\">{}</td><td><code>{}</code></td></tr>",
                esc(&f.name),
                f.lines,
                f.bytes,
                f.sha256
            ));
        }
        h.push_str("</tbody></table><h3>Trace matrix</h3><table><thead><tr><th>operator</th><th>element</th><th>origin</th><th>equation</th><th>C lines</th></tr></thead><tbody>");
        for t in &self.trace {
            h.push_str(&format!(
                "<tr><td>{}</td><td><code>{}</code></td><td>{}</td><td><code>{}</code></td><td class=\"num\">{}–{}</td></tr>",
                esc(&t.operator),
                esc(&t.element),
                esc(t.origin.as_deref().unwrap_or("")),
                esc(&t.source),
                t.first_line,
                t.last_line
            ));
        }
        h.push_str("</tbody></table></section>");
        h.push_str("<footer>Statuses: ✔ pass · ▲ gaps (incomplete) · ✖ fail · — not run. The verdict is FAIL if any section fails, PASS WITH GAPS if any is incomplete or not run.</footer>");
        h.push_str("</main></body></html>");
        h
    }
}

/// Closes the section before it (Identification leaves its own open) and
/// opens the next.
fn section_head(id: &str, title: &str, s: Status) -> String {
    format!(r#"</section><section id="{id}"><h2>{} {}</h2>"#, esc(title), chip(s))
}

const CSS: &str = r#"
:root { --ink: #1e1e1e; --muted: #5a5a5a; --line: #dcdcdc; --head: #f3f4f6; --accent: #2b579a;
  --pass: #107c10; --pass-bg: #e6f4ea; --fail: #b3261e; --fail-bg: #fdeeee; --gaps: #8a5300; --gaps-bg: #fff4e0;
  --nr: #5a5a5a; --nr-bg: #eeeeee; }
* { box-sizing: border-box; }
body { margin: 0; background: #fff; color: var(--ink); font: 13px/1.5 "Segoe UI", -apple-system, BlinkMacSystemFont, sans-serif; }
main { max-width: 1040px; margin: 0 auto; padding: 24px 16px 48px; }
header { border-bottom: 2px solid var(--accent); padding-bottom: 12px; margin-bottom: 8px; }
.eyebrow { text-transform: uppercase; letter-spacing: .08em; font-size: 11px; color: var(--accent); font-weight: 600; }
h1 { margin: 2px 0 2px; font-size: 26px; }
.sub { margin: 0 0 10px; color: var(--muted); }
h2 { font-size: 17px; margin: 26px 0 8px; display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
h3 { font-size: 13px; margin: 14px 0 6px; color: var(--muted); text-transform: uppercase; letter-spacing: .04em; }
p { margin: 6px 0; }
.verdict { display: inline-block; padding: 6px 14px; border-radius: 4px; font-weight: 700; font-size: 15px; }
.verdict.pass { color: var(--pass); background: var(--pass-bg); }
.verdict.fail { color: var(--fail); background: var(--fail-bg); }
.verdict.gaps { color: var(--gaps); background: var(--gaps-bg); }
.chip { display: inline-block; padding: 1px 8px; border-radius: 10px; font-size: 11px; font-weight: 600; white-space: nowrap; }
.chip.pass { color: var(--pass); background: var(--pass-bg); }
.chip.fail { color: var(--fail); background: var(--fail-bg); }
.chip.gaps { color: var(--gaps); background: var(--gaps-bg); }
.chip.not-run { color: var(--nr); background: var(--nr-bg); }
table { border-collapse: collapse; width: 100%; margin: 6px 0 10px; }
th, td { text-align: left; vertical-align: top; padding: 4px 8px; border: 1px solid var(--line); }
th { background: var(--head); font-weight: 600; }
td.num { text-align: right; font-variant-numeric: tabular-nums; }
table.kv th { width: 170px; }
code, pre { font-family: Consolas, "Cascadia Mono", monospace; font-size: 12px; overflow-wrap: anywhere; }
pre { background: #f7f7f8; border: 1px solid var(--line); padding: 8px 10px; white-space: pre-wrap; }
a { color: var(--accent); }
.note { color: var(--muted); font-size: 11px; }
footer { margin-top: 28px; padding-top: 8px; border-top: 1px solid var(--line); color: var(--muted); font-size: 11px; }
@media print {
  main { max-width: none; padding: 0; }
  section, table, pre { break-inside: avoid; }
  a { color: inherit; text-decoration: none; }
}
@page { margin: 16mm; }
"#;
