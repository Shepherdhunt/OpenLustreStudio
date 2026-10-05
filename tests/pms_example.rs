//! The Payload Management System example (examples/pms, a snapshot of the
//! PayloadManagementSystem repository; see its PLAN.md):
//! a complete Studio project that must stay clean, tested and proved.
//!
//! * The workspace type-checks and contract-checks with no findings.
//! * Every scenario matches its golden trace on the model and on the
//!   compiled generated C, with full decision and MC/DC coverage.
//! * With Kind 2 installed (see kind2_projection.rs for how it is found),
//!   every property of every contract is proved, and so is the absence of
//!   runtime errors (RTE-1).

use std::path::PathBuf;
use std::process::Command;

fn pms() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples/pms")
}

fn project() -> ol_ir::Project {
    let mut p = ol_ir::load_project(&pms().join("pms.wksc")).expect("workspace loads");
    p.lower_state_machines().expect("state machines lower");
    p.lower_activations().expect("activations lower");
    p
}

fn has_cc() -> bool {
    ["cc", "gcc", "clang"]
        .iter()
        .any(|c| Command::new(c).arg("--version").output().map(|o| o.status.success()).unwrap_or(false))
}

#[test]
fn the_pms_project_checks_clean() {
    let p = project();
    assert_eq!(p.main.as_deref(), Some("PMS"));
    let names: Vec<&str> = p.all_nodes().map(|n| n.name.as_str()).collect();
    for op in ["StationDecode", "Balance", "PlanRelease", "ReleaseSequencer", "PMS"] {
        assert!(names.contains(&op), "{op} missing from {names:?}");
    }
    let tc = ol_typecheck::check_project(&p);
    assert!(tc.diagnostics.is_empty(), "{:?}", tc.diagnostics);
    let cc = ol_contract_check::check_project(&p);
    let findings: Vec<_> = cc.diagnostics.iter().filter(|d| d.severity != ol_ir::Severity::Info).collect();
    assert!(findings.is_empty(), "{findings:?}");
    // Every operator carries a contract.
    for n in p.all_nodes() {
        assert!(n.contract.is_some(), "`{}` has no contract", n.name);
    }
}

#[test]
fn every_scenario_matches_on_the_model_and_the_generated_c_with_full_mcdc() {
    let backend = if has_cc() { "both" } else { "ir" };
    let o = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "test", "run"])
        .arg(pms().join("pms.wksc"))
        .arg("--scenarios")
        .arg(pms().join("scenarios"))
        .args(["--backend", backend])
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(o.status.success(), "{text}");
    let runs = if backend == "both" { 16 } else { 8 };
    assert!(text.contains(&format!("{runs} passed, 0 failed")), "{text}");
    assert!(text.contains("MC/DC: 151/151 conditions independent (96/96 decisions fully covered)"), "{text}");
}

/// The flight code: the C generated for PMS, compiled with the platform
/// integration (examples/pms/integration), flies the scripted 50 s mission
/// and logs exactly what the project recorded.
#[test]
fn the_generated_flight_code_flies_the_scripted_mission() {
    if !has_cc() {
        eprintln!("no C compiler: skipping the mission");
        return;
    }
    let stamp = openlustre_integration_tests::unique_stamp();
    let out = std::env::temp_dir().join(format!("ol_pms_mission_{stamp}"));
    let o = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "emit-clite"])
        .arg(pms().join("pms.wksc"))
        .args(["--root", "PMS", "--out"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let integration = pms().join("integration");
    let exe = out.join("pms_mission");
    let cc = ["cc", "gcc", "clang"].into_iter().find(|c| Command::new(c).arg("--version").output().is_ok()).unwrap();
    let o = Command::new(cc)
        .args(["-std=c11", "-O2", "-Wall", "-Wextra", "-Wno-unused-but-set-variable", "-Wno-unused-variable", "-Werror"])
        .arg(format!("-I{}", out.join("clite").display()))
        .arg(format!("-I{}", integration.display()))
        .arg(out.join("clite/openlustre_generated.c"))
        .arg(integration.join("pms_task.c"))
        .arg(integration.join("pms_clock.c"))
        .arg(integration.join("mission_sim.c"))
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let log = Command::new(&exe).output().unwrap();
    let _ = std::fs::remove_dir_all(&out);
    let expected = std::fs::read_to_string(integration.join("expected_mission.txt")).unwrap();
    assert_eq!(String::from_utf8(log.stdout).unwrap(), expected);
}

/// The C test driver reads and prints enums by name, as the simulator's
/// traces do — the PMS's inputs and outputs are mostly enums.
#[test]
fn the_generated_c_driver_speaks_enum_names() {
    let p = project();
    let pms = p.find_node("PMS").unwrap();
    let driver = ol_clite_emit::harness::emit_csv_driver_for(&p, pms, None);
    assert!(driver.contains("static int ol_enum_StoreKind(const char* s)"), "{driver}");
    assert!(driver.contains("if (strcmp(s, \"MedKit\") == 0) return MedKit;"), "{driver}");
    assert!(driver.contains("case WouldUnbalance: return \"WouldUnbalance\";"), "{driver}");
    assert!(driver.contains("in.req_kind = (StoreKind) ol_enum_StoreKind(tok);"), "{driver}");
    // Only the helpers that are used (unused statics would fail -Werror).
    assert!(!driver.contains("ol_enum_Inhibit("), "Inhibit is never read: {driver}");
}

#[test]
fn the_pms_is_proved_by_kind2() {
    let on_path = |exe: &str| {
        std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(exe)).find(|f| f.is_file()))
    };
    let bin = std::env::var_os("OPENLUSTRE_KIND2").map(PathBuf::from).or_else(|| on_path("kind2"));
    let z3 = std::env::var_os("OPENLUSTRE_Z3").map(PathBuf::from).or_else(|| on_path("z3"));
    let (Some(bin), Some(z3)) = (bin, z3) else {
        assert!(std::env::var_os("OPENLUSTRE_REQUIRE_KIND2").is_none(), "kind2 / z3 required but not found");
        eprintln!("kind2 or z3 not found: skipping the PMS proof");
        return;
    };
    let slice = project().slice_for_root("PMS").unwrap();
    let input = ol_cocospec_emit::kind2::emit(&slice).expect("Kind 2 view");
    let stamp = openlustre_integration_tests::unique_stamp();
    let dir = std::env::temp_dir().join(format!("ol_pms_proof_{stamp}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pms.lus");
    std::fs::write(&path, &input.text).unwrap();
    let result = ol_kind2::run_kind2(
        &path,
        &ol_kind2::Kind2Options {
            kind2_binary: bin.display().to_string(),
            main_node: Some("PMS".into()),
            timeout_seconds: Some(900),
            extra_args: vec!["--smt_solver".into(), "Z3".into(), "--z3_bin".into(), z3.display().to_string()],
            ..Default::default()
        },
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(result.properties.len() >= 100, "{} properties", result.properties.len());
    let open: Vec<_> = result
        .properties
        .iter()
        .filter(|p| p.outcome() != ol_kind2::Outcome::Holds)
        .map(|p| format!("{} {}", p.label, p.status))
        .collect();
    assert!(open.is_empty(), "not proved: {open:?}");
    // The requirement-level guarantees are among them.
    for id in ["REL_1_armed_airborne", "REL_3_lateral_pairs", "REL_4_pulse_bounded", "REL_5_no_refire_hung",
               "BAL_2_stays_balanced", "BAL_5_complete"] {
        assert!(result.properties.iter().any(|p| p.label.contains(id)), "{id} not reported");
    }
    // RTE-1: no runtime error — every moment, sum and counter fits int32, in
    // the context of PMS (so all of them are among the proved properties).
    assert!(input.checks.len() >= 50, "{} runtime-error checks", input.checks.len());
    for c in &input.checks {
        assert!(result.properties.iter().any(|p| p.name == c.name), "{} not reported", c.describe());
    }
    assert!(input.checks.iter().any(|c| c.node == "ReleaseSequencer" && c.what == "pre cnt + 1 fits int32"));
}
