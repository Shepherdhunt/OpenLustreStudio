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
    // Kind 2 v2 has no per-property flag: everything is proved and the
    // results are filtered to the selection.
    assert!(!inv.contains("--lus_props"), "got `{inv}`");
    assert!(inv.contains("--lus_main ReleaseLogic"), "got `{inv}`");
}

/// Kind 2 v2.2 `-json` output, abridged from a real run: the same property
/// reported by two engines, mode checks, and two `ensure`s of one mode.
const V22: &str = r#"[
{"objectType": "log", "level": "info", "source": "parse", "value": "kind2 v2.2.0"},
{"objectType": "property", "name": "C[l2c13].Big[l16c3]", "scope": "Top", "line": 16, "source": "NonVacuityCheck", "answer": {"source": "bmc", "value": "reachable"}, "witness": []},
{"objectType": "property", "name": "C._one_mode_active", "scope": "Top", "line": 16, "source": "OneModeActive", "answer": {"source": "ic3ia", "value": "valid"}},
{"objectType": "property", "name": "C[l2c13].Big[l16c3]", "scope": "Top", "line": 16, "source": "NonVacuityCheck", "answer": {"source": "ic3qe", "value": "reachable"}, "witness": []},
{"objectType": "property", "name": "C[l2c13].Big.ensure[l16c30]", "scope": "Top", "line": 17, "source": "Ensure", "answer": {"source": "ic3qe", "value": "valid"}},
{"objectType": "property", "name": "C[l2c13].Big.ensure[l16c40]", "scope": "Top", "line": 18, "source": "Ensure", "answer": {"source": "ic3qe", "value": "falsifiable"}, "counterExample": []},
{"objectType": "property", "name": "C[l2c13].pos", "scope": "Top", "line": 14, "source": "Guarantee", "answer": {"source": "ind", "value": "valid"}}
]"#;

#[test]
fn kind2_v2_results_are_deduplicated_labeled_and_classified() {
    use ol_kind2::{clause_at, parse_kind2_json, Outcome};
    let props = parse_kind2_json(V22);
    assert_eq!(props.len(), 5, "one result per property: {props:?}");
    let labels: Vec<&str> = props.iter().map(|p| p.label.as_str()).collect();
    assert_eq!(labels, ["C.Big", "C._one_mode_active", "C.Big.ensure #1", "C.Big.ensure #2", "C.pos"]);
    // A reachable mode and a valid guarantee both hold; a falsified ensure fails.
    assert_eq!(props[0].outcome(), Outcome::Holds);
    assert!(props[0].is_mode_check() && props[1].is_mode_check() && !props[4].is_mode_check());
    assert_eq!(props[3].outcome(), Outcome::Fails);
    assert!(props[3].counterexample.is_some());
    // The clause text comes from the input file by line.
    let input = "a\nb\n  guarantee \"pos\" y >= 0;\n";
    assert_eq!(clause_at(input, 3).as_deref(), Some("guarantee \"pos\" y >= 0"));
    assert_eq!(clause_at(input, 9), None);
}

#[test]
fn kind2_errors_and_realizability_are_parsed() {
    let (errors, real) = ol_kind2::parse_kind2_log(
        r#"[
{"objectType": "log", "level": "error", "source": "parse", "file": "m.lus", "line": 13, "column": 4, "value": "Syntax Error!\n"},
{"objectType": "log", "level": "fatal", "source": "parse", "value": "No SMT Solver found."},
{"objectType": "analysisStart", "top": "Top", "context": "environment"},
{"objectType": "realizabilityCheck", "result": "realizable"},
{"objectType": "analysisStart", "top": "Top", "context": "contract"},
{"objectType": "realizabilityCheck", "result": "unrealizable", "conflictingSet": {"nodes": [{"name": "Top", "elements": [{"category": "guarantee", "name": "bad"}]}]}}
]"#,
    );
    assert_eq!(errors, ["line 13, column 4: Syntax Error!", "No SMT Solver found."]);
    assert_eq!(real.len(), 2);
    assert_eq!((real[1].node.as_str(), real[1].context.as_str(), real[1].result.as_str()), ("Top", "contract", "unrealizable"));
    assert_eq!(real[1].conflicting, ["guarantee bad"]);
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
fn counterexample_streams_keep_scope_type_class_and_pad_cycles() {
    // Kind 2 reports Booleans and integers as JSON literals, not strings;
    // one stream here also skips a cycle.
    let cex: serde_json::Value = serde_json::from_str(
        r#"[{
            "blockType": "node", "name": "Main",
            "streams": [
                { "name": "x", "type": "bool", "class": "input",
                  "instantValues": [[0, true], [1, false], [2, true]] },
                { "name": "y", "type": "int", "class": "output",
                  "instantValues": [[0, 0], [2, 7]] }
            ]
        }]"#,
    )
    .unwrap();
    let c = ol_kind2::counterexample_streams(&cex).expect("parses");
    assert_eq!(c.cycles, 3);
    assert_eq!(c.streams.len(), 2);
    assert_eq!(c.streams[0].scope, "Main");
    assert_eq!(c.streams[0].class, "input");
    assert_eq!(c.streams[0].ty, "bool");
    assert_eq!(c.streams[0].values, ["true", "false", "true"]);
    assert_eq!(c.streams[1].values, ["0", "", "7"]);
    assert!(ol_kind2::counterexample_streams(&serde_json::json!({"no": "array"})).is_none());
    // The serialized form names the type `type`, as the Studio reads it.
    let v = serde_json::to_value(&c.streams[1]).unwrap();
    assert_eq!(v["type"], "int");
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
