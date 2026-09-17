//! E6/E7/E8: CLI argument handling, exit codes, and artifact round-trip.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_concir-backend")
}

fn tmp(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "concir-cli-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    p
}

fn run(args: &[&str]) -> (i32, String) {
    let out = Command::new(bin()).args(args).output().expect("run");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

#[test]
fn e6_legacy_budget_is_read_and_validated() {
    let patches = tmp("patches.json");
    std::fs::write(
        &patches,
        r#"[{"module":"main","function":"t2","changes":[{"kind":"swap_statements","a":"s1","b":"s2"}]}]"#,
    )
    .unwrap();
    let model = "tests/repro_bench/single_cycle.json";
    let contract = "tests/repro_bench/single_cycle_contract.json";
    let patches = patches.to_str().unwrap();

    // Omitted budget: repaired.
    let (code, out) = run(&["repair", model, contract, patches]);
    assert_eq!(code, 0, "{out}");
    // Explicit 0: the caller's zero-candidate budget must hold.
    let (code, out) = run(&["repair", model, contract, patches, "0"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("budget_exhausted"), "{out}");
    // Invalid budget: usage error.
    assert_eq!(run(&["repair", model, contract, patches, "abc"]).0, 2);
    // Extra argument: usage error.
    assert_eq!(run(&["repair", model, contract, patches, "1", "extra"]).0, 2);
}

#[test]
fn e7_strategy_exit_codes_match_explore() {
    // INVALID -> 4.
    assert_eq!(
        run(&[
            "repair",
            "tests/repro_round2/runtime_invalid_exit.json",
            "tests/repro_round2/runtime_invalid_exit_contract.json",
            "--strategy",
            "c",
        ])
        .0,
        4
    );
    // UNSUPPORTED -> 5.
    assert_eq!(
        run(&[
            "repair",
            "tests/repro_round2/ignored_assumptions.json",
            "tests/repro_round2/ignored_assumptions_contract.json",
            "--strategy",
            "c",
        ])
        .0,
        5
    );
    // UNKNOWN root -> 3.
    assert_eq!(
        run(&[
            "repair",
            "tests/repro_round2/finite_call_loop.json",
            "tests/repro_round2/tiny_bounds_contract.json",
            "--strategy",
            "c",
        ])
        .0,
        3
    );
}

#[test]
fn e8_repair_artifact_round_trips_and_replays() {
    let artifact = tmp("artifact.json");
    let artifact_s = artifact.to_str().unwrap();
    let (code, out) = run(&[
        "repair",
        "tests/repro_bench/two_cycles.json",
        "tests/repro_bench/two_cycles_contract.json",
        "--strategy",
        "c",
        "--artifact",
        artifact_s,
    ]);
    assert_eq!(code, 0, "{out}");
    // The stdout is the complete artifact.
    let v: serde_json::Value = serde_json::from_str(&out).expect("stdout artifact JSON");
    assert_eq!(v["schema_version"], "concir-repair-artifact-v1");
    assert!(v["input_program"].is_object());
    assert!(v["frozen_contract"].is_object());
    assert_eq!(v["patch_chain"].as_array().unwrap().len(), 2);
    assert!(v["accepted_program"].is_object());
    assert!(v["source"]["binary_fingerprint"].is_string());
    assert!(v["effective_config"]["bounds"].is_object());

    // Replay the written artifact.
    let (code, out) = run(&["replay", artifact_s]);
    assert_eq!(code, 0, "{out}");

    // Tamper with the input program: replay must fail.
    let mut tampered: serde_json::Value = v.clone();
    tampered["input_program"]["modules"][0]["functions"][1]["body"][0]["resource"] =
        serde_json::json!("zzz");
    let bad = tmp("bad.json");
    std::fs::write(&bad, serde_json::to_string(&tampered).unwrap()).unwrap();
    let (code, _) = run(&["replay", bad.to_str().unwrap()]);
    assert_eq!(code, 4, "tampered artifact must be rejected");
}

#[test]
fn e8_bench_writes_complete_records() {
    let path = tmp("bench.json");
    let (code, _) = run(&["bench", "--artifact", path.to_str().unwrap()]);
    assert_eq!(code, 0);
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let records = v.as_array().unwrap();
    assert_eq!(records.len(), 8 * 3);
    for r in records {
        assert!(r["artifact"]["frozen_contract"].is_object());
        assert!(r["artifact"]["input_program"].is_object());
        assert!(r["artifact"]["nodes"].is_array());
        assert!(r["artifact"]["source"]["binary_fingerprint"].is_string());
    }
}
