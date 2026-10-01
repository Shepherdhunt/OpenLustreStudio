//! Multi-file project loading: `includes:` lists, directory mode, and cycle
//! detection. Real models split across files must load as one merged project.

use std::path::PathBuf;

use ol_ir::load_project;

fn make_tempdir() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let p =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_multifile_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn write(path: PathBuf, contents: &str) -> PathBuf {
    std::fs::write(&path, contents).unwrap();
    path
}

#[test]
fn loader_follows_an_includes_list_and_merges_packages() {
    let dir = make_tempdir();
    write(
        dir.join("child.yaml"),
        r#"
name: child
packages:
  - name: shared
    nodes:
      - name: B
        kind: Function
        inputs:  [{ name: x, ty: { kind: Bool } }]
        outputs: [{ name: y, ty: { kind: Bool } }]
        equations: [{ lhs: [y], rhs: { expr: Var, name: x } }]
"#,
    );
    write(
        dir.join("root.yaml"),
        r#"
name: root
includes: [child.yaml]
packages:
  - name: shared
    nodes:
      - name: A
        kind: Function
        inputs:  [{ name: x, ty: { kind: Bool } }]
        outputs: [{ name: y, ty: { kind: Bool } }]
        equations: [{ lhs: [y], rhs: { expr: Var, name: x } }]
"#,
    );

    let project = load_project(&dir.join("root.yaml")).expect("loads");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(project.packages.len(), 1, "packages merged by name");
    let shared = &project.packages[0];
    assert_eq!(shared.name, "shared");
    let names: Vec<&str> = shared.nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"A"));
    assert!(names.contains(&"B"));
}

#[test]
fn loader_treats_a_directory_as_a_merged_project() {
    let dir = make_tempdir();
    write(
        dir.join("a.yaml"),
        r#"
name: a
packages:
  - name: lib
    nodes:
      - name: A
        kind: Function
        inputs:  [{ name: x, ty: { kind: Bool } }]
        outputs: [{ name: y, ty: { kind: Bool } }]
        equations: [{ lhs: [y], rhs: { expr: Var, name: x } }]
"#,
    );
    write(
        dir.join("b.json"),
        r#"
{
  "name": "b",
  "packages": [{
    "name": "lib",
    "nodes": [{
      "name": "B",
      "kind": "Function",
      "inputs":  [{"name":"x","ty":{"kind":"Bool"}}],
      "outputs": [{"name":"y","ty":{"kind":"Bool"}}],
      "equations": [{"lhs":["y"],"rhs":{"expr":"Var","name":"x"}}]
    }]
  }]
}
"#,
    );

    let project = load_project(&dir).expect("dir loads");
    let _ = std::fs::remove_dir_all(&dir);

    let names: Vec<&str> = project.all_nodes().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"A"));
    assert!(names.contains(&"B"));
}

#[test]
fn loader_includes_propagate_main_from_child_when_parent_is_silent() {
    let dir = make_tempdir();
    write(
        dir.join("child.yaml"),
        r#"
name: child
main: ChildMain
packages:
  - name: p
    nodes:
      - name: ChildMain
        kind: Operator
        inputs: []
        outputs: [{ name: y, ty: { kind: Bool } }]
        equations: [{ lhs: [y], rhs: { expr: Const, lit: { lit: Bool, value: true } } }]
"#,
    );
    write(
        dir.join("root.yaml"),
        r#"
name: root
includes: [child.yaml]
packages: []
"#,
    );
    let project = load_project(&dir.join("root.yaml")).expect("loads");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(project.main.as_deref(), Some("ChildMain"));
}

#[test]
fn cyclic_includes_are_detected() {
    let dir = make_tempdir();
    write(dir.join("a.yaml"), "name: a\nincludes: [b.yaml]\n");
    write(dir.join("b.yaml"), "name: b\nincludes: [a.yaml]\n");
    let result = load_project(&dir.join("a.yaml"));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(matches!(
        result,
        Err(ol_ir::loader::LoadError::CyclicInclude(_))
    ));
}

#[test]
fn duplicate_node_across_files_surfaces_via_typecheck() {
    let dir = make_tempdir();
    let common = r#"
        - name: Dup
          kind: Function
          inputs:  [{ name: x, ty: { kind: Bool } }]
          outputs: [{ name: y, ty: { kind: Bool } }]
          equations: [{ lhs: [y], rhs: { expr: Var, name: x } }]
    "#;
    let a = format!("name: a\npackages:\n  - name: lib\n    nodes:\n{common}");
    let b = format!("name: b\npackages:\n  - name: lib\n    nodes:\n{common}");
    write(dir.join("a.yaml"), &a);
    write(dir.join("b.yaml"), &b);
    let project = load_project(&dir).expect("loads even with dups");
    let report = ol_typecheck::check_project(&project);
    let codes: Vec<_> = report.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"E0001"), "got {codes:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn independent_projects_share_a_transitive_dependency_once() {
    let dir = make_tempdir();
    write(dir.join("types.json"), r#"{"name":"signals","packages":[{"name":"signals","types":[{"body":{"kind":"Alias","name":"Sample","target":{"kind":"Int32"}}}]}]}"#);
    write(dir.join("a.yaml"), "name: a\nmain: A\nincludes: [types.json]\n");
    write(dir.join("b.yaml"), "name: b\nmain: B\nincludes: [types.json]\n");
    write(dir.join("root.yaml"), "name: root\nmain: Root\nincludes: [a.yaml, b.yaml]\n");
    let project = load_project(&dir.join("root.yaml")).expect("shared dependency is not a cycle");
    assert_eq!(project.main.as_deref(), Some("Root"));
    assert_eq!(project.packages.len(), 1);
    assert_eq!(project.packages[0].types.len(), 1, "one canonical dependency identity");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn repeated_canonical_file_reference_is_idempotent() {
    let dir = make_tempdir();
    write(dir.join("shared.yaml"), "name: shared\npackages: [{name: shared}]\n");
    write(dir.join("root.yaml"), "name: root\nincludes: [shared.yaml, ./shared.yaml]\n");
    let project = load_project(&dir.join("root.yaml")).expect("repeated reference is not a cycle");
    assert_eq!(project.packages.len(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn directory_scan_deduplicates_an_explicitly_included_sibling() {
    let dir = make_tempdir();
    write(dir.join("a.yaml"), "name: a\nincludes: [b.yaml]\n");
    write(dir.join("b.yaml"), "name: b\npackages: [{name: shared}]\n");
    let project = load_project(&dir).expect("directory members can include one another");
    assert_eq!(project.packages.len(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn shared_dependency_does_not_hide_a_real_cycle() {
    let dir = make_tempdir();
    write(dir.join("shared.yaml"), "name: shared\n");
    write(dir.join("a.yaml"), "name: a\nincludes: [shared.yaml, b.yaml]\n");
    write(dir.join("b.yaml"), "name: b\nincludes: [shared.yaml, a.yaml]\n");
    assert!(matches!(load_project(&dir.join("a.yaml")), Err(ol_ir::loader::LoadError::CyclicInclude(_))));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn independent_type_definitions_cannot_silently_replace_each_other() {
    let dir = make_tempdir();
    write(dir.join("a.json"), r#"{"name":"a","packages":[{"name":"a","types":[{"body":{"kind":"Alias","name":"Sample","target":{"kind":"Int32"}}}]}]}"#);
    write(dir.join("b.json"), r#"{"name":"b","packages":[{"name":"b","types":[{"body":{"kind":"Record","name":"Sample","fields":[{"name":"valid","ty":{"kind":"Bool"}}]}}]}]}"#);
    let project = load_project(&dir).unwrap();
    let report = ol_typecheck::check_project(&project);
    assert!(report.diagnostics.iter().any(|d| d.code == "E0006"), "duplicate type names must be rejected: {:?}", report.diagnostics);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn independent_enums_cannot_silently_rebind_shared_variants() {
    let enum_package = |package: &str, ty: &str| serde_json::json!({
        "name":package, "types":[{"body":{"kind":"Enum", "name":ty, "variants":["Idle"]}}]
    });
    for packages in [
        vec![enum_package("flight", "FlightStatus"), enum_package("payload", "PayloadStatus")],
        vec![enum_package("payload", "PayloadStatus"), enum_package("flight", "FlightStatus")],
    ] {
        let project: ol_ir::Project = serde_json::from_value(serde_json::json!({"name":"root", "packages":packages})).unwrap();
        let report = ol_typecheck::check_project(&project);
        assert!(report.diagnostics.iter().any(|d| d.code == "E0007"), "ambiguous variant must fail in either include order");
    }
}
