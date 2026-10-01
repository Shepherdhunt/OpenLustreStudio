//! Native reusable-project resolver regressions. These are synthetic models,
//! not flight-control laws, aircraft timing, or hardware acceptance tests.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use ol_ir::native_project::{load_native_project, snapshot_native_project};
use ol_ir::{BinOp, Expr, Project};
use ol_sim::{Sim, Value};
use serde_json::{json, Value as Json};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        loop {
            let p = std::env::temp_dir().join(format!(
                "openlustre_native_{}_{}", std::process::id(), NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&p) {
                Ok(()) => return Self(p),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("create unique test directory: {e}"),
            }
        }
    }
    fn path(&self) -> &Path { &self.0 }
}
impl Drop for TempDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn write_json(path: &Path, value: &Json) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn read_json(path: &Path) -> Json {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn int_ty() -> Json { json!({"kind":"Int32"}) }
fn named(name: &str) -> Json { json!({"kind":"Named", "name":name}) }
fn port(name: &str, ty: Json) -> Json { json!({"name":name,"ty":ty}) }
fn expr(expr: Expr) -> Json { serde_json::to_value(expr).unwrap() }
fn node(name: &str, inputs: Vec<Json>, outputs: Vec<Json>, equations: Vec<(&str, Expr)>) -> Json {
    json!({"name":name,"kind":"Operator","inputs":inputs,"outputs":outputs,
        "equations":equations.into_iter().map(|(lhs,rhs)| json!({"lhs":[lhs],"rhs":expr(rhs)})).collect::<Vec<_>>()})
}
fn simple_node(name: &str, rhs: Expr) -> Json {
    node(name, vec![port("x", int_ty())], vec![port("y", int_ty())], vec![("y", rhs)])
}
fn model(nodes: Vec<Json>) -> Json {
    json!({"name":"synthetic_test_only","packages":[{"name":"local","nodes":nodes}]})
}
fn exports(nodes: &[&str], types: &[&str], constants: &[&str], contracts: &[&str]) -> Json {
    json!({"nodes":nodes,"types":types,"constants":constants,"contracts":contracts})
}
fn project(dir: &Path, id: &str, source: Json, entry: &str, public: Json, dependencies: Vec<Json>) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    write_json(&dir.join("model.json"), &source);
    let p = dir.join("project.olproj");
    write_json(&p, &json!({"format":"openlustre.project/v1","project_id":id,
        "model":"model.json","entrypoint":entry,"exports":public,"dependencies":dependencies}));
    p
}
fn dep(alias: &str, relative: &str, path: &Path) -> Json {
    json!({"alias":alias,"project":relative,"project_id":read_json(path)["project_id"],
        "snapshot_sha256":snapshot_native_project(path).expect("dependency snapshot")})
}
fn error(path: &Path, contains: &str) -> String {
    let err = load_native_project(path).expect_err("invalid project must be rejected");
    assert!(err.to_lowercase().contains(&contains.to_lowercase()), "expected {contains:?}, got {err}");
    err
}
fn checked(path: &Path) -> Project {
    let p = load_native_project(path).expect("native project loads");
    let r = ol_typecheck::check_project(&p);
    assert!(!r.has_errors(), "typecheck: {:?}", r.diagnostics);
    let r = ol_contract_check::check_project(&p);
    assert!(!r.has_errors(), "contract check: {:?}", r.diagnostics);
    p
}
fn root(dir: &Path, dependencies: Vec<Json>, rhs: Expr) -> PathBuf {
    project(dir, "synthetic.root", model(vec![simple_node("Root", rhs)]), "Root",
        exports(&["Root"], &[], &[], &[]), dependencies)
}
fn library(dir: &Path, id: &str) -> PathBuf {
    project(dir, id, model(vec![simple_node("Pass", Expr::var("x"))]), "Pass",
        exports(&["Pass"], &[], &[], &[]), vec![])
}
fn run_one(p: &Project, x: i64) -> i64 {
    let mut sim = Sim::new(p, p.main.as_deref().unwrap()).unwrap();
    sim.step(&BTreeMap::from([("x".into(), Value::Int(x))])).unwrap()["y"].as_int().unwrap()
}

fn collision_library(dir: &Path, id: &str, increment: i64) -> PathBuf {
    let mut m = model(vec![simple_node("Step", Expr::bin(BinOp::Add, Expr::var("x"), Expr::var("LIMIT"))),
        node("Make", vec![port("x", int_ty())], vec![port("sample", named("Sample")), port("state", named("Status"))],
            vec![("sample", Expr::Struct {ty:"Sample".into(),fields:vec![ol_ir::FieldInit {field:"value".into(),value:Expr::var("x")}] }),
                 ("state", Expr::var("Status::Idle"))])]);
    let package = &mut m["packages"][0];
    package["types"] = json!([
        {"body":{"kind":"Record","name":"Sample","fields":[{"name":"value","ty":{"kind":"Int32"}}]}},
        {"body":{"kind":"Enum","name":"Status","variants":["Idle","Active"]}}
    ]);
    package["constants"] = json!([{"name":"LIMIT","ty":int_ty(),"value":expr(Expr::int_lit(increment))}]);
    package["contracts"] = json!([{"name":"Bounds","inputs":[port("x",int_ty())],"outputs":[port("y",int_ty())],
        "guarantees":[{"name":"offset","expr":expr(Expr::bin(BinOp::Eq,Expr::var("y"),Expr::bin(BinOp::Add,Expr::var("x"),Expr::var("LIMIT"))))}]}]);
    package["nodes"][0]["contract"] = json!("Bounds");
    project(dir,id,m,"Step",exports(&["Step","Make"],&["Sample","Status"],&["LIMIT"],&["Bounds"]),vec![])
}

#[test]
fn duplicate_local_names_are_collision_safe_and_have_source_provenance() {
    let tmp = TempDir::new();
    let a = collision_library(&tmp.path().join("a"), "synthetic.a", 2);
    let b = collision_library(&tmp.path().join("b"), "synthetic.b", 17);
    let mut m = model(vec![node("Root",vec![port("x",int_ty())],vec![port("a",int_ty()),port("b",int_ty()),port("limit",int_ty()),port("idle",json!({"kind":"Bool"}))],vec![
        ("a",Expr::call("flight::Step",vec![Expr::var("x")])),
        ("b",Expr::call("payload::Step",vec![Expr::var("x")])),
        ("limit",Expr::var("payload::LIMIT")),
        ("idle",Expr::bin(BinOp::Neq,Expr::var("flight::Status::Idle"),Expr::var("flight::Status::Active")))])]);
    m["packages"][0]["contracts"] = json!([{"name":"RootContract","inputs":[port("x",int_ty())],
        "outputs":[port("a",int_ty()),port("b",int_ty()),port("limit",int_ty()),port("idle",json!({"kind":"Bool"}))],
        "imports":[{"contract":"flight::Bounds","input_map":[["x",expr(Expr::var("x"))]],"output_map":[["y","a"]]}]}]);
    m["packages"][0]["nodes"][0]["contract"] = json!("RootContract");
    let r = project(&tmp.path().join("root"),"synthetic.root",m,"Root",exports(&["Root"],&[],&[],&[]),vec![
        dep("flight","../a/project.olproj",&a),dep("payload","../b/project.olproj",&b)]);
    let p = checked(&r);
    assert_ne!(p.find_node("flight::Step").unwrap().name,p.find_node("payload::Step").unwrap().name);
    assert_eq!(p.find_node("Root").unwrap().name,p.main.as_deref().unwrap());
    let metadata = p.resolution.as_ref().expect("source provenance");
    assert_eq!(metadata.root_project_id,"synthetic.root");
    assert_eq!(metadata.projects.len(),3);
    let names = metadata.symbols.iter().map(|s| s.resolved_name.as_str()).collect::<BTreeSet<_>>();
    assert_eq!(names.len(),metadata.symbols.len(),"backend names are injective across symbol categories");
    assert_eq!(metadata.symbols.iter().filter(|s| s.local_name == "Sample").count(),2);
    for symbol in &metadata.symbols {
        let source=metadata.projects.iter().find(|s|s.project_id==symbol.project_id).unwrap();
        assert!(Path::new(&source.manifest_path).parent().unwrap().join(&symbol.source_path).exists(),
            "source provenance must locate authored bytes: {symbol:?}");
    }
    let mut sim = Sim::new(&p,"Root").unwrap();
    let out = sim.step(&BTreeMap::from([("x".into(),Value::Int(5))])).unwrap();
    assert_eq!(out,BTreeMap::from([("a".into(),Value::Int(7)),("b".into(),Value::Int(22)),("limit".into(),Value::Int(17)),("idle".into(),Value::Bool(true))]));
    let bundle = ol_clite_emit::emit_project(&p);
    compile_c(&tmp.path().join("cc"),&bundle.header,&bundle.source,&ol_clite_emit::harness::emit_csv_driver(p.find_node("Root").unwrap()),false);
}

#[test]
fn two_aliases_to_one_library_preserve_one_identity() {
    let tmp=TempDir::new(); let lib=library(&tmp.path().join("lib"),"synthetic.lib");
    let r=root(&tmp.path().join("root"),vec![dep("first","../lib/project.olproj",&lib),dep("second","../lib/project.olproj",&lib)],
        Expr::call("first::Pass",vec![Expr::call("second::Pass",vec![Expr::var("x")])]));
    let p=checked(&r);
    assert_eq!(p.resolution.as_ref().unwrap().projects.len(),2);
    assert_eq!(p.find_node("first::Pass"),p.find_node("second::Pass"));
    assert_eq!(run_one(&p,41),41);
}

fn diamond(tmp:&TempDir, mismatch:bool) -> PathBuf {
    let mut source=model(vec![simple_node("Marker",Expr::var("x"))]);
    source["packages"][0]["types"]=json!([{"body":{"kind":"Record","name":"Sample","fields":[{"name":"value","ty":int_ty()}]}}]);
    let common=project(&tmp.path().join("common"),"synthetic.interfaces",source,"Marker",exports(&["Marker"],&["Sample"],&[],&[]),vec![]);
    std::fs::create_dir_all(tmp.path().join("copy")).unwrap();
    for file in ["project.olproj","model.json"] {std::fs::copy(tmp.path().join("common").join(file),tmp.path().join("copy").join(file)).unwrap();}
    let copy=tmp.path().join("copy/project.olproj");
    if mismatch {let mut m=read_json(&copy);m["project_id"]=json!("synthetic.other_interfaces");write_json(&copy,&m);}
    let pass=|qual:&str| model(vec![node("Pass",vec![port("sample",named(&format!("{qual}::Sample")))],vec![port("sample_out",named(&format!("{qual}::Sample")))],vec![("sample_out",Expr::var("sample"))])]);
    let a=project(&tmp.path().join("a"),"synthetic.a",pass("schema"),"Pass",exports(&["Pass"],&[],&[],&[]),vec![dep("schema","../common/project.olproj",&common)]);
    let b=project(&tmp.path().join("b"),"synthetic.b",pass("types"),"Pass",exports(&["Pass"],&[],&[],&[]),vec![dep("types","../copy/project.olproj",&copy)]);
    let sample=Expr::Struct{ty:"interfaces::Sample".into(),fields:vec![ol_ir::FieldInit{field:"value".into(),value:Expr::var("x")}]};
    let through=Expr::call("right::Pass",vec![Expr::call("left::Pass",vec![sample])]);
    root(&tmp.path().join("root"),vec![dep("left","../a/project.olproj",&a),dep("right","../b/project.olproj",&b),dep("interfaces","../common/project.olproj",&common)],
        Expr::Field{base:Box::new(through),field:"value".into()})
}

#[test]
fn diamond_shared_record_identity_survives_aliases_and_identical_source_copies() {
    let tmp=TempDir::new(); let r=diamond(&tmp,false); let p=checked(&r);
    assert_eq!(p.resolution.as_ref().unwrap().projects.len(),4,"one canonical interfaces identity");
    assert_eq!(p.resolution.as_ref().unwrap().symbols.iter().filter(|s|s.local_name=="Sample").count(),1);
    assert_eq!(p.find_node("left::Pass").unwrap().inputs[0].ty,p.find_node("right::Pass").unwrap().inputs[0].ty);
    assert_eq!(run_one(&p,-13),-13);
    let bundle=ol_clite_emit::emit_project(&p);
    let exe=compile_c(&tmp.path().join("cc"),&bundle.header,&bundle.source,&ol_clite_emit::harness::emit_csv_driver(p.find_node("Root").unwrap()),false);
    assert_eq!(run_c(&exe,"x\n-13\n"),"cycle,y\n0,-13\n");
}

#[test]
fn independent_same_layout_records_remain_nominally_distinct() {
    let tmp=TempDir::new();let r=diamond(&tmp,true);let p=load_native_project(&r).unwrap();
    assert_ne!(p.find_node("left::Pass").unwrap().inputs[0].ty,p.find_node("right::Pass").unwrap().inputs[0].ty);
    assert!(ol_typecheck::check_project(&p).has_errors(),"identical field layouts do not erase project type ownership");
}

#[test]
fn conflicting_snapshots_of_one_project_id_are_rejected() {
    let tmp=TempDir::new();let a=library(&tmp.path().join("a"),"synthetic.same");
    let b=project(&tmp.path().join("b"),"synthetic.same",model(vec![simple_node("Pass",Expr::int_lit(9))]),"Pass",exports(&["Pass"],&[],&[],&[]),vec![]);
    let r=root(&tmp.path().join("root"),vec![dep("one","../a/project.olproj",&a),dep("two","../b/project.olproj",&b)],Expr::var("x"));
    error(&r,"conflicting snapshots");
}

#[test]
fn modified_model_invalidates_exact_dependency_pin() {
    let tmp=TempDir::new();let lib=library(&tmp.path().join("lib"),"synthetic.lib");
    let r=root(&tmp.path().join("root"),vec![dep("lib","../lib/project.olproj",&lib)],Expr::var("x"));
    let m=lib.parent().unwrap().join("model.json");
    std::fs::write(&m,format!("{}\n",std::fs::read_to_string(&m).unwrap())).unwrap();
    error(&r,"snapshot mismatch");
}

#[test]
fn modified_manifest_invalidates_exact_dependency_pin() {
    let tmp=TempDir::new();let lib=library(&tmp.path().join("lib"),"synthetic.lib");
    let r=root(&tmp.path().join("root"),vec![dep("lib","../lib/project.olproj",&lib)],Expr::var("x"));
    let mut m=read_json(&lib);m["exports"]["nodes"]=json!([]);write_json(&lib,&m);
    error(&r,"snapshot mismatch");
}

#[test]
fn missing_dependency_manifest_is_rejected() {
    let tmp=TempDir::new();let lib=library(&tmp.path().join("lib"),"synthetic.lib");
    let r=root(&tmp.path().join("root"),vec![dep("lib","../lib/project.olproj",&lib)],Expr::var("x"));
    std::fs::remove_file(&lib).unwrap();assert!(load_native_project(&r).is_err());
}

#[test]
fn missing_dependency_model_is_rejected() {
    let tmp=TempDir::new();let lib=library(&tmp.path().join("lib"),"synthetic.lib");
    let r=root(&tmp.path().join("root"),vec![dep("lib","../lib/project.olproj",&lib)],Expr::var("x"));
    std::fs::remove_file(lib.parent().unwrap().join("model.json")).unwrap();assert!(load_native_project(&r).is_err());
}

#[test]
fn dependency_project_id_must_match_declared_identity() {
    let tmp=TempDir::new();let lib=library(&tmp.path().join("lib"),"synthetic.lib");let mut d=dep("lib","../lib/project.olproj",&lib);d["project_id"]=json!("synthetic.wrong");
    let r=root(&tmp.path().join("root"),vec![d],Expr::var("x"));error(&r,"identity mismatch");
    assert!(snapshot_native_project(&r).unwrap_err().contains("identity mismatch"));
}

#[test]
fn dependency_snapshot_must_be_a_full_sha256() {
    let tmp=TempDir::new();let lib=library(&tmp.path().join("lib"),"synthetic.lib");
    for invalid in ["", "abcd", &"z".repeat(64)] {
        let mut d=dep("lib","../lib/project.olproj",&lib);d["snapshot_sha256"]=json!(invalid);
        let r=root(&tmp.path().join("root"),vec![d],Expr::var("x"));assert!(load_native_project(&r).is_err());
    }
}

#[test]
fn native_dependency_cycles_are_rejected_before_pin_validation() {
    let tmp=TempDir::new();let a=library(&tmp.path().join("a"),"synthetic.a");let b=library(&tmp.path().join("b"),"synthetic.b");
    for (own,alias,relative,id) in [(&a,"b","../b/project.olproj","synthetic.b"),(&b,"a","../a/project.olproj","synthetic.a")] {
        let mut m=read_json(own);m["dependencies"]=json!([{"alias":alias,"project":relative,"project_id":id,"snapshot_sha256":"0".repeat(64)}]);write_json(own,&m);
    }
    error(&a,"cyclic project dependency");assert!(snapshot_native_project(&a).is_err());
}

#[test]
fn duplicate_consumer_aliases_are_rejected() {
    let tmp=TempDir::new();let lib=library(&tmp.path().join("lib"),"synthetic.lib");
    let r=root(&tmp.path().join("root"),vec![dep("same","../lib/project.olproj",&lib),dep("same","../lib/project.olproj",&lib)],Expr::var("x"));
    error(&r,"alias");
}

#[test]
fn unknown_consumer_alias_is_rejected() {
    let tmp=TempDir::new();let r=root(tmp.path(),vec![],Expr::call("missing::Pass",vec![Expr::var("x")]));error(&r,"missing");
}

#[test]
fn dependency_transitive_alias_is_not_an_implicit_reexport() {
    let tmp=TempDir::new();let common=library(&tmp.path().join("common"),"synthetic.common");
    let a=project(&tmp.path().join("a"),"synthetic.a",model(vec![simple_node("Pass",Expr::call("inside::Pass",vec![Expr::var("x")]))]),"Pass",exports(&["Pass"],&[],&[],&[]),vec![dep("inside","../common/project.olproj",&common)]);
    let r=root(&tmp.path().join("root"),vec![dep("a","../a/project.olproj",&a)],Expr::call("a::inside::Pass",vec![Expr::var("x")]));
    assert!(load_native_project(&r).is_err(),"only consumer-owned direct dependency aliases are supported");
}

fn private_use(kind:&str) {
    let tmp=TempDir::new();let lib=collision_library(&tmp.path().join("lib"),"synthetic.lib",3);
    let mut manifest=read_json(&lib);manifest["exports"]=exports(&[],&[],&[],&[]);write_json(&lib,&manifest);
    let mut m=model(vec![simple_node("Root",Expr::var("x"))]);
    match kind {
        "node"=>m["packages"][0]["nodes"][0]["equations"][0]["rhs"]=expr(Expr::call("lib::Step",vec![Expr::var("x")])),
        "type"=>m["packages"][0]["nodes"][0]["inputs"][0]["ty"]=named("lib::Sample"),
        "constant"=>m["packages"][0]["nodes"][0]["equations"][0]["rhs"]=expr(Expr::var("lib::LIMIT")),
        "enum"=>m["packages"][0]["nodes"][0]["equations"][0]["rhs"]=expr(Expr::var("lib::Status::Idle")),
        "contract"=>m["packages"][0]["nodes"][0]["contract"]=json!("lib::Bounds"),
        _=>unreachable!(),
    }
    let r=project(&tmp.path().join("root"),"synthetic.root",m,"Root",exports(&["Root"],&[],&[],&[]),vec![dep("lib","../lib/project.olproj",&lib)]);
    error(&r,"export");
}
#[test] fn private_node_cannot_be_called() {private_use("node");}
#[test] fn private_type_cannot_be_used() {private_use("type");}
#[test] fn private_constant_cannot_be_read() {private_use("constant");}
#[test] fn private_enum_variant_requires_its_type_export() {private_use("enum");}
#[test] fn private_contract_cannot_be_attached() {private_use("contract");}

#[test]
fn exports_must_name_own_existing_declarations() {
    let tmp=TempDir::new();let lib=library(tmp.path(),"synthetic.lib");
    for kind in ["nodes","types","constants","contracts"] {
        let mut m=read_json(&lib);m["exports"][kind]=json!(["Absent"]);write_json(&lib,&m);error(&lib,"Absent");
        m["exports"][kind]=json!([]);write_json(&lib,&m);
    }
}

#[test]
fn entrypoint_is_required_and_must_name_an_own_node() {
    let tmp=TempDir::new();let lib=library(&tmp.path().join("lib"),"synthetic.lib");
    let r=root(&tmp.path().join("root"),vec![dep("lib","../lib/project.olproj",&lib)],Expr::var("x"));
    let mut m=read_json(&r);
    for invalid in ["Absent","lib::Pass",""] {m["entrypoint"]=json!(invalid);write_json(&r,&m);assert!(load_native_project(&r).is_err());}
    m.as_object_mut().unwrap().remove("entrypoint");write_json(&r,&m);assert!(load_native_project(&r).is_err());
}

#[test]
fn native_entrypoint_overrides_legacy_model_main_and_never_inherits_dependency_main() {
    let tmp=TempDir::new();let lib=library(&tmp.path().join("lib"),"synthetic.lib");
    let r=root(&tmp.path().join("root"),vec![dep("lib","../lib/project.olproj",&lib)],Expr::var("x"));
    let model=r.parent().unwrap().join("model.json");let mut m=read_json(&model);m["main"]=json!("UnknownLegacyMain");write_json(&model,&m);
    let p=checked(&r);assert_eq!(p.main.as_deref(),Some(p.find_node("Root").unwrap().name.as_str()));
    assert_ne!(p.main.as_deref(),Some(p.find_node("lib::Pass").unwrap().name.as_str()));
    assert_eq!(ol_ir::load_project(&r).unwrap(),p,"generic loader recognizes .olproj");
}

#[test]
fn legacy_includes_keep_existing_behavior_and_have_no_native_metadata() {
    let tmp=TempDir::new();write_json(&tmp.path().join("lib.json"),&model(vec![simple_node("Pass",Expr::var("x"))]));
    let mut m=model(vec![simple_node("Root",Expr::call("Pass",vec![Expr::var("x")]))]);m["main"]=json!("Root");m["includes"]=json!(["lib.json"]);write_json(&tmp.path().join("root.json"),&m);
    let p=ol_ir::load_project(&tmp.path().join("root.json")).unwrap();assert!(p.resolution.is_none());assert_eq!(p.main.as_deref(),Some("Root"));assert_eq!(run_one(&p,7),7);
}

#[test]
fn same_project_model_fragments_are_hashed_and_resolved_without_copying() {
    let tmp=TempDir::new();let lib=library(&tmp.path().join("lib"),"synthetic.lib");
    let fragment=lib.parent().unwrap().join("fragment.json");write_json(&fragment,&model(vec![simple_node("Extra",Expr::var("x"))]));
    let path=lib.parent().unwrap().join("model.json");let mut m=read_json(&path);m["includes"]=json!(["fragment.json"]);write_json(&path,&m);
    let mut manifest=read_json(&lib);manifest["exports"]["nodes"]=json!(["Pass","Extra"]);write_json(&lib,&manifest);
    let r=root(&tmp.path().join("root"),vec![dep("lib","../lib/project.olproj",&lib)],Expr::call("lib::Extra",vec![Expr::var("x")]));
    let p=checked(&r);assert_eq!(run_one(&p,8),8);
    assert_eq!(p.resolution.as_ref().unwrap().projects.iter().find(|s|s.project_id=="synthetic.lib").unwrap().model_files.len(),2);
    std::fs::write(&fragment,format!("{}\n",std::fs::read_to_string(&fragment).unwrap())).unwrap();error(&r,"snapshot mismatch");
}

#[test]
fn same_project_include_cycles_are_rejected() {
    let tmp=TempDir::new();let lib=library(tmp.path(),"synthetic.lib");let path=tmp.path().join("model.json");let mut m=read_json(&path);m["includes"]=json!(["fragment.json"]);write_json(&path,&m);
    let mut fragment=model(vec![]);fragment["includes"]=json!(["model.json"]);write_json(&tmp.path().join("fragment.json"),&fragment);error(&lib,"cyclic model include");
}

#[test]
fn native_own_model_paths_cannot_escape_project_boundary() {
    let tmp=TempDir::new();let lib=library(&tmp.path().join("lib"),"synthetic.lib");
    write_json(&tmp.path().join("outside.json"),&model(vec![simple_node("Pass",Expr::var("x"))]));
    let mut m=read_json(&lib);m["model"]=json!("../outside.json");write_json(&lib,&m);error(&lib,"outside project root");
    m["model"]=json!(tmp.path().join("outside.json").to_str().unwrap());write_json(&lib,&m);error(&lib,"must be relative");
}

#[test]
fn native_unknown_manifest_fields_are_rejected_instead_of_silently_dropped() {
    let tmp=TempDir::new();let lib=library(tmp.path(),"synthetic.lib");let mut m=read_json(&lib);m["remote_registry"]=json!("unsupported");write_json(&lib,&m);error(&lib,"unknown field");
}

#[test]
fn snapshot_digest_matches_independent_python_hashlib_vector() {
    let tmp=TempDir::new();
    // Expected digest was produced independently with Python hashlib and
    // struct.pack('>Q', len(frame)); it does not call the Rust implementation.
    let source="{\"name\":\"digest\",\"packages\":[{\"name\":\"p\",\"nodes\":[{\"name\":\"Pass\",\"kind\":\"Function\",\"inputs\":[{\"name\":\"x\",\"ty\":{\"kind\":\"Int32\"}}],\"outputs\":[{\"name\":\"y\",\"ty\":{\"kind\":\"Int32\"}}],\"equations\":[{\"lhs\":[\"y\"],\"rhs\":{\"expr\":\"Var\",\"name\":\"x\"}}]}]}]}\n";
    std::fs::write(tmp.path().join("model.json"),source).unwrap();
    let manifest=tmp.path().join("project.olproj");
    write_json(&manifest,&json!({"format":"openlustre.project/v1","project_id":"synthetic.digest","model":"model.json","entrypoint":"Pass","dependencies":[],"exports":exports(&["Pass"],&[],&[],&[])}));
    assert_eq!(snapshot_native_project(&manifest).unwrap(),"704c47b53b891fa8b0fe4e40727eba787534ac3e8ea7867187c4c96687a01c1c");
    let canonical=snapshot_native_project(&manifest).unwrap();
    let m=read_json(&manifest);std::fs::write(&manifest,serde_json::to_vec(&m).unwrap()).unwrap();
    assert_eq!(snapshot_native_project(&manifest).unwrap(),canonical,"manifest whitespace and key order are canonicalized");
}

#[test]
fn directory_relocation_and_consumer_alias_change_preserve_library_backend_identity() {
    let tmp=TempDir::new();let lib=library(&tmp.path().join("original"),"synthetic.relocatable");let original_hash=snapshot_native_project(&lib).unwrap();
    std::fs::create_dir_all(tmp.path().join("moved")).unwrap();
    for file in ["project.olproj","model.json"] {std::fs::copy(tmp.path().join("original").join(file),tmp.path().join("moved").join(file)).unwrap();}
    let moved=tmp.path().join("moved/project.olproj");assert_eq!(snapshot_native_project(&moved).unwrap(),original_hash);
    let r=root(&tmp.path().join("root"),vec![dep("before","../original/project.olproj",&lib)],Expr::call("before::Pass",vec![Expr::var("x")]));
    let first=checked(&r).find_node("before::Pass").unwrap().name.clone();
    let r=root(&tmp.path().join("root"),vec![dep("after","../moved/project.olproj",&moved)],Expr::call("after::Pass",vec![Expr::var("x")]));
    let second=checked(&r).find_node("after::Pass").unwrap().name.clone();assert_eq!(first,second);
}

#[test]
fn modified_second_diamond_edge_is_verified_even_after_identity_was_seen() {
    let tmp=TempDir::new();let r=diamond(&tmp,false);
    let copy=tmp.path().join("copy/model.json");std::fs::write(&copy,format!("{}\n",std::fs::read_to_string(&copy).unwrap())).unwrap();
    let err=load_native_project(&r).expect_err("a modified duplicate source cannot reuse the already-seen identity");
    assert!(err.contains("snapshot mismatch") || err.contains("conflicting snapshots"),"{err}");
}

#[test]
fn native_dependency_manifest_cannot_hide_inside_legacy_includes() {
    let tmp=TempDir::new();let lib=library(tmp.path(),"synthetic.lib");
    let path=tmp.path().join("model.json");let mut m=read_json(&path);m["includes"]=json!(["project.olproj"]);write_json(&path,&m);assert!(load_native_project(&lib).is_err());
}

#[test]
fn exported_root_selection_does_not_expose_private_dependency_backend_names() {
    let tmp=TempDir::new();let lib=stateful_library(&tmp.path().join("lib"),"synthetic.lib");
    let r=root(&tmp.path().join("root"),vec![dep("lib","../lib/project.olproj",&lib)],Expr::call("lib::Envelope",vec![Expr::var("x")]));
    let p=checked(&r);let hidden=p.resolution.as_ref().unwrap().symbols.iter().find(|s|s.project_id=="synthetic.lib"&&s.local_name=="Cell").unwrap();
    assert!(p.find_node(&hidden.resolved_name).is_some(),"internal compiler lookup retains callees");
    assert!(p.selected_node_name(&hidden.resolved_name).is_none(),"backend spelling cannot bypass exports");
    assert!(p.selected_node_name("lib::Cell").is_none());
    assert_eq!(p.selected_node_name("lib::Envelope"),Some(p.find_node("lib::Envelope").unwrap().name.as_str()));
    assert_eq!(p.selected_node_name("Root"),p.main.as_deref());
}

#[test]
fn cross_project_and_forward_local_constants_are_topologically_ordered_in_ir_and_c() {
    let tmp=TempDir::new();let lib=library(&tmp.path().join("lib"),"z-provider");
    let model_path=lib.parent().unwrap().join("model.json");let mut source=read_json(&model_path);
    source["packages"][0]["constants"]=json!([{"name":"LIMIT","ty":int_ty(),"value":expr(Expr::int_lit(8))}]);write_json(&model_path,&source);
    let mut manifest=read_json(&lib);manifest["exports"]["constants"]=json!(["LIMIT"]);write_json(&lib,&manifest);
    let mut m=model(vec![simple_node("Root",Expr::bin(BinOp::Add,Expr::var("x"),Expr::var("FIRST")))]);
    m["packages"][0]["constants"]=json!([
        {"name":"FIRST","ty":int_ty(),"value":expr(Expr::bin(BinOp::Add,Expr::var("SECOND"),Expr::int_lit(1)))},
        {"name":"SECOND","ty":int_ty(),"value":expr(Expr::var("lib::LIMIT"))}
    ]);
    let r=project(&tmp.path().join("root"),"a-consumer",m,"Root",exports(&["Root"],&[],&[],&[]),vec![dep("lib","../lib/project.olproj",&lib)]);
    let p=checked(&r);assert_eq!(run_one(&p,3),12,"8 + 1 must not become the default zero through declaration order");
    let bundle=ol_clite_emit::emit_project(&p);let executable=compile_c(&tmp.path().join("cc"),&bundle.header,&bundle.source,&ol_clite_emit::harness::emit_csv_driver(p.find_node("Root").unwrap()),false);
    assert_eq!(run_c(&executable,"x\n3\n"),"cycle,y\n0,12\n");
}

#[test]
fn cyclic_constant_initializers_are_rejected_at_native_resolution() {
    let tmp=TempDir::new();let mut m=model(vec![simple_node("Root",Expr::var("x"))]);m["packages"][0]["constants"]=json!([
        {"name":"ONE","ty":int_ty(),"value":expr(Expr::var("TWO"))},
        {"name":"TWO","ty":int_ty(),"value":expr(Expr::var("ONE"))}
    ]);
    let r=project(tmp.path(),"synthetic.cycle",m,"Root",exports(&["Root"],&[],&[],&[]),vec![]);error(&r,"cyclic");
}

#[test]
fn composed_reserved_output_names_project_the_escaped_c_field() {
    let tmp=TempDir::new();
    let lib=project(&tmp.path().join("lib"),"synthetic.reserved",model(vec![
        node("Pass",vec![port("x",int_ty())],vec![port("out",int_ty())],vec![("out",Expr::var("x"))])
    ]),"Pass",exports(&["Pass"],&[],&[],&[]),vec![]);
    let r=root(&tmp.path().join("root"),vec![dep("lib","../lib/project.olproj",&lib)],Expr::call("lib::Pass",vec![Expr::var("x")]));
    let p=checked(&r);
    assert_eq!(run_one(&p,12),12);
    let bundle=ol_clite_emit::emit_project(&p);
    let driver=ol_clite_emit::harness::emit_csv_driver(p.find_node("Root").unwrap());
    assert_c_oracle(&tmp.path().join("cc"),&bundle.header,&bundle.source,&driver,"x\n12\n","cycle,y\n0,12\n","reserved output projection");
}

#[test]
fn cross_project_and_forward_record_types_are_topologically_ordered_for_c() {
    let tmp=TempDir::new();let lib=library(&tmp.path().join("lib"),"z-provider");let model_path=lib.parent().unwrap().join("model.json");let mut source=read_json(&model_path);
    source["packages"][0]["types"]=json!([{"body":{"kind":"Record","name":"Box","fields":[port("value",int_ty())]}}]);write_json(&model_path,&source);
    let mut manifest=read_json(&lib);manifest["exports"]["types"]=json!(["Box"]);write_json(&lib,&manifest);
    let mut m=model(vec![simple_node("Root",Expr::var("x"))]);m["packages"][0]["types"]=json!([
        {"body":{"kind":"Record","name":"Outer","fields":[port("nested",named("Wrap"))]}},
        {"body":{"kind":"Record","name":"Wrap","fields":[port("nested",json!({"kind":"Array","len":2,"elem":named("lib::Box")}))]}}
    ]);
    let r=project(&tmp.path().join("root"),"a-consumer",m,"Root",exports(&["Root"],&[],&[],&[]),vec![dep("lib","../lib/project.olproj",&lib)]);let p=checked(&r);
    let bundle=ol_clite_emit::emit_project(&p);compile_c(&tmp.path().join("cc"),&bundle.header,&bundle.source,&ol_clite_emit::harness::emit_csv_driver(p.find_node("Root").unwrap()),false);
}

#[test]
fn cyclic_record_types_are_rejected_at_native_resolution() {
    let tmp=TempDir::new();let mut m=model(vec![simple_node("Root",Expr::var("x"))]);m["packages"][0]["types"]=json!([
        {"body":{"kind":"Record","name":"One","fields":[port("nested",named("Two"))]}},
        {"body":{"kind":"Record","name":"Two","fields":[port("nested",named("One"))]}}
    ]);
    let r=project(tmp.path(),"synthetic.cycle",m,"Root",exports(&["Root"],&[],&[],&[]),vec![]);error(&r,"cyclic");
}

#[test]
fn native_local_owned_fsm_lowering_preserves_origins_and_generated_c_semantics() {
    let tmp=TempDir::new();let mut m=model(vec![node("Root",vec![port("advance",json!({"kind":"Bool"}))],vec![port("value",int_ty())],vec![])]);
    m["packages"][0]["state_machines"]=json!([{"name":"Mode","owner":"Root","inputs":[port("advance",json!({"kind":"Bool"}))],"outputs":[port("value",int_ty())],"initial_state":"Idle","states":[
        {"name":"Idle","equations":[{"lhs":["value"],"rhs":expr(Expr::int_lit(0))}],"transitions":[{"guard":expr(Expr::var("advance")),"target":"Active"}]},
        {"name":"Active","equations":[{"lhs":["value"],"rhs":expr(Expr::int_lit(7))}],"transitions":[{"guard":expr(Expr::var("advance")),"target":"Idle"}]}
    ]}]);
    let mut fragment=model(vec![]);fragment["packages"][0]["state_machines"]=m["packages"][0]["state_machines"].take();
    m["packages"][0].as_object_mut().unwrap().remove("state_machines");m["includes"]=json!(["fragment.json"]);
    let r=project(tmp.path(),"synthetic.fsm",m,"Root",exports(&["Root"],&[],&[],&[]),vec![]);write_json(&tmp.path().join("fragment.json"),&fragment);let p=checked(&r);
    assert!(p.packages.iter().all(|package|package.state_machines.is_empty()));assert_eq!(p.origins.len(),1);
    assert_eq!(p.origins[0].node,p.main.as_deref().unwrap());assert_eq!(p.origins[0].name,"Mode");
    assert!(p.resolution.as_ref().unwrap().symbols.iter().any(|s|s.local_name=="Mode_StateEnum"&&s.source_path=="fragment.json"));
    assert!(p.resolution.as_ref().unwrap().symbols.iter().any(|s|s.kind=="state_machine"&&s.local_name=="Mode"&&s.source_path=="fragment.json"));
    let csv="advance\nfalse\ntrue\nfalse\ntrue\nfalse\n";let expected="cycle,value\n0,0\n1,0\n2,7\n3,7\n4,0\n";
    assert_eq!(Sim::new(&p,"Root").unwrap().run_csv(csv).unwrap().to_csv(),expected);
    let bundle=ol_clite_emit::emit_project(&p);let executable=compile_c(&tmp.path().join("cc"),&bundle.header,&bundle.source,&ol_clite_emit::harness::emit_csv_driver(p.find_node("Root").unwrap()),false);assert_eq!(run_c(&executable,csv),expected);
}

fn activation_model(ty:&str,initial:Option<&str>)->Json {
    let mut m=model(vec![node("Root",vec![port("advance",json!({"kind":"Bool"}))],vec![port("state",named(ty)),port("active",json!({"kind":"Bool"}))],
        vec![("active",Expr::bin(BinOp::Eq,Expr::var("state"),Expr::var(&format!("{ty}::Active"))))])]);
    let mut last_args=vec![Expr::var("state")];if let Some(initial)=initial {last_args.push(Expr::var(initial));}
    m["packages"][0]["activations"]=json!([{"name":"Select","owner":"Root","outputs":[port("state",named(ty))],"branches":[
        {"name":"Set","condition":expr(Expr::var("advance")),"equations":[{"lhs":["state"],"rhs":expr(Expr::var(&format!("{ty}::Active")))}]}
    ],"else_equations":[{"lhs":["state"],"rhs":expr(Expr::call("last",last_args))}]}]);m
}

#[test]
fn native_local_enum_activation_default_is_lowered_and_has_origins() {
    let tmp=TempDir::new();let mut m=activation_model("Status",None);
    m["packages"][0]["types"]=json!([{"body":{"kind":"Enum","name":"Status","variants":["Idle","Active"]}}]);
    let mut fragment=model(vec![]);fragment["packages"][0]["activations"]=m["packages"][0]["activations"].take();
    m["packages"][0].as_object_mut().unwrap().remove("activations");m["includes"]=json!(["fragment.json"]);
    let r=project(tmp.path(),"synthetic.activation",m,"Root",exports(&["Root"],&[],&[],&[]),vec![]);write_json(&tmp.path().join("fragment.json"),&fragment);let p=checked(&r);
    assert!(p.packages.iter().all(|package|package.activations.is_empty()));assert_eq!(p.origins.len(),1);assert_eq!(p.origins[0].name,"Select");
    assert!(p.resolution.as_ref().unwrap().symbols.iter().any(|s|s.kind=="activation"&&s.local_name=="Select"&&s.source_path=="fragment.json"));
    let mut sim=Sim::new(&p,"Root").unwrap();for (advance,active) in [(false,false),(true,true),(false,true),(false,true)] {
        assert_eq!(sim.step(&BTreeMap::from([("advance".into(),Value::Bool(advance))])).unwrap()["active"],Value::Bool(active));
    }
}

#[test]
fn native_dependency_enum_activation_requires_explicit_last_initialization() {
    let tmp=TempDir::new();let lib=library(&tmp.path().join("lib"),"synthetic.enums");let model_path=lib.parent().unwrap().join("model.json");let mut m=read_json(&model_path);
    m["packages"][0]["types"]=json!([{"body":{"kind":"Enum","name":"Status","variants":["Idle","Active"]}}]);write_json(&model_path,&m);
    let mut manifest=read_json(&lib);manifest["exports"]["types"]=json!(["Status"]);write_json(&lib,&manifest);
    let r=project(&tmp.path().join("root"),"synthetic.activation",activation_model("lib::Status",Some("lib::Status::Idle")),"Root",exports(&["Root"],&[],&[],&[]),vec![dep("lib","../lib/project.olproj",&lib)]);let p=checked(&r);
    let mut sim=Sim::new(&p,"Root").unwrap();for (advance,active) in [(false,false),(true,true),(false,true)] {assert_eq!(sim.step(&BTreeMap::from([("advance".into(),Value::Bool(advance))])).unwrap()["active"],Value::Bool(active));}
    let mut m=read_json(&r.parent().unwrap().join("model.json"));m["packages"][0]["activations"][0]["else_equations"][0]["rhs"]=expr(Expr::call("last",vec![Expr::var("state")]));write_json(&r.parent().unwrap().join("model.json"),&m);
    error(&r,"needs an initial value");
}

#[test]
fn native_imported_c_is_explicitly_rejected_for_nodes_and_manifests() {
    let tmp=TempDir::new();
    for imported_node in [true,false] {
        let mut m=model(vec![simple_node("Root",Expr::var("x"))]);
        if imported_node {m["packages"][0]["nodes"][0]["kind"]=json!("Imported");}
        else {m["packages"][0]["imported_operators"]=json!([{"name":"Foreign","symbol":"foreign","language":"c"}]);}
        let r=project(tmp.path(),"synthetic.root",m,"Root",exports(&["Root"],&[],&[],&[]),vec![]);error(&r,"Imported nodes and imported operators are unsupported");
    }
}

#[test]
fn loading_success_and_failure_leave_readonly_library_bytes_and_modes_unchanged() {
    let tmp=TempDir::new();let lib=library(&tmp.path().join("lib"),"synthetic.lib");let files=[lib.clone(),lib.parent().unwrap().join("model.json")];
    for path in &files {let mut permissions=std::fs::metadata(path).unwrap().permissions();permissions.set_readonly(true);std::fs::set_permissions(path,permissions).unwrap();}
    let before=files.iter().map(|p|(std::fs::read(p).unwrap(),std::fs::metadata(p).unwrap().permissions())).collect::<Vec<_>>();
    let r=root(&tmp.path().join("root"),vec![dep("lib","../lib/project.olproj",&lib)],Expr::call("lib::Pass",vec![Expr::var("x")]));checked(&r);
    let mut m=read_json(&r);m["dependencies"][0]["snapshot_sha256"]=json!("0".repeat(64));write_json(&r,&m);error(&r,"snapshot mismatch");
    for (path,(bytes,mode)) in files.iter().zip(before) {assert_eq!(std::fs::read(path).unwrap(),bytes);assert_eq!(std::fs::metadata(path).unwrap().permissions(),mode);}
}

fn c_executable(dir: &Path, name: &str) -> PathBuf {
    dir.join(if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() })
}

fn c_compiler(sanitizers: bool) -> Command {
    let mut cc = Command::new("cc");
    cc.args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-Wno-unused-variable", "-Wno-unused-but-set-variable"]);
    if sanitizers {
        cc.args(["-fsanitize=address,undefined", "-fno-omit-frame-pointer", "-fno-sanitize-recover=all"]);
    }
    cc
}

fn sanitizer_support() -> &'static Result<(), String> {
    static SUPPORT: OnceLock<Result<(), String>> = OnceLock::new();
    SUPPORT.get_or_init(|| {
        let probe = TempDir::new();
        let source = probe.path().join("sanitizer_probe.c");
        std::fs::write(&source, "int main(void) { volatile int value = 7; return value != 7; }\n").unwrap();
        let executable = c_executable(probe.path(), "sanitizer_probe");
        let output = c_compiler(true).arg(&source).arg("-o").arg(&executable).output()
            .expect("existing host cc is required for sanitizer capability probing");
        if !output.status.success() {
            return Err(format!("compile/link probe {}: {}{}", output.status,
                String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)));
        }
        let output = Command::new(&executable).output()
            .map_err(|error| format!("runtime probe could not start: {error}"))?;
        if !output.status.success() {
            return Err(format!("runtime probe {}: {}{}", output.status,
                String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)));
        }
        Ok(())
    })
}

fn report_c_coverage(context: &str, coverage: &str) {
    use std::io::Write;
    // Direct stderr writes remain visible in successful CI tests; libtest
    // captures eprintln! output unless the workflow requests --show-output.
    writeln!(std::io::stderr(), "native generated-C [{context}]: {coverage}").unwrap();
}

fn assert_c_oracle(dir: &Path, header: &str, source: &str, driver: &str,
                   input: &str, expected: &str, context: &str) {
    // Functional C acceptance is mandatory even when sanitizers are absent.
    let plain = compile_c(&dir.join("plain"), header, source, driver, false);
    assert_eq!(run_c(&plain, input), expected, "unsanitized C oracle: {context}");
    report_c_coverage(context, "unsanitized C oracle PASS");
    match sanitizer_support() {
        Ok(()) => {
            // A successful standalone probe makes every generated-C compile,
            // runtime and oracle failure fatal. There is no fallback here.
            let sanitized = compile_c(&dir.join("sanitized"), header, source, driver, true);
            assert_eq!(run_c(&sanitized, input), expected, "ASan+UBSan C oracle: {context}");
            report_c_coverage(context, "ASan+UBSan C oracle PASS");
        }
        Err(reason) => report_c_coverage(context, &format!("ASan+UBSan unavailable; additional sanitized run omitted: {reason}")),
    }
}

fn compile_c(dir:&Path,header:&str,source:&str,driver:&str,sanitizers:bool)->PathBuf {
    std::fs::create_dir_all(dir).unwrap();std::fs::write(dir.join("openlustre_generated.h"),header).unwrap();std::fs::write(dir.join("openlustre_generated.c"),source).unwrap();std::fs::write(dir.join("driver.c"),driver).unwrap();
    let executable=c_executable(dir,"run");let mut cc=c_compiler(sanitizers);
    let result=cc.arg("-o").arg(&executable).arg(dir.join("openlustre_generated.c")).arg(dir.join("driver.c")).arg(format!("-I{}",dir.display())).output().expect("existing host cc is required for native generated-C acceptance");
    assert!(result.status.success(),"cc failed: {}",String::from_utf8_lossy(&result.stderr));executable
}
fn run_c(executable:&Path,input:&str)->String {
    use std::io::Write;
    let mut child=Command::new(executable).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();let out=child.wait_with_output().unwrap();
    assert!(out.status.success(),"generated C failed: {}",String::from_utf8_lossy(&out.stderr));normalize_c_stdout(&String::from_utf8(out.stdout).unwrap())
}

fn normalize_c_stdout(stdout: &str) -> String {
    // Windows C text output translates LF to CRLF. Preserve every other byte,
    // including bare CR, whitespace, row order and terminal newline count.
    stdout.replace("\r\n", "\n")
}

#[test]
fn generated_c_stdout_normalization_preserves_all_numeric_rows() {
    let rows = (0..400).map(|row| format!("{row},{},{},{},{},{}",
        row % 2, row * 3, row * 5, row * 7, row * 11)).collect::<Vec<_>>();
    let expected = rows.join("\n") + "\n";
    let crlf = rows.join("\r\n") + "\r\n";
    assert_eq!(normalize_c_stdout(&expected), expected);
    assert_eq!(normalize_c_stdout(&crlf), expected);
    assert_eq!(normalize_c_stdout(&expected.replacen('\n', "\r\n", 1)), expected);
    let mut reordered = rows.clone();
    reordered.swap(137, 138);
    for (control, changed) in [
        ("wrong value", crlf.replacen("137,1,411,", "137,1,412,", 1)),
        ("missing row", rows[..399].join("\r\n") + "\r\n"),
        ("extra row", crlf.clone() + &rows[137] + "\r\n"),
        ("reordered rows", reordered.join("\r\n") + "\r\n"),
        ("blank row", crlf.replacen("\r\n", "\r\n\r\n", 1)),
        ("whitespace", crlf.replacen("137,1,", "137, 1,", 1)),
        ("bare CR", crlf.replacen("\r\n", "\r", 1)),
        ("CRCRLF", crlf.replacen("\r\n", "\r\r\n", 1)),
        ("missing terminal newline", crlf.trim_end_matches("\r\n").to_owned()),
        ("extra terminal newline", crlf.clone() + "\r\n"),
    ] {
        assert_ne!(normalize_c_stdout(&changed), expected, "must reject {control}");
    }
}

fn stateful_library(dir:&Path,id:&str)->PathBuf {
    let cell=simple_node("Cell",Expr::arrow(Expr::int_lit(0),Expr::bin(BinOp::Add,Expr::pre(Expr::var("y")),Expr::var("x"))));
    project(dir,id,model(vec![cell,simple_node("Wrapper",Expr::call("Cell",vec![Expr::var("x")])),simple_node("Envelope",Expr::call("Wrapper",vec![Expr::var("x")]))]),"Envelope",exports(&["Envelope"],&[],&[],&[]),vec![])
}

#[test]
fn native_repeated_nested_instances_reset_and_interleave_in_ir_and_generated_c() {
    let tmp=TempDir::new();let a=stateful_library(&tmp.path().join("a"),"synthetic.state.a");let b=stateful_library(&tmp.path().join("b"),"synthetic.state.b");
    let names=["a0","a1","b0","b1"];
    let r=project(&tmp.path().join("root"),"synthetic.root",model(vec![node("Root",names.iter().map(|n|port(n,int_ty())).collect(),names.iter().map(|n|port(&format!("out_{n}"),int_ty())).collect(),
        vec![("out_a0",Expr::call("left::Envelope",vec![Expr::var("a0")])),("out_a1",Expr::call("left::Envelope",vec![Expr::var("a1")])),("out_b0",Expr::call("right::Envelope",vec![Expr::var("b0")])),("out_b1",Expr::call("right::Envelope",vec![Expr::var("b1")]))])]),"Root",exports(&["Root"],&[],&[],&[]),vec![dep("left","../a/project.olproj",&a),dep("right","../b/project.olproj",&b)]);
    let p=checked(&r);let mut sims=[Sim::new(&p,"Root").unwrap(),Sim::new(&p,"Root").unwrap()];
    let mut totals=[[0i64;4];2];let mut first=[true;2];let mut expected=String::new();let mut driver=String::from("#include \"openlustre_generated.h\"\n#include <stdio.h>\nint main(void) {\n");let backend=&p.find_node("Root").unwrap().name;
    driver.push_str(&format!("{backend}_State s0,s1; {backend}_init(&s0); {backend}_init(&s1); {backend}_Input in; {backend}_Output out;\n"));
    for frame in 0..300usize {
        for owner in 0..2 {
            if owner==1 && frame%3!=0 {continue;}
            if (owner==0 && [37,181].contains(&frame)) || (owner==1 && frame==150) {sims[owner]=Sim::new(&p,"Root").unwrap();totals[owner]=[0;4];first[owner]=true;driver.push_str(&format!("{backend}_init(&s{owner});\n"));}
            let inputs=std::array::from_fn::<_,4,_>(|i| ((frame*(i+3)+owner*13+i*7)%19) as i64-9);
            let map=names.iter().zip(inputs).map(|(name,value)|((*name).into(),Value::Int(value))).collect();let out=sims[owner].step(&map).unwrap();
            if !first[owner] {for (sum,input) in totals[owner].iter_mut().zip(inputs) {*sum+=input;}}first[owner]=false;
            for (name,value) in names.iter().zip(totals[owner]) {assert_eq!(out[&format!("out_{name}")],Value::Int(value),"frame={frame}, instance={owner}, output={name}");}
            expected.push_str(&format!("{frame},{owner},{},{},{},{}\n",totals[owner][0],totals[owner][1],totals[owner][2],totals[owner][3]));
            for (name,value) in names.iter().zip(inputs) {driver.push_str(&format!("in.{name}={value}; "));}
            driver.push_str(&format!("{backend}_step(&s{owner},&in,&out); printf(\"{frame},{owner},%d,%d,%d,%d\\n\",out.out_a0,out.out_a1,out.out_b0,out.out_b1);\n"));
        }
    }
    driver.push_str("return 0; }\n");let bundle=ol_clite_emit::emit_project(&p);
    assert_c_oracle(&tmp.path().join("cc"),&bundle.header,&bundle.source,&driver,"",&expected,"400 root-instance updates / 1,600 values including 3 resets");
}
