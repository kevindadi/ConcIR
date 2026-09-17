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

use concir::ast::Program;
use concir::explore::contract::ContractSpec;
use concir::repair::search::{run_search, RepairStrategy, SearchConfig};

fn write_artifact(name: &str, cfg: &SearchConfig, contract_json: &str) -> (PathBuf, serde_json::Value) {
    let p: Program = serde_json::from_str(&std::fs::read_to_string("tests/repro_bench/two_cycles.json").unwrap()).unwrap();
    let spec: ContractSpec = serde_json::from_str(contract_json).unwrap();
    let report = run_search(&p, &spec, cfg);
    let artifact = report.artifact_with_config(&p, &spec, cfg);
    let v = serde_json::to_value(&artifact).unwrap();
    let path = tmp(name);
    std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
    (path, v)
}

const TWO_CYCLES_CONTRACT: &str =
    include_str!("repro_bench/two_cycles_contract.json");

fn replay_expect_fail(v: &serde_json::Value, name: &str) {
    let path = tmp(&format!("{name}.json"));
    std::fs::write(&path, serde_json::to_string(v).unwrap()).unwrap();
    let out = Command::new(bin())
        .args(["replay", path.to_str().unwrap()])
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_ne!(code, 0, "{name}: tamper was accepted");
    assert!(err.contains("replay failed"), "{name}: stderr '{err}'");
}

#[test]
fn f1_f3_single_field_tampering_is_rejected() {
    let cfg = SearchConfig {
        strategy: RepairStrategy::Diagnostic,
        ..SearchConfig::default()
    };
    let (_path, clean) = write_artifact("clean", &cfg, TWO_CYCLES_CONTRACT);
    // Sanity: the clean artifact replays.
    let clean_path = tmp("clean-replay.json");
    std::fs::write(&clean_path, serde_json::to_string(&clean).unwrap()).unwrap();
    assert_eq!(
        Command::new(bin())
            .args(["replay", clean_path.to_str().unwrap()])
            .output()
            .unwrap()
            .status
            .code()
            .unwrap_or(-1),
        0
    );

    // F1: empty patch chain.
    let mut v = clean.clone();
    v["patch_chain"] = serde_json::json!([]);
    replay_expect_fail(&v, "empty_chain");

    // F1: bad chain function base.
    let mut v = clean.clone();
    v["patch_chain"][0]["original_function_hash"] = serde_json::json!("bad-hash");
    replay_expect_fail(&v, "bad_chain_hash");

    // F1: unrelated resource added to accepted_program only.
    let mut v = clean.clone();
    let res = serde_json::json!({"name":"extra","kind":"sync","type":"Mutex","mode":"Sync"});
    v["accepted_program"]["modules"][0]["resources"]
        .as_array_mut()
        .unwrap()
        .push(res);
    replay_expect_fail(&v, "unrelated_accepted");

    // F1: accepted_node out of range.
    let mut v = clean.clone();
    v["accepted_node"] = serde_json::json!(999);
    replay_expect_fail(&v, "bad_accepted_node");

    // F1: accepted_report fingerprint mismatch.
    let mut v = clean.clone();
    v["accepted_report"]["model_fingerprint"] = serde_json::json!("bad-model");
    replay_expect_fail(&v, "bad_accepted_report");

    // F1: chain end fingerprint mismatch.
    let mut v = clean.clone();
    let last = v["patch_chain"].as_array().unwrap().len() - 1;
    v["patch_chain"][last]["program_fingerprint"] = serde_json::json!("bad-fp");
    replay_expect_fail(&v, "bad_chain_result");

    // F2: permission disallowed by the frozen contract.
    let mut v = clean.clone();
    v["frozen_contract"]["allowed_scope"]["allow_lock_reorder"] = serde_json::json!(false);
    replay_expect_fail(&v, "forbidden_scope");

    // F2: preserved removed from the frozen contract.
    let mut v = clean.clone();
    v["frozen_contract"]["preserved"] = serde_json::json!([]);
    replay_expect_fail(&v, "deleted_preserved");

    // F3: attempt parent out of range.
    let mut v = clean.clone();
    v["attempts"][0]["parent"] = serde_json::json!(99999);
    replay_expect_fail(&v, "bad_attempt_parent");

    // F3: false counts.
    let mut v = clean.clone();
    v["counts"]["verification_calls"] = serde_json::json!(0);
    replay_expect_fail(&v, "false_counts");

    // F3: false effective bounds.
    let mut v = clean.clone();
    v["effective_config"]["bounds"]["max_states"] = serde_json::json!(1);
    replay_expect_fail(&v, "false_effective_bounds");

    // F3: false node completeness.
    let mut v = clean.clone();
    let last = v["nodes"].as_array().unwrap().len() - 1;
    v["nodes"][last]["report"]["complete"] = serde_json::json!(false);
    replay_expect_fail(&v, "false_node_complete");

    // F3: emptied root properties.
    let mut v = clean.clone();
    v["nodes"][0]["report"]["properties"] = serde_json::json!([]);
    replay_expect_fail(&v, "false_root_properties");

    // F3: incoming result fingerprint mismatch.
    let mut v = clean.clone();
    v["nodes"][1]["incoming"]["program_fingerprint"] = serde_json::json!("bad-hash");
    replay_expect_fail(&v, "bad_incoming_result_hash");
}

#[test]
fn f4_budget_blocked_attempt_is_recorded_and_replays() {
    let cfg = SearchConfig {
        strategy: RepairStrategy::Composite,
        verification_budget: 1,
        ..SearchConfig::default()
    };
    let (path, v) = write_artifact("budget_one", &cfg, TWO_CYCLES_CONTRACT);
    // The generated candidate is recorded, not dropped.
    let attempts = v["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1, "{v}");
    assert_eq!(attempts[0]["result"], "budget-blocked");
    assert!(attempts[0]["patch"].is_object());
    assert!(attempts[0]["program_fingerprint"].is_string());
    assert!(attempts[0]["outcome"].is_null());
    assert_eq!(v["counts"]["proposals"], 1);
    assert_eq!(v["counts"]["verification_calls"], 1);
    // And the artifact replays cleanly.
    let out = Command::new(bin())
        .args(["replay", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code().unwrap_or(-1), 0, "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn f_positive_artifacts_replay() {
    // Reused + denied + budget-blocked attempts, and every terminal outcome.
    let contract: ContractSpec = serde_json::from_str(TWO_CYCLES_CONTRACT).unwrap();
    let mut denied = contract.clone();
    denied.allowed_scope.modules = vec!["other".to_string()];
    let denied_json = serde_json::to_string(&denied).unwrap();

    let singles: [(&str, SearchConfig, &str); 5] = [
        ("reused", SearchConfig { strategy: RepairStrategy::Composite, ..SearchConfig::default() }, TWO_CYCLES_CONTRACT),
        ("denied", SearchConfig { strategy: RepairStrategy::Composite, ..SearchConfig::default() }, &denied_json),
        ("unknown", SearchConfig { strategy: RepairStrategy::Composite, ..SearchConfig::default() }, include_str!("repro_round2/tiny_bounds_contract.json")),
        ("budget_zero", SearchConfig { strategy: RepairStrategy::Diagnostic, verification_budget: 0, ..SearchConfig::default() }, TWO_CYCLES_CONTRACT),
        ("cand_budget", SearchConfig { strategy: RepairStrategy::Composite, candidate_budget: 1, ..SearchConfig::default() }, TWO_CYCLES_CONTRACT),
    ];
    for (name, cfg, cj) in singles {
        let (path, _) = write_artifact(name, &cfg, cj);
        let out = Command::new(bin())
            .args(["replay", path.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code().unwrap_or(-1),
            0,
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn artifact_value(model: &str, contract: &str, cfg: &SearchConfig) -> serde_json::Value {
    let p: Program = serde_json::from_str(&std::fs::read_to_string(model).unwrap()).unwrap();
    let spec: ContractSpec = serde_json::from_str(contract).unwrap();
    let report = run_search(&p, &spec, cfg);
    serde_json::to_value(report.artifact_with_config(&p, &spec, cfg)).unwrap()
}

#[test]
fn g1_g3_residual_tampering_is_rejected() {
    let diag = SearchConfig {
        strategy: RepairStrategy::Diagnostic,
        ..SearchConfig::default()
    };
    let single = artifact_value(
        "tests/repro_bench/single_cycle.json",
        include_str!("repro_bench/single_cycle_contract.json"),
        &diag,
    );
    let blocked_cfg = SearchConfig {
        strategy: RepairStrategy::Composite,
        verification_budget: 1,
        ..SearchConfig::default()
    };
    let blocked = artifact_value(
        "tests/repro_bench/two_cycles.json",
        TWO_CYCLES_CONTRACT,
        &blocked_cfg,
    );
    let unfix_cfg = SearchConfig {
        strategy: RepairStrategy::Composite,
        ..SearchConfig::default()
    };
    let unfix = artifact_value(
        "tests/repro_bench/preserved_unfixable.json",
        include_str!("repro_bench/preserved_unfixable_contract.json"),
        &unfix_cfg,
    );

    let cases: Vec<(&str, serde_json::Value)> = vec![
        ("attempt_bad_hash", {
            let mut v = single.clone();
            v["attempts"][0]["patch"]["original_hash"] = serde_json::json!("broken");
            v
        }),
        ("attempt_missing_sid", {
            let mut v = single.clone();
            v["attempts"][0]["patch"]["changes"][0]["a"] = serde_json::json!("nonexistent_sid");
            v
        }),
        ("attempt_wrong_target", {
            let mut v = single.clone();
            v["attempts"][0]["patch"]["function"] = serde_json::json!("nonexistent_function");
            v
        }),
        ("false_transitions", {
            let mut v = single.clone();
            v["nodes"][0]["report"]["transitions_explored"] = serde_json::json!(0);
            v
        }),
        ("zero_states_consistently", {
            let mut v = single.clone();
            for n in v["nodes"].as_array_mut().unwrap() {
                n["report"]["states_explored"] = serde_json::json!(0);
            }
            v["counts"]["states_explored"] = serde_json::json!(0);
            v["accepted_report"]["states_explored"] = serde_json::json!(0);
            v
        }),
        ("false_analysis_started", {
            let mut v = single.clone();
            v["nodes"][0]["report"]["analysis_started"] = serde_json::json!(false);
            v
        }),
        ("erase_counterexample", {
            let mut v = single.clone();
            v["nodes"][0]["report"]["diagnostics"][0]["counterexample"] = serde_json::json!([]);
            v
        }),
        ("erase_blocking_facts", {
            let mut v = single.clone();
            v["nodes"][0]["report"]["diagnostics"][0]["blocked"] = serde_json::json!([]);
            v
        }),
        ("blocked_patch_bad_hash", {
            let mut v = blocked.clone();
            v["attempts"][0]["patch"]["original_hash"] = serde_json::json!("broken");
            v
        }),
        ("budget_as_no_candidate", {
            let mut v = blocked.clone();
            v["outcome"] = serde_json::json!("no_acceptable_candidate");
            v
        }),
        ("budget_reason_solved", {
            let mut v = blocked.clone();
            v["stop_reason"] = serde_json::json!("solved");
            v
        }),
        ("unfix_as_unknown", {
            let mut v = unfix.clone();
            v["outcome"] = serde_json::json!("analysis_unknown");
            v
        }),
        ("unfix_as_budget", {
            let mut v = unfix.clone();
            v["outcome"] = serde_json::json!("budget_exhausted");
            v
        }),
        ("unfix_wrong_stop_reason", {
            let mut v = unfix.clone();
            v["stop_reason"] = serde_json::json!("verification-budget");
            v
        }),
    ];
    for (name, v) in cases {
        replay_expect_fail(&v, name);
    }
}

#[test]
fn g_positive_terminal_artifacts_replay() {
    let diag = SearchConfig {
        strategy: RepairStrategy::Diagnostic,
        ..SearchConfig::default()
    };
    let comp = SearchConfig {
        strategy: RepairStrategy::Composite,
        ..SearchConfig::default()
    };
    let cases: Vec<(&str, serde_json::Value)> = vec![
        (
            "already_correct",
            artifact_value(
                "tests/repro_bench/already_correct.json",
                include_str!("repro_bench/already_correct_contract.json"),
                &diag,
            ),
        ),
        (
            "root_invalid",
            artifact_value(
                "tests/repro_round2/runtime_invalid_exit.json",
                include_str!("repro_round2/runtime_invalid_exit_contract.json"),
                &diag,
            ),
        ),
        (
            "root_unsupported",
            artifact_value(
                "tests/repro_round2/ignored_assumptions.json",
                include_str!("repro_round2/ignored_assumptions_contract.json"),
                &diag,
            ),
        ),
        (
            "no_acceptable",
            artifact_value(
                "tests/repro_bench/preserved_unfixable.json",
                include_str!("repro_bench/preserved_unfixable_contract.json"),
                &comp,
            ),
        ),
        (
            "two_step_repaired",
            artifact_value(
                "tests/repro_bench/two_cycles.json",
                TWO_CYCLES_CONTRACT,
                &diag,
            ),
        ),
        (
            "composite_with_fail_intermediate",
            artifact_value(
                "tests/repro_bench/two_cycles.json",
                TWO_CYCLES_CONTRACT,
                &comp,
            ),
        ),
    ];
    for (name, v) in cases {
        let path = tmp(&format!("positive-{name}.json"));
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
        let out = Command::new(bin())
            .args(["replay", path.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code().unwrap_or(-1),
            0,
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
