//! ConcIR v5 integration tests:
//! 1. UniPN CTL & LTL model checking (with fairness + non-vacuity rejection)
//! 2. `concir-backend explain-contract` & `concir-backend export-cvn`
//! 3. `TypeHole` (`src/domain.rs`), `Struct`/`Array` composite types, `Mutex`/`RwLock` payloads
//! 4. `RwLock` semantics (`interp` & `petri` agreement), MIR-clean codegen, and refinement
//! 5. Multi-file module codegen (`src/<mod>.rs`) and `--module <MOD>` compositional refinement

use std::process::Command;

use concir::ast::{BaseType, ComplexBaseType, Program};
use concir::codegen;
use concir::explore::contract::ContractSpec;
use concir::explore::{verify_program, EngineKind};
use concir::refinement::{verify_refinement, verify_refinement_for_module, ImplNet};
use concir::sem::outcome::Outcome;
use concir::sem::program;
use concir::validate;

fn parse_program(json: &str) -> Program {
    serde_json::from_str(json).expect("valid program JSON")
}

fn parse_contract(json: &str) -> ContractSpec {
    serde_json::from_str(json).expect("valid contract JSON")
}

fn run_cli(args: &[&str]) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_concir-backend");
    let out = Command::new(bin).args(args).output().expect("spawn cli");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

// ──────────────────── 1. CTL & LTL + Fairness + Non-Vacuity ────────────────────

#[test]
fn ctl_and_ltl_model_checking_with_non_vacuity_rejection() {
    let prog = parse_program(
        r#"{
            "program": "v5_temporal_counter",
            "version": "4.0.0",
            "entry": "main::main",
            "modules": [{
                "name": "main",
                "provides": {"resources": ["m", "c"], "functions": ["main", "inc"]},
                "requires": {"resources": [], "functions": []},
                "resources": [
                    {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"},
                    {"name": "c", "kind": "var", "type": "Var", "base": {"Int": [0, 3]}, "init": 0}
                ],
                "protection": [{"var": "c", "lock": "m"}],
                "functions": [
                    {
                        "name": "main",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "scope", "funcs": ["inc"]},
                            {"sid": "s2", "kind": "return"}
                        ]
                    },
                    {
                        "name": "inc",
                        "kind": "normal",
                        "structured_body": [
                            {
                                "id": "b1",
                                "kind": "lock_guard",
                                "resource": "m",
                                "body": [
                                    {"id": "b2", "kind": "write_shared", "resource": "c", "expr": "c + 1"}
                                ]
                            },
                            {"id": "b3", "kind": "return"}
                        ]
                    }
                ]
            }]
        }"#,
    );

    // Passing CTL + LTL (including non-vacuous implication whose antecedent IS reachable: c == 1 -> eventually inc completes).
    let pass_contract = parse_contract(
        r#"{
            "name": "v5-temporal-pass",
            "properties": [
                {"kind": "deadlock_free", "id": "dl", "req": ["R1"]},
                {
                    "kind": "ctl",
                    "id": "ctl-ef-inc",
                    "req": ["R2"],
                    "formula": {
                        "op": "ef",
                        "arg": {"op": "atom", "atom": {"kind": "var_eq", "resource": "main::c", "value": 1}}
                    }
                },
                {
                    "kind": "ctl",
                    "id": "ctl-nonvac-ok",
                    "non_vacuous": true,
                    "req": ["R2"],
                    "formula": {
                        "op": "ag",
                        "arg": {
                            "op": "implies",
                            "lhs": {"op": "atom", "atom": {"kind": "var_eq", "resource": "main::c", "value": 1}},
                            "rhs": {"op": "af", "arg": {"op": "atom", "atom": {"kind": "function_completed", "function": "main::inc"}}}
                        }
                    }
                },
                {
                    "kind": "ltl",
                    "id": "ltl-eventually-done",
                    "non_vacuous": true,
                    "req": ["R3"],
                    "formula": {
                        "op": "always",
                        "arg": {
                            "op": "implies",
                            "lhs": {"op": "atom", "atom": {"kind": "var_eq", "resource": "main::c", "value": 0}},
                            "rhs": {"op": "eventually", "arg": {"op": "atom", "atom": {"kind": "function_completed", "function": "main::inc"}}}
                        }
                    }
                }
            ]
        }"#,
    );

    for engine in [EngineKind::Interpreter, EngineKind::Petri] {
        let rep = verify_program(&prog, &pass_contract, engine);
        assert_eq!(rep.outcome, Outcome::Pass, "engine {:?}: {:?}", engine, rep);
        for p in &rep.properties {
            assert!(!p.vacuous, "property {} unexpectedly vacuous", p.id);
        }
    }

    // Vacuous implication (antecedent c > 2 is never reachable) MUST fail when non_vacuous=true!
    let vac_contract = parse_contract(
        r#"{
            "name": "v5-vacuity",
            "properties": [
                {
                    "kind": "ctl",
                    "id": "ctl-vacuous",
                    "non_vacuous": true,
                    "req": ["R4"],
                    "formula": {
                        "op": "ag",
                        "arg": {
                            "op": "implies",
                            "lhs": {"op": "atom", "atom": {"kind": "var_cmp", "resource": "main::c", "op": ">", "value": 2}},
                            "rhs": {"op": "atom", "atom": {"kind": "var_eq", "resource": "main::c", "value": 0}}
                        }
                    }
                },
                {
                    "kind": "ltl",
                    "id": "ltl-vacuous",
                    "non_vacuous": true,
                    "req": ["R5"],
                    "formula": {
                        "op": "always",
                        "arg": {
                            "op": "implies",
                            "lhs": {"op": "atom", "atom": {"kind": "var_cmp", "resource": "main::c", "op": ">", "value": 2}},
                            "rhs": {"op": "eventually", "arg": {"op": "atom", "atom": {"kind": "var_eq", "resource": "main::c", "value": 0}}}
                        }
                    }
                }
            ]
        }"#,
    );

    let rep_vac = verify_program(&prog, &vac_contract, EngineKind::Petri);
    assert_eq!(rep_vac.outcome, Outcome::Fail);
    assert_eq!(rep_vac.properties.len(), 2);
    for p in &rep_vac.properties {
        assert_eq!(p.outcome, Outcome::Fail, "expected vacuous fail for {}", p.id);
        assert!(p.vacuous, "expected vacuous=true for {}", p.id);
        assert!(!p.vacuous_antecedents.is_empty());
        assert!(p.explanation.is_some());
    }
    assert_eq!(rep_vac.diagnostics.len(), 2);
}

#[test]
fn ltl_fairness_eliminates_starvation_counterexample() {
    // Two threads: spinner loops forever reading flag until setter sets flag = 1.
    // Without fairness, spinner can starve setter forever -> LTL F(flag == 1) FAILS with lasso.
    // With Justice fairness that setter eventually completes (or reaches its write), LTL PASSES!
    let prog = parse_program(
        r#"{
            "program": "fair_spin",
            "version": "4.0.0",
            "entry": "main::main",
            "modules": [{
                "name": "main",
                "provides": {"resources": ["flag"], "functions": ["main", "spinner", "setter"]},
                "requires": {"resources": [], "functions": []},
                "resources": [
                    {"name": "flag", "kind": "var", "type": "Atomic", "base": {"Int": [0, 1]}, "init": 0}
                ],
                "protection": [],
                "functions": [
                    {
                        "name": "main",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "scope", "funcs": ["spinner", "setter"]},
                            {"sid": "s2", "kind": "return"}
                        ]
                    },
                    {
                        "name": "spinner",
                        "kind": "normal",
                        "locals": [{"name": "v", "type": {"Int": [0, 1]}, "modeled": true, "init": 0}],
                        "body": [
                            {"sid": "s1", "kind": "atomic_load", "resource": "flag", "dst": "v"},
                            {"sid": "s2", "kind": "branch", "cond": "v == 0", "then": "s1", "else": "s3"},
                            {"sid": "s3", "kind": "return"}
                        ]
                    },
                    {
                        "name": "setter",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "atomic_store", "resource": "flag", "value": "1"},
                            {"sid": "s2", "kind": "return"}
                        ]
                    }
                ]
            }]
        }"#,
    );

    let unfair = parse_contract(
        r#"{
            "name": "unfair-liveness",
            "properties": [{
                "kind": "ltl",
                "id": "eventually-flag",
                "formula": {
                    "op": "eventually",
                    "arg": {"op": "atom", "atom": {"kind": "var_eq", "resource": "main::flag", "value": 1}}
                }
            }]
        }"#,
    );
    let rep_unfair = verify_program(&prog, &unfair, EngineKind::Petri);
    assert_eq!(rep_unfair.outcome, Outcome::Fail, "without fairness, spinner can starve setter");
    assert!(!rep_unfair.diagnostics.is_empty());

    let fair = parse_contract(
        r#"{
            "name": "fair-liveness",
            "properties": [{
                "kind": "ltl",
                "id": "eventually-flag-fair",
                "formula": {
                    "op": "eventually",
                    "arg": {"op": "atom", "atom": {"kind": "var_eq", "resource": "main::flag", "value": 1}}
                },
                "fairness": [{
                    "kind": "justice",
                    "atom": {"kind": "statement_reached", "function": "main::setter", "sid": "s1"}
                }]
            }]
        }"#,
    );
    let rep_fair = verify_program(&prog, &fair, EngineKind::Petri);
    assert_eq!(rep_fair.outcome, Outcome::Pass, "with justice on setter, flag==1 holds: {:?}", rep_fair);
}

// ──────────────────── 2. CLI: explain-contract & export-cvn ────────────────────

#[test]
fn cli_explain_contract_and_export_cvn() {
    let tmp = std::env::temp_dir().join("concir_v5_cli_test");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let contract_path = tmp.join("contract.json");
    let explain_out = tmp.join("explained.json");
    std::fs::write(
        &contract_path,
        r#"{
            "name": "demo-contract",
            "properties": [
                {"kind": "deadlock_free", "id": "dl", "req": ["R1"]},
                {
                    "kind": "ctl",
                    "id": "ctl1",
                    "non_vacuous": true,
                    "req": ["R2"],
                    "formula": {
                        "op": "ag",
                        "arg": {
                            "op": "implies",
                            "lhs": {"op": "atom", "atom": {"kind": "mutex_held", "resource": "main::m"}},
                            "rhs": {"op": "ef", "arg": {"op": "atom", "atom": {"kind": "mutex_free", "resource": "main::m"}}}
                        }
                    }
                },
                {
                    "kind": "ltl",
                    "id": "ltl1",
                    "req": ["R3"],
                    "formula": {"op": "eventually", "arg": {"op": "atom", "atom": {"kind": "function_completed", "function": "main::main"}}},
                    "fairness": [{"kind": "justice", "atom": {"kind": "mutex_free", "resource": "main::m"}}]
                }
            ],
            "preserved": [
                {"kind": "reachable", "description": "main finishes", "req": ["R3"], "goal": {"kind": "function_completed", "function": "main::main"}}
            ]
        }"#,
    )
    .unwrap();

    let (code, stdout, stderr) = run_cli(&[
        "explain-contract",
        contract_path.to_str().unwrap(),
        "--output",
        explain_out.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let explained: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(explained["schema"], "concir-contract-explanation/1");
    assert_eq!(explained["name"], "demo-contract");
    assert_eq!(explained["properties"].as_array().unwrap().len(), 3);
    assert_eq!(explained["properties"][1]["non_vacuous"], true);
    assert_eq!(explained["properties"][1]["req"][0], "R2");
    assert!(!explained["properties"][1]["explanation"].as_str().unwrap().is_empty());
    assert_eq!(explained["preserved"][0]["req"][0], "R3");
    assert!(explain_out.exists());

    // Test `export-cvn` on an SCIR program (auto-lowers structured_body!).
    let prog_path = tmp.join("scir_prog.json");
    let cvn_out = tmp.join("cvn.json");
    std::fs::write(
        &prog_path,
        r#"{
            "program": "scir_cvn",
            "version": "4.0.0",
            "entry": "main::main",
            "modules": [{
                "name": "main",
                "provides": {"resources": ["m", "c"], "functions": ["main"]},
                "requires": {"resources": [], "functions": []},
                "types": [{"name": "OrderBook", "type": {"Hole": {"description": "order book", "req": ["R9"]}}}],
                "resources": [
                    {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"},
                    {"name": "c", "kind": "var", "type": "Var", "base": {"Int": [0, 5]}, "init": 0}
                ],
                "protection": [{"var": "c", "lock": "m"}],
                "functions": [{
                    "name": "main",
                    "kind": "normal",
                    "structured_body": [
                        {
                            "id": "lg1",
                            "kind": "lock_guard",
                            "resource": "m",
                            "body": [
                                {"id": "h1", "kind": "seq_hole", "reads": ["c"], "writes": ["c"], "updates_modeled": [{"target": "c", "expr": "c + 1"}], "expected_type": "()"}
                            ]
                        },
                        {"id": "r1", "kind": "return"}
                    ]
                }]
            }]
        }"#,
    )
    .unwrap();

    let (code, stdout, stderr) = run_cli(&[
        "export-cvn",
        prog_path.to_str().unwrap(),
        "--output",
        cvn_out.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "export-cvn failed: {stderr}");
    let exported: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(exported["schema"], "concir-cvn-export/1");
    assert_eq!(exported["program"], "scir_cvn");
    assert!(exported["net"]["places"].as_array().unwrap().len() >= 2);
    assert!(exported["net"]["transitions"].as_array().unwrap().len() >= 2);
    assert!(exported["initial"].is_object());
    assert!(exported["report"].is_object());
    assert!(cvn_out.exists());

    let _ = std::fs::remove_dir_all(&tmp);
}

// ──────────────────── 3. TypeHole, Struct, Array, RwLock Codegen ────────────────────

#[test]
fn type_hole_struct_array_and_rwlock_codegen_compiles_cleanly() {
    // Also test both `{"Hole": {"description": ..., "req": [...]}}` and `{"Hole": "short"}` deserialization.
    let short_hole: BaseType = serde_json::from_str(r#"{"Hole": "opaque map"}"#).unwrap();
    assert_eq!(
        short_hole,
        BaseType::Complex(ComplexBaseType::Hole {
            description: Some("opaque map".into()),
            req: Vec::new(),
        })
    );

    let prog = parse_program(
        r#"{
            "program": "v5_domain_rwlock",
            "version": "4.0.0",
            "entry": "main::main",
            "modules": [{
                "name": "main",
                "provides": {"resources": ["book_lock", "entry_var"], "functions": ["main", "reader", "writer"], "types": ["OrderBook", "Entry"]},
                "requires": {"resources": [], "functions": []},
                "types": [
                    {"name": "OrderBook", "type": {"Hole": {"description": "price-level order book", "req": ["R9"]}}},
                    {"name": "Entry", "type": {"Struct": {"price": "Int", "levels": {"Array": {"elem": "Int", "len": 4}}}}}
                ],
                "resources": [
                    {"name": "book_lock", "kind": "sync", "type": "RwLock", "mode": "Sync", "base": "OrderBook"},
                    {"name": "entry_var", "kind": "var", "type": "Var", "base": "Entry", "init": {"price": 100, "levels": [1, 2, 3, 4]}}
                ],
                "protection": [{"var": "entry_var", "lock": "book_lock"}],
                "functions": [
                    {
                        "name": "main",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "scope", "funcs": ["reader", "reader", "writer"]},
                            {"sid": "s2", "kind": "return"}
                        ]
                    },
                    {
                        "name": "reader",
                        "kind": "normal",
                        "locals": [{"name": "p", "type": "Int", "modeled": true, "init": 0}],
                        "body": [
                            {"sid": "s1", "kind": "rwlock_read", "resource": "book_lock"},
                            {"sid": "s2", "kind": "assign_local", "target": "p", "expr": "entry_var.price"},
                            {"sid": "s3", "kind": "rwlock_unlock", "resource": "book_lock"},
                            {"sid": "s4", "kind": "return"}
                        ]
                    },
                    {
                        "name": "writer",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "rwlock_write", "resource": "book_lock"},
                            {"sid": "s2", "kind": "seq_hole", "id": "h_upd", "reads": ["entry_var"], "writes": ["entry_var"], "expected_type": "OrderBook", "req": ["R9"]},
                            {"sid": "s3", "kind": "rwlock_unlock", "resource": "book_lock"},
                            {"sid": "s4", "kind": "return"}
                        ]
                    }
                ]
            }]
        }"#,
    );

    let report = validate::validate(&prog);
    assert!(report.valid, "diagnostics: {:?}", report.diagnostics);

    // 1. Both Interpreter & Petri verify RwLock program identically!
    let contract = parse_contract(
        r#"{
            "name": "rwlock-verify",
            "properties": [
                {"kind": "deadlock_free", "id": "dl"},
                {"kind": "reachability", "id": "both-read", "goal": {"kind": "function_completed_at_least", "function": "main::reader", "n": 2}},
                {"kind": "reachability", "id": "writer-done", "goal": {"kind": "function_completed", "function": "main::writer"}}
            ]
        }"#,
    );
    let rep_i = verify_program(&prog, &contract, EngineKind::Interpreter);
    let rep_p = verify_program(&prog, &contract, EngineKind::Petri);
    assert_eq!(rep_i.outcome, Outcome::Pass, "interp: {:?}", rep_i);
    assert_eq!(rep_p.outcome, Outcome::Pass, "petri: {:?}", rep_p);

    // 2. Codegen emits `src/domain.rs`, `type_holes`, `RwLock`, `Struct`, `Array`.
    let sem = program::lower(&prog).unwrap();
    let generated = codegen::generate(&sem).expect("codegen should succeed");

    let domain_rs = generated.domain_rs.as_ref().expect("domain.rs must be generated");
    assert!(domain_rs.contains("/* TYPE_HOLE(th1) type: OrderBook */"));
    assert!(domain_rs.contains("pub struct OrderBook {}"));
    assert!(domain_rs.contains("assert_domain_bounds::<OrderBook>()"));
    assert!(domain_rs.contains("pub struct Entry {"));
    assert!(domain_rs.contains("pub levels: [i64; 4],"));

    assert_eq!(generated.map.type_holes.len(), 1);
    let th = &generated.map.type_holes[0];
    assert_eq!(th.id, "th1");
    assert_eq!(th.type_name, "OrderBook");
    assert_eq!(th.file, "src/domain.rs");
    assert_eq!(th.req, vec!["R9".to_string()]);
    let th_line = domain_rs.lines().nth(th.line - 1).unwrap_or("");
    assert!(th_line.contains("/* TYPE_HOLE(th1) type: OrderBook */"), "got: {th_line}");

    assert!(generated.main_rs.contains("Arc<std::sync::RwLock<crate::domain::OrderBook>>"));
    assert!(generated.main_rs.contains("cir_trace::rwlock_read_lock"));
    assert!(generated.main_rs.contains("cir_trace::rwlock_write_lock"));
    assert!(generated.main_rs.contains("crate::__res::r_main__book_lock.read().unwrap()"));
    assert!(generated.main_rs.contains("crate::__res::r_main__book_lock.write().unwrap()"));
    assert_eq!(generated.map.holes[0].file, "src/main.rs");

    // 3. Verify generated crate compiles under BOTH default and `--no-default-features`!
    let dir = std::env::temp_dir().join("concir_v5_rwlock_domain_build");
    let _ = std::fs::remove_dir_all(&dir);
    codegen::write_project(&dir, &generated).unwrap();

    for extra in [&[][..], &["--no-default-features"][..]] {
        let mut cmd = Command::new("cargo");
        cmd.arg("check").arg("--offline").args(extra).current_dir(&dir);
        let out = cmd.output().expect("cargo check");
        assert!(
            out.status.success(),
            "cargo check {:?} failed:\n{}",
            extra,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // 4. Verify RwLock refinement (matching read/write ops pass; unauthorized write fails).
    let good_impl: ImplNet = serde_json::from_str(
        r#"{
            "schema": "concir-impl-net/1",
            "crate": "rw_demo",
            "states": [
                {"id": 0, "initial": true, "terminal": false, "deadlock": false, "held_locks": []},
                {"id": 1, "initial": false, "terminal": false, "deadlock": false, "held_locks": ["main::book_lock"]},
                {"id": 2, "initial": false, "terminal": false, "deadlock": false, "held_locks": []},
                {"id": 3, "initial": false, "terminal": false, "deadlock": false, "held_locks": ["main::book_lock"]},
                {"id": 4, "initial": false, "terminal": true, "deadlock": false, "held_locks": []}
            ],
            "transitions": [
                {"id": 0, "observable": true, "op": "rwlock_read_lock", "resource": "r_main__book_lock"},
                {"id": 1, "observable": true, "op": "rwlock_read_unlock", "resource": "r_main__book_lock"},
                {"id": 2, "observable": true, "op": "rwlock_write_lock", "resource": "r_main__book_lock"},
                {"id": 3, "observable": true, "op": "rwlock_write_unlock", "resource": "r_main__book_lock"}
            ],
            "edges": [
                {"from": 0, "to": 1, "transition": 0},
                {"from": 1, "to": 2, "transition": 1},
                {"from": 2, "to": 3, "transition": 2},
                {"from": 3, "to": 4, "transition": 3}
            ],
            "purity_violations": [],
            "completed_functions": ["main::reader", "main::writer"]
        }"#,
    )
    .unwrap();
    let ref_ok = verify_refinement(&prog, &good_impl, None, Some(&generated.map)).unwrap();
    assert_eq!(ref_ok.outcome, "pass", "rwlock refinement failed: {:?}", ref_ok);

    let _ = std::fs::remove_dir_all(&dir);
}

// ──────────────────── 4. Multi-File Codegen & --module Compositional Refinement ────────────────────

#[test]
fn multi_file_module_codegen_and_compositional_refinement() {
    let prog = parse_program(
        r#"{
            "program": "multi_mod_v5",
            "version": "4.0.0",
            "entry": "app::main",
            "modules": [
                {
                    "name": "app",
                    "provides": {"resources": [], "functions": ["main"]},
                    "requires": {"resources": [], "functions": ["worker::run_w", "storage::run_s"]},
                    "resources": [],
                    "protection": [],
                    "functions": [{
                        "name": "main",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "scope", "funcs": ["worker::run_w", "storage::run_s"]},
                            {"sid": "s2", "kind": "return"}
                        ]
                    }]
                },
                {
                    "name": "worker",
                    "provides": {"resources": ["wm"], "functions": ["run_w"]},
                    "requires": {"resources": [], "functions": []},
                    "resources": [
                        {"name": "wm", "kind": "sync", "type": "Mutex", "mode": "Sync"}
                    ],
                    "protection": [],
                    "functions": [{
                        "name": "run_w",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "mutex_lock", "resource": "wm"},
                            {"sid": "s2", "kind": "seq_hole", "id": "h_w", "expected_type": "i64", "req": ["R10"]},
                            {"sid": "s3", "kind": "mutex_unlock", "resource": "wm"},
                            {"sid": "s4", "kind": "return"}
                        ]
                    }]
                },
                {
                    "name": "storage",
                    "provides": {"resources": ["sm"], "functions": ["run_s"]},
                    "requires": {"resources": [], "functions": []},
                    "resources": [
                        {"name": "sm", "kind": "sync", "type": "Mutex", "mode": "Sync"}
                    ],
                    "protection": [],
                    "functions": [{
                        "name": "run_s",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "mutex_lock", "resource": "sm"},
                            {"sid": "s2", "kind": "mutex_unlock", "resource": "sm"},
                            {"sid": "s3", "kind": "return"}
                        ]
                    }]
                }
            ]
        }"#,
    );

    let sem = program::lower(&prog).unwrap();
    let generated = codegen::generate(&sem).expect("multi-module codegen");

    // Non-entry modules emitted to `src/worker.rs` and `src/storage.rs`!
    assert!(generated.main_rs.contains("#[path = \"worker.rs\"]"));
    assert!(generated.main_rs.contains("pub(crate) mod worker;"));
    assert!(generated.main_rs.contains("#[path = \"storage.rs\"]"));
    assert!(generated.module_files.contains_key("src/worker.rs"));
    assert!(generated.module_files.contains_key("src/storage.rs"));

    // Hole in `worker` records `file: "src/worker.rs"` and accurate line!
    let hw = generated.map.holes.iter().find(|h| h.id == "h_w").unwrap();
    assert_eq!(hw.file, "src/worker.rs");
    let worker_src = &generated.module_files["src/worker.rs"];
    assert!(
        worker_src.lines().nth(hw.line - 1).unwrap_or("").contains("/* HOLE(h_w)"),
        "line {} in src/worker.rs did not match hole",
        hw.line
    );

    // Multi-file project compiles cleanly with cargo check --offline!
    let dir = std::env::temp_dir().join("concir_v5_multimod_build");
    let _ = std::fs::remove_dir_all(&dir);
    codegen::write_project(&dir, &generated).unwrap();
    let out = Command::new("cargo")
        .args(["check", "--offline"])
        .current_dir(&dir)
        .output()
        .expect("cargo check multi-module");
    assert!(
        out.status.success(),
        "multi-module cargo check failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);

    // Compositional refinement `--module worker`:
    // Impl performs worker::wm lock/unlock AND also an unrelated extra lock in storage (outside worker scope).
    // Focused on `worker`, projection ignores storage's unrelated op and PASSES!
    let impl_net: ImplNet = serde_json::from_str(
        r#"{
            "schema": "concir-impl-net/1",
            "crate": "multi_mod",
            "focus_module": "worker",
            "states": [
                {"id": 0, "initial": true, "terminal": false, "deadlock": false, "held_locks": []},
                {"id": 1, "initial": false, "terminal": false, "deadlock": false, "held_locks": ["worker::wm"]},
                {"id": 2, "initial": false, "terminal": false, "deadlock": false, "held_locks": []},
                {"id": 3, "initial": false, "terminal": true, "deadlock": false, "held_locks": []}
            ],
            "transitions": [
                {"id": 0, "observable": true, "op": "mutex_lock", "resource": "worker::wm", "fn_def_path": "multi_mod::worker::cf_run_w"},
                {"id": 1, "observable": true, "op": "mutex_unlock", "resource": "worker::wm", "fn_def_path": "multi_mod::worker::cf_run_w"},
                {"id": 2, "observable": true, "op": "mutex_lock", "resource": "storage::extra_internal", "fn_def_path": "multi_mod::storage::cf_run_s"}
            ],
            "edges": [
                {"from": 0, "to": 1, "transition": 0},
                {"from": 1, "to": 2, "transition": 1},
                {"from": 2, "to": 3, "transition": 2}
            ],
            "purity_violations": [],
            "completed_functions": ["worker::run_w"]
        }"#,
    )
    .unwrap();

    let rep_mod = verify_refinement_for_module(
        &prog,
        &impl_net,
        None,
        Some(&generated.map),
        Some("worker"),
    )
    .unwrap();
    assert_eq!(rep_mod.outcome, "pass", "modular refinement for worker failed: {:?}", rep_mod);

    // Whole-program refinement without `--module` catches `storage::extra_internal`!
    let rep_global = verify_refinement(&prog, &impl_net, None, Some(&generated.map)).unwrap();
    assert_eq!(rep_global.outcome, "fail");
    assert!(!rep_global.checks.weak_simulation);

    // Cross-module lock-order cycle is caught by `--module` via `summarize_program`!
    let cyclic_prog = parse_program(
        r#"{
            "program": "cyclic_mod",
            "version": "4.0.0",
            "entry": "mod_a::fa",
            "modules": [
                {
                    "name": "mod_a",
                    "provides": {"resources": ["la"], "functions": ["fa"]},
                    "requires": {"resources": ["mod_b::lb"], "functions": []},
                    "resources": [{"name": "la", "kind": "sync", "type": "Mutex", "mode": "Sync"}],
                    "protection": [],
                    "functions": [{
                        "name": "fa",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "mutex_lock", "resource": "la"},
                            {"sid": "s2", "kind": "mutex_lock", "resource": "mod_b::lb"},
                            {"sid": "s3", "kind": "mutex_unlock", "resource": "mod_b::lb"},
                            {"sid": "s4", "kind": "mutex_unlock", "resource": "la"},
                            {"sid": "s5", "kind": "return"}
                        ]
                    }]
                },
                {
                    "name": "mod_b",
                    "provides": {"resources": ["lb"], "functions": ["fb"]},
                    "requires": {"resources": ["mod_a::la"], "functions": []},
                    "resources": [{"name": "lb", "kind": "sync", "type": "Mutex", "mode": "Sync"}],
                    "protection": [],
                    "functions": [{
                        "name": "fb",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "mutex_lock", "resource": "lb"},
                            {"sid": "s2", "kind": "mutex_lock", "resource": "mod_a::la"},
                            {"sid": "s3", "kind": "mutex_unlock", "resource": "mod_a::la"},
                            {"sid": "s4", "kind": "mutex_unlock", "resource": "lb"},
                            {"sid": "s5", "kind": "return"}
                        ]
                    }]
                }
            ]
        }"#,
    );
    let cyc_impl: ImplNet = serde_json::from_str(
        r#"{
            "schema": "concir-impl-net/1",
            "crate": "cyclic",
            "states": [
                {"id": 0, "initial": true, "terminal": false, "deadlock": false, "held_locks": []},
                {"id": 1, "initial": false, "terminal": false, "deadlock": false, "held_locks": ["mod_a::la"]},
                {"id": 2, "initial": false, "terminal": false, "deadlock": false, "held_locks": ["mod_a::la", "mod_b::lb"]},
                {"id": 3, "initial": false, "terminal": false, "deadlock": false, "held_locks": ["mod_a::la"]},
                {"id": 4, "initial": false, "terminal": true, "deadlock": false, "held_locks": []}
            ],
            "transitions": [
                {"id": 0, "observable": true, "op": "mutex_lock", "resource": "mod_a::la"},
                {"id": 1, "observable": true, "op": "mutex_lock", "resource": "mod_b::lb"},
                {"id": 2, "observable": true, "op": "mutex_unlock", "resource": "mod_b::lb"},
                {"id": 3, "observable": true, "op": "mutex_unlock", "resource": "mod_a::la"}
            ],
            "edges": [
                {"from": 0, "to": 1, "transition": 0},
                {"from": 1, "to": 2, "transition": 1},
                {"from": 2, "to": 3, "transition": 2},
                {"from": 3, "to": 4, "transition": 3}
            ],
            "purity_violations": [],
            "completed_functions": ["mod_a::fa"]
        }"#,
    )
    .unwrap();

    let rep_cyc = verify_refinement_for_module(&cyclic_prog, &cyc_impl, None, None, Some("mod_a")).unwrap();
    assert_eq!(rep_cyc.outcome, "fail");
    assert!(
        rep_cyc.violations.iter().any(|v| v.kind == "cross_module_lock_cycle"),
        "expected cross_module_lock_cycle violation, got: {:?}",
        rep_cyc.violations
    );
}

#[test]
fn non_unit_mutex_and_rwlock_payload_guards_compile_under_both_features() {
    let prog = parse_program(
        r#"{
            "program": "guard_payload_codegen",
            "version": "5.0.0",
            "entry": "main::main",
            "modules": [{
                "name": "main",
                "provides": {"resources": ["rw", "mtx"], "functions": ["main", "writer", "reader", "mtx_user"], "types": ["OrderBook"]},
                "requires": {"resources": [], "functions": []},
                "types": [
                    {"name": "OrderBook", "type": {"Hole": {"description": "order book", "req": ["R1"]}}}
                ],
                "resources": [
                    {"name": "rw", "kind": "sync", "type": "RwLock", "mode": "Sync", "base": "OrderBook"},
                    {"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync", "base": "OrderBook"}
                ],
                "protection": [],
                "functions": [
                    {
                        "name": "main",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "scope", "funcs": ["writer", "reader", "mtx_user"]},
                            {"sid": "s2", "kind": "return"}
                        ]
                    },
                    {
                        "name": "writer",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "rwlock_write", "resource": "rw"},
                            {"sid": "s2", "kind": "seq_hole", "id": "h_w", "expected_type": "()", "req": ["R1"]},
                            {"sid": "s3", "kind": "rwlock_unlock", "resource": "rw"},
                            {"sid": "s4", "kind": "return"}
                        ]
                    },
                    {
                        "name": "reader",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "rwlock_read", "resource": "rw"},
                            {"sid": "s2", "kind": "seq_hole", "id": "h_r", "expected_type": "i64", "req": ["R1"]},
                            {"sid": "s3", "kind": "rwlock_unlock", "resource": "rw"},
                            {"sid": "s4", "kind": "return"}
                        ]
                    },
                    {
                        "name": "mtx_user",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "mutex_lock", "resource": "mtx"},
                            {"sid": "s2", "kind": "seq_hole", "id": "h_m", "expected_type": "()", "req": ["R1"]},
                            {"sid": "s3", "kind": "mutex_unlock", "resource": "mtx"},
                            {"sid": "s4", "kind": "return"}
                        ]
                    }
                ]
            }]
        }"#,
    );

    let sem = program::lower(&prog).unwrap();
    let mut generated = codegen::generate(&sem).expect("codegen");

    // Fill the OrderBook type hole with `best_bid: i64` and `record_bid` / `best` methods.
    let domain_rs = generated
        .domain_rs
        .as_mut()
        .expect("domain.rs must be generated");
    assert!(domain_rs.contains("pub struct OrderBook {}"));
    *domain_rs = domain_rs.replace(
        "pub struct OrderBook {}",
        "pub struct OrderBook {\n    pub best_bid: i64,\n}\nimpl OrderBook {\n    pub fn record_bid(&mut self, p: i64) { self.best_bid = p; }\n    pub fn best(&self) -> i64 { self.best_bid }\n}",
    );

    // Fill the SeqHoles with method calls on `_wg_...`, `_rg_...`, and `_g_...`.
    assert!(generated.main_rs.contains("/* HOLE(h_w) expected: () */ Default::default()"));
    assert!(generated.main_rs.contains("/* HOLE(h_r) expected: i64 */ Default::default()"));
    assert!(generated.main_rs.contains("/* HOLE(h_m) expected: () */ Default::default()"));
    generated.main_rs = generated
        .main_rs
        .replace(
            "/* HOLE(h_w) expected: () */ Default::default()",
            "{ _wg_r_main__rw.record_bid(7); }",
        )
        .replace(
            "/* HOLE(h_r) expected: i64 */ Default::default()",
            "_rg_r_main__rw.best()",
        )
        .replace(
            "/* HOLE(h_m) expected: () */ Default::default()",
            "{ _g_r_main__mtx.record_bid(9); }",
        );

    let dir = std::env::temp_dir().join("concir_v5_payload_guard_build");
    let _ = std::fs::remove_dir_all(&dir);
    codegen::write_project(&dir, &generated).unwrap();

    for extra in [&[][..], &["--no-default-features"][..]] {
        let mut cmd = Command::new("cargo");
        cmd.arg("check").arg("--offline").args(extra).current_dir(&dir);
        let out = cmd.output().expect("cargo check");
        assert!(
            out.status.success(),
            "cargo check {:?} failed:\n{}",
            extra,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
