//! The model file format's version (`docs/model-format.md`): files from
//! every release keep loading and keep their meaning, a newer format is
//! refused rather than misread, and every file the Studio writes names its
//! format.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

/// The Studio's access token for these tests (`OPENLUSTRE_STUDIO_TOKEN`).
const TEST_TOKEN: &str = "openlustre-test-token";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/format")
}

/// One folder per release, holding the files that release wrote.
fn releases() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(fixtures())
        .expect("tests/fixtures/format")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    assert!(!dirs.is_empty(), "no release fixtures");
    dirs
}

fn openlustre(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--"])
        .args(args)
        .output()
        .expect("cargo run openlustre")
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ol-model-format-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Each release's sample models load, and their recorded scenarios still
/// give the recorded results: the files mean what they meant.
#[test]
fn every_release_s_files_still_load_and_mean_the_same() {
    for release in releases() {
        let models = [
            ("pms/pms.wksc", "pms/scenarios"),
            ("release_logic/release_logic.json", "release_logic/scenarios"),
            ("release_logic/release_logic.ols", "release_logic/scenarios"),
        ];
        for (model, scenarios) in models {
            let model = release.join(model);
            if !model.exists() {
                continue;
            }
            ol_ir::load_project(&model).unwrap_or_else(|e| panic!("{}: {e}", model.display()));
            let o = openlustre(&[
                "test".as_ref(),
                "run".as_ref(),
                model.as_os_str(),
                "--scenarios".as_ref(),
                release.join(scenarios).as_os_str(),
                "--backend".as_ref(),
                "ir".as_ref(),
            ]);
            let out = String::from_utf8_lossy(&o.stdout);
            assert!(o.status.success(), "{}:\n{out}\n{}", model.display(), String::from_utf8_lossy(&o.stderr));
            assert!(out.contains(" passed, 0 failed, 0 skipped"), "{}:\n{out}", model.display());
        }
    }
}

/// A file from a newer release is refused with a message that says so —
/// by the CLI and by the Studio — instead of being misread (and its
/// unknown parts dropped on the next save).
#[test]
fn a_newer_format_is_refused_with_a_clear_message() {
    let dir = temp_dir("newer");
    let src = releases()[0].join("release_logic/release_logic.json");
    let mut doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(src).unwrap()).unwrap();
    doc["format_version"] = (ol_ir::FORMAT_VERSION + 1).into();
    doc["a_future_field"] = serde_json::json!({"kept": "only by a newer release"});
    let model = dir.join("future.json");
    std::fs::write(&model, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

    let e = ol_ir::load_project(&model).unwrap_err().to_string();
    assert!(e.contains("newer OpenLustre Studio"), "{e}");
    for cmd in [&["check".as_ref(), model.as_os_str()][..], &["studio".as_ref(), "serve".as_ref(), model.as_os_str()][..]] {
        let o = openlustre(cmd);
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success(), "{cmd:?} accepted a newer format");
        assert!(err.contains("newer OpenLustre Studio") && err.contains("update OpenLustre Studio"), "{cmd:?}: {err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

struct ServerGuard(Child);

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn request(port: u16, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nX-OpenLustre-Token: {TEST_TOKEN}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).unwrap();
    stream.shutdown(Shutdown::Write).ok();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, payload) = raw.split_once("\r\n\r\n").unwrap();
    (head.split_whitespace().nth(1).unwrap().parse().unwrap(), payload.to_string())
}

fn serve(model: &Path) -> (ServerGuard, u16) {
    use std::io::BufRead;
    let mut child = Command::new(env!("CARGO"))
        .env("OPENLUSTRE_STUDIO_TOKEN", TEST_TOKEN)
        .args(["run", "-q", "-p", "ol_cli", "--", "studio", "serve"])
        .arg(model)
        .args(["--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut port = None;
    for _ in 0..400 {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if let Some((_, rest)) = line.split_once("http://127.0.0.1:") {
            port = rest.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok();
            break;
        }
    }
    let guard = ServerGuard(child);
    (guard, port.expect("the Studio prints its port"))
}

/// An edit in the Studio saves a 0.1.0 workspace in the current format:
/// `format_version` first, everything else read back unchanged, and — the
/// format being the same — no backup copy.
#[test]
fn the_studio_writes_the_format_version_into_a_file_it_saves() {
    let dir = temp_dir("save");
    let release = &releases()[0];
    for f in ["pms.wksc", "types.json"] {
        std::fs::copy(release.join("pms").join(f), dir.join(f)).unwrap();
    }
    let model = dir.join("pms.wksc");
    let before = ol_ir::load_project(&model).unwrap();
    let (_server, port) = serve(&model);
    let (s, body) = request(port, "/api/edit/add_node", r#"{"name":"FormatProbe","kind":"function"}"#);
    assert_eq!(s, 200, "{body}");

    let text = std::fs::read_to_string(&model).unwrap();
    let head = format!("{{\n  \"format_version\": {},\n  \"name\": ", ol_ir::FORMAT_VERSION);
    assert!(text.starts_with(&head), "{}", &text[..80.min(text.len())]);
    let after = ol_ir::load_project(&model).unwrap();
    let mut probe_removed = after.clone();
    for p in &mut probe_removed.packages {
        p.nodes.retain(|n| n.name != "FormatProbe");
    }
    assert_eq!(probe_removed, before, "the save changed more than the edit");
    let backups: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().filter(|e| e.path().to_string_lossy().ends_with(".bak")).collect();
    assert!(backups.is_empty(), "same format, no backup: {backups:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
