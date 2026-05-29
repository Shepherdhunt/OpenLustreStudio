//! Imported C operator wrapper, end-to-end (plan Task 14). Generate a wrapper
//! from a manifest, write a real external C implementation, compile the
//! wrapper + external source together, and confirm the wrapper bridges the
//! OpenLustre `_Input`/`_Output` structs to the external symbol correctly.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use ol_clite_emit::manifest::{
    ImportedContract, ImportedOperator, ImportedPort, ImportedProperties,
};

fn cc_available() -> bool {
    Command::new("cc")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn crc16_manifest() -> ImportedOperator {
    ImportedOperator {
        name: "CRC16_CCITT".into(),
        kind: "imported_operator".into(),
        language: "c".into(),
        header: "crc16.h".into(),
        source: "crc16.c".into(),
        symbol: "crc16_ccitt".into(),
        inputs: vec![
            ImportedPort { name: "data".into(), ty_str: "uint8[4]".into() },
            ImportedPort { name: "length".into(), ty_str: "uint32".into() },
        ],
        outputs: vec![ImportedPort { name: "crc".into(), ty_str: "uint16".into() }],
        contract: ImportedContract {
            properties: ImportedProperties {
                pure: true,
                deterministic: true,
                no_dynamic_memory: true,
                no_global_write: true,
                bounded_execution: true,
            },
            ..Default::default()
        },
    }
}

const CRC16_H: &str = "\
#ifndef CRC16_H
#define CRC16_H
#include <stdint.h>
uint16_t crc16_ccitt(const uint8_t* data, uint32_t length);
#endif
";

// A tiny, correct CRC-16/CCITT (poly 0x1021, init 0xFFFF) implementation.
const CRC16_C: &str = "\
#include \"crc16.h\"
uint16_t crc16_ccitt(const uint8_t* data, uint32_t length) {
  uint16_t crc = 0xFFFF;
  for (uint32_t i = 0; i < length; i++) {
    crc ^= (uint16_t)data[i] << 8;
    for (int b = 0; b < 8; b++) {
      if (crc & 0x8000) crc = (uint16_t)((crc << 1) ^ 0x1021);
      else crc = (uint16_t)(crc << 1);
    }
  }
  return crc;
}
";

#[test]
fn wrapper_compiles_and_bridges_to_external_symbol() {
    if !cc_available() {
        eprintln!("skipping: cc not available");
        return;
    }
    let op = crc16_manifest();
    assert!(op.validate().is_ok());

    let w = ol_clite_emit::emit_wrapper(&op);

    let tmp = make_tempdir();
    std::fs::write(tmp.join("crc16.h"), CRC16_H).unwrap();
    std::fs::write(tmp.join("crc16.c"), CRC16_C).unwrap();
    std::fs::write(tmp.join("CRC16_CCITT_wrapper.h"), &w.header).unwrap();
    std::fs::write(tmp.join(&w.build.wrapper_source), &w.source).unwrap();

    // A small main that drives the wrapper through its OpenLustre struct ABI
    // and compares against the external symbol called directly.
    let main_c = "\
#include \"CRC16_CCITT_wrapper.h\"
#include \"crc16.h\"
#include <stdio.h>
int main(void) {
  CRC16_CCITT_Input in;
  CRC16_CCITT_Output out;
  uint8_t bytes[4] = {0x12, 0x34, 0x56, 0x78};
  for (int i = 0; i < 4; i++) in.data[i] = bytes[i];
  in.length = 4;
  CRC16_CCITT_step(&in, &out);
  uint16_t direct = crc16_ccitt(bytes, 4);
  printf(\"%u %u\\n\", (unsigned)out.crc, (unsigned)direct);
  return (out.crc == direct) ? 0 : 1;
}
";
    std::fs::write(tmp.join("main.c"), main_c).unwrap();
    let exe = tmp.join("crc_test");

    let cc = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-o"])
        .arg(&exe)
        .arg(tmp.join("crc16.c"))
        .arg(tmp.join(&w.build.wrapper_source))
        .arg(tmp.join("main.c"))
        .arg(format!("-I{}", tmp.display()))
        .output()
        .expect("cc runs");
    if !cc.status.success() {
        let stderr = String::from_utf8_lossy(&cc.stderr).to_string();
        let _ = std::fs::remove_dir_all(&tmp);
        panic!(
            "cc failed:\n{stderr}\n--- wrapper.h ---\n{}\n--- wrapper.c ---\n{}",
            w.header, w.source
        );
    }

    let run = Command::new(&exe).output().expect("runs");
    let stdout = String::from_utf8_lossy(&run.stdout).to_string();
    let success = run.status.success();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        success,
        "wrapper output did not match direct call: {stdout}"
    );
    // Both numbers on the line must be equal and non-trivial.
    let nums: Vec<&str> = stdout.split_whitespace().collect();
    assert_eq!(nums.len(), 2, "unexpected output: {stdout}");
    assert_eq!(nums[0], nums[1], "wrapper != direct: {stdout}");
    assert_ne!(nums[0], "0", "CRC should be non-zero for this input");
}

#[test]
fn impure_imported_operator_is_rejected() {
    let mut op = crc16_manifest();
    op.contract.properties.pure = false;
    assert!(op.validate().is_err());
}

fn make_tempdir() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_imp_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}
