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
