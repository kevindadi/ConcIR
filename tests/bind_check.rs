//! Integration tests for the restricted bind_check CLI.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bind_check_test_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_bind_check")).args(args).output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).to_string())
}

fn channels_cir(dir: &PathBuf) -> PathBuf {
    let p = dir.join("cir.json");
    fs::write(&p, r#"{"program":"c","version":"3.5.0","entry":"main::main","modules":[{"name":"main","provides":{"resources":["ch","ch1","ch2"],"functions":["main"]},"requires":{"resources":[],"functions":[]},"resources":[{"name":"ch","kind":"sync","type":"Channel","mode":"Sync","base":"Int","capacity":0},{"name":"ch1","kind":"sync","type":"Channel","mode":"Sync","base":"Int","capacity":0},{"name":"ch2","kind":"sync","type":"Channel","mode":"Sync","base":"Int","capacity":0}],"protection":[],"functions":[{"name":"main","kind":"normal","body":[{"sid":"s1","kind":"return"}]}]}]}"#).unwrap();
    p
}

#[test]
fn channel_token_keeps_digits() {
    let dir = tmp("chan");
    let res = dir.join("resources.json");
    fs::write(&res, r#"{"source":"","resources":[{"name":"ch1_tx#1","kind":"Channel","display":"ch1_tx"},{"name":"ch2_rx#2","kind":"Channel","display":"ch2_rx"}]}"#).unwrap();
    let cir = channels_cir(&dir);
    let (code, out) = run(&["--resources", res.to_str().unwrap(), "--cir", cir.to_str().unwrap()]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["verified"]["ch1_tx#1"]["cir"], "main::ch1");
    assert_eq!(v["verified"]["ch2_rx#2"]["cir"], "main::ch2");
}

#[test]
fn manifest_alias_is_mutually_exclusive() {
    let dir = tmp("alias");
    let res = dir.join("resources.json");
    fs::write(&res, r#"{"source":"","resources":[{"name":"a_mutex0#10","kind":"Mutex","display":"a_mutex0"},{"name":"b_mutex0#20","kind":"Mutex","display":"b_mutex0"}]}"#).unwrap();
    let cir = dir.join("cir.json");
    fs::write(&cir, r#"{"modules":[{"name":"main","resources":[{"name":"a","type":"Mutex"},{"name":"b","type":"Mutex"}],"functions":[{"name":"main"}]}]}"#).unwrap();
    let man = dir.join("m.json");
    fs::write(&man, r#"[{"rust":"a_mutex0","cir":"main::b"}]"#).unwrap();
    let (code, out) = run(&["--resources", res.to_str().unwrap(), "--cir", cir.to_str().unwrap(),
                            "--manifest", man.to_str().unwrap()]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&out).unwrap();
    // a_mutex0#10 is violated and NOT also verified
    assert!(v["violated"].get("a_mutex0").is_some());
    assert!(v["verified"].get("a_mutex0#10").is_none());
}

#[test]
fn broken_manifest_is_an_input_error() {
    let dir = tmp("broken");
    let res = dir.join("resources.json");
    fs::write(&res, r#"{"source":"","resources":[]}"#).unwrap();
    let cir = dir.join("cir.json");
    fs::write(&cir, r#"{"modules":[]}"#).unwrap();
    let man = dir.join("bad.json");
    fs::write(&man, "{not json").unwrap();
    let (code, _) = run(&["--resources", res.to_str().unwrap(), "--cir", cir.to_str().unwrap(),
                          "--manifest", man.to_str().unwrap()]);
    assert_eq!(code, 2);
}

#[test]
fn missing_manifest_is_an_input_error() {
    let dir = tmp("missing");
    let res = dir.join("resources.json");
    fs::write(&res, r#"{"source":"","resources":[]}"#).unwrap();
    let cir = dir.join("cir.json");
    fs::write(&cir, r#"{"modules":[]}"#).unwrap();
    let (code, _) = run(&["--resources", res.to_str().unwrap(), "--cir", cir.to_str().unwrap(),
                          "--manifest", dir.join("nope.json").to_str().unwrap()]);
    assert_eq!(code, 2);
}

#[test]
fn unestablished_claim_is_unresolved_not_violated() {
    // A valid object whose relation cannot be established must be unresolved,
    // not reported as a structural contradiction.
    let dir = tmp("unest");
    let res = dir.join("resources.json");
    fs::write(&res, r#"{"source":"","resources":[{"name":"tx#10","kind":"Channel","display":"tx"}]}"#).unwrap();
    let cir = channels_cir(&dir);
    let man = dir.join("m.json");
    fs::write(&man, r#"[{"rust":"tx#10","cir":"main::ch"}]"#).unwrap();
    let (code, out) = run(&["--resources", res.to_str().unwrap(), "--cir", cir.to_str().unwrap(),
                            "--manifest", man.to_str().unwrap()]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v["violated"].get("tx#10").is_none());
    assert!(v["unresolved"].get("tx#10").is_some());
}

#[test]
fn nonexistent_object_claim_is_violated() {
    let dir = tmp("noobj");
    let res = dir.join("resources.json");
    fs::write(&res, r#"{"source":"","resources":[]}"#).unwrap();
    let cir = dir.join("cir.json");
    fs::write(&cir, r#"{"modules":[]}"#).unwrap();
    let man = dir.join("m.json");
    fs::write(&man, r#"[{"rust":"ghost","cir":"main::a"}]"#).unwrap();
    let (code, out) = run(&["--resources", res.to_str().unwrap(), "--cir", cir.to_str().unwrap(),
                            "--manifest", man.to_str().unwrap()]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v["violated"].get("ghost").is_some());
}
