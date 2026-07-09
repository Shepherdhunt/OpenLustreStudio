//! Phase 7 polish: Kind 2 timeout / property-selection options propagate
//! to the kind2 invocation, and counterexamples render as a per-cycle
//! waveform table. None of these tests require Kind 2 to be installed —
//! they exercise the adapter's argument building and JSON parsing with
//! fixtures.

use std::path::PathBuf;

use ol_kind2::{render_counterexample_waveform, run_kind2, Kind2Options, SerMode};

#[test]
fn timeout_and_property_selection_flow_into_kind2_invocation() {
    let opts = Kind2Options {
        kind2_binary: "/nonexistent/kind2".into(),
        mode: SerMode::BmcInd,
        main_node: Some("ReleaseLogic".into()),
        extra_args: vec![],
        timeout_seconds: Some(30),
        properties: vec!["g1".into(), "g2".into()],
    };
    let result = run_kind2(&PathBuf::from("/tmp/missing.lus"), &opts).expect("returns result");
    // `kind2` isn't actually on disk, so the adapter returns a "could not
    // launch" message — but the recorded invocation should contain the
    // built argument list, which is what we want to check.
    let inv = result.invocation.join(" ");
    assert!(inv.contains("--timeout_wall 30"), "got `{inv}`");
    assert!(inv.contains("--lus_props g1,g2"), "got `{inv}`");
    assert!(inv.contains("--lus_main ReleaseLogic"), "got `{inv}`");
}

#[test]
fn defaults_do_not_emit_timeout_or_properties_args() {
    let opts = Kind2Options::default();
    let result = run_kind2(&PathBuf::from("/tmp/missing.lus"), &opts).expect("returns");
    let inv = result.invocation.join(" ");
    assert!(!inv.contains("--timeout_wall"), "got `{inv}`");
    assert!(!inv.contains("--lus_props"), "got `{inv}`");
}

#[test]
fn waveform_renders_a_kind2_counterexample() {
    // Realistic Kind 2 `-json` counterexample shape: an array of scopes,
    // each with a `streams` array. Each stream has `instantValues` pairs.
    let cex: serde_json::Value = serde_json::from_str(
        r#"[{
            "blockType": "node",
            "name": "Main",
            "streams": [
                { "name": "x", "type": "bool",
                  "instantValues": [[0, "true"], [1, "false"], [2, "true"]] },
                { "name": "y", "type": "int",
                  "instantValues": [[0, "0"], [1, "1"], [2, "2"]] }
            ]
        }]"#,
    )
    .unwrap();

    let rendered = render_counterexample_waveform(&cex).expect("renders");
    // The table must have a header row, a separator, and one row per cycle.
    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(lines.len(), 2 + 3, "lines: {rendered}");
    assert!(lines[0].contains("cycle") && lines[0].contains("x") && lines[0].contains("y"));
    assert!(lines[1].chars().all(|c| c == '-' || c == '+' || c == ' '));
    assert!(lines[2].contains("0") && lines[2].contains("true") && lines[2].contains("0"));
    assert!(lines[3].contains("1") && lines[3].contains("false") && lines[3].contains("1"));
    assert!(lines[4].contains("2") && lines[4].contains("true") && lines[4].contains("2"));
}

#[test]
fn waveform_returns_none_for_a_non_array_counterexample() {
    let cex: serde_json::Value = serde_json::json!({"oops": "shape"});
    assert!(render_counterexample_waveform(&cex).is_none());
}

#[test]
fn waveform_renders_multi_scope_counterexamples_into_one_table() {
    // Two scopes that share the cycle axis — typical when a contract and
    // its node both produce streams.
    let cex: serde_json::Value = serde_json::from_str(
        r#"[
            { "blockType": "node", "name": "A",
              "streams": [
                { "name": "a", "type": "bool",
                  "instantValues": [[0, "true"], [1, "true"]] }
              ]
            },
            { "blockType": "node", "name": "B",
              "streams": [
                { "name": "b", "type": "int",
                  "instantValues": [[0, "0"], [1, "42"]] }
              ]
            }
        ]"#,
    )
    .unwrap();

    let rendered = render_counterexample_waveform(&cex).expect("renders");
    assert!(rendered.contains("a"));
    assert!(rendered.contains("b"));
    assert!(rendered.contains("42"));
}

// --- Counterexample replay: the input streams as a steppable trace ------------

#[test]
fn input_trace_extracts_only_input_streams_and_converts_fractions() {
    let cex: serde_json::Value = serde_json::from_str(
        r#"[{
            "blockType": "node",
            "name": "Main",
            "streams": [
                { "name": "arm", "type": "bool", "class": "input",
                  "instantValues": [[0, "false"], [1, "true"], [2, "true"]] },
                { "name": "gain", "type": "real", "class": "input",
                  "instantValues": [[0, "-1/2"], [1, "3/4"], [2, "1"]] },
                { "name": "release", "type": "bool", "class": "output",
                  "instantValues": [[0, "false"], [1, "false"], [2, "true"]] }
            ]
        }]"#,
    )
    .unwrap();
    let (cols, rows) = ol_kind2::counterexample_input_trace(&cex).expect("trace");
    assert_eq!(cols, vec!["arm", "gain"], "outputs are not replay inputs");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], vec!["false", "-0.5"], "fractions become decimals");
    assert_eq!(rows[1], vec!["true", "0.75"]);
    assert_eq!(rows[2], vec!["true", "1"]);
}

#[test]
fn input_trace_holds_values_through_gaps_and_rejects_no_inputs() {
    // Kind 2 sometimes omits unchanged instants: the trace must hold the
    // last seen value so every replay row is complete.
    let cex: serde_json::Value = serde_json::from_str(
        r#"[{ "streams": [
            { "name": "x", "type": "int", "class": "input",
              "instantValues": [[0, "7"], [3, "9"]] }
        ]}]"#,
    )
    .unwrap();
    let (cols, rows) = ol_kind2::counterexample_input_trace(&cex).expect("trace");
    assert_eq!(cols, vec!["x"]);
    assert_eq!(
        rows.iter().map(|r| r[0].as_str()).collect::<Vec<_>>(),
        vec!["7", "7", "7", "9"],
        "gaps hold the last value"
    );

    let no_inputs: serde_json::Value = serde_json::from_str(
        r#"[{ "streams": [ { "name": "y", "class": "output", "instantValues": [[0, "1"]] } ] }]"#,
    )
    .unwrap();
    assert!(ol_kind2::counterexample_input_trace(&no_inputs).is_none());
}
