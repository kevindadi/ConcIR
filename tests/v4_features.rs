//! Comprehensive unit & CLI integration tests for ConcIR v4.0 features:
//! 1. `req` traceability tags & backward compatibility
//! 2. First-class `Op::SeqHole` semantics (`interp` & `petri` agreement)
//! 3. Structured Surface IR (`SCIR` v4.0) lowering & `TraceMap`
//! 4. MIR-clean `codegen` (`SyncVar`, `AtomicI64`/`AtomicBool`, `cir_trace` feature, `SeqHole`)
//! 5. Compositional module summary (`summarize`) & cross-module lock-cycle detection
//! 6. Branch-pruning stuttering refinement (`verify-refinement`)

use std::path::PathBuf;
use std::process::Command;

use concir::ast::Program;
use concir::codegen::generate;
use concir::explore::contract::ContractSpec;
use concir::explore::{verify_program, EngineKind};
use concir::refinement::{verify_refinement, ImplNet};
use concir::sem::outcome::Outcome;
use concir::sem::program;
use concir::summarize::summarize_program;
use concir::validate::validate;

fn backend_bin() -> &'static str {
    env!("CARGO_BIN_EXE_concir-backend")
}

fn tmp(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "concir-v4-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    p
}

fn run_cli(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(backend_bin()).args(args).output().expect("run");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

// ──────────────────── 1 & 2: req tags & SeqHole semantics ────────────────────

#[test]
fn seq_hole_simultaneous_updates_and_engine_agreement() {
    let json = r#"{
        "program": "hole_swap",
        "version": "4.0.0",
        "entry": "main::main",
        "modules": [{
            "name": "main",
            "req": ["R1"],
            "resources": [
                {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync", "req": ["R1"]},
                {"name": "a", "kind": "var", "type": "Var", "base": {"Int": [0, 10]}, "init": 1, "req": ["R2"]},
                {"name": "b", "kind": "var", "type": "Var", "base": {"Int": [0, 10]}, "init": 2, "req": ["R2"]}
            ],
            "protection": [
                {"var": "a", "lock": "m"},
                {"var": "b", "lock": "m"}
            ],
            "functions": [{
                "name": "main",
                "kind": "normal",
                "req": ["R1", "R2"],
                "body": [
                    {"sid": "s1", "kind": "mutex_lock", "resource": "m", "req": ["R1"]},
                    {
                        "sid": "s2",
                        "kind": "seq_hole",
                        "id": "h_swap",
                        "desc": "simultaneous swap a and b",
                        "reads": ["a", "b"],
                        "writes": ["a", "b"],
                        "updates_modeled": [
                            {"target": "a", "expr": "b"},
                            {"target": "b", "expr": "a"}
                        ],
                        "expected_type": "()",
                        "req": ["R2"]
                    },
                    {"sid": "s3", "kind": "mutex_unlock", "resource": "m"},
                    {"sid": "s4", "kind": "return"}
                ]
            }]
        }]
    }"#;

    let prog: Program = serde_json::from_str(json).expect("parse");
    let vrep = validate(&prog);
    assert!(vrep.valid, "diagnostics: {:?}", vrep.diagnostics);

    // Lowering must support SeqHole (no longer Unsupported).
    let sem = program::lower(&prog).expect("lower");
    assert!(sem.unsupported().is_empty(), "unsupported: {:?}", sem.unsupported());

    // Contract verifies simultaneous swap: a == 2 && b == 1 at completion!
    let contract: ContractSpec = serde_json::from_str(r#"{
        "name": "swap_ok",
        "properties": [
            {"kind": "deadlock_free", "id": "dl"},
            {
                "kind": "reachability",
                "id": "swapped",
                "goal": {
                    "kind": "and",
                    "predicates": [
                        {"kind": "var_eq", "resource": "a", "value": 2},
                        {"kind": "var_eq", "resource": "b", "value": 1},
                        {"kind": "function_completed", "function": "main::main"}
                    ]
                }
            }
        ]
    }"#).unwrap();

    let r_petri = verify_program(&prog, &contract, EngineKind::Petri);
    let r_interp = verify_program(&prog, &contract, EngineKind::Interpreter);
    assert_eq!(r_petri.outcome, Outcome::Pass, "{r_petri:?}");
    assert_eq!(r_interp.outcome, Outcome::Pass, "{r_interp:?}");
    assert_eq!(r_petri.states_explored, r_interp.states_explored);
}

#[test]
fn seq_hole_updates_modeled_enforces_lock_protection_and_target_kind() {
    // Unprotected Var in updates_modeled -> E309
    let unprotected = r#"{
        "program": "bad_prot",
        "entry": "main::main",
        "modules": [{
            "name": "main",
            "resources": [
                {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"},
                {"name": "c", "kind": "var", "type": "Var", "base": "Int", "init": 0}
            ],
            "protection": [{"var": "c", "lock": "m"}],
            "functions": [{
                "name": "main",
                "kind": "normal",
                "body": [
                    {
                        "sid": "s1",
                        "kind": "seq_hole",
                        "id": "h1",
                        "updates_modeled": [{"target": "c", "expr": "c + 1"}]
                    },
                    {"sid": "s2", "kind": "return"}
                ]
            }]
        }]
    }"#;
    let p: Program = serde_json::from_str(unprotected).unwrap();
    let rep = validate(&p);
    assert!(!rep.valid);
    assert!(rep.diagnostics.iter().any(|d| d.code == "E309"), "{:?}", rep.diagnostics);

    // Mutex in updates_modeled -> E310
    let mutex_target = r#"{
        "program": "bad_target",
        "entry": "main::main",
        "modules": [{
            "name": "main",
            "resources": [
                {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"}
            ],
            "protection": [],
            "functions": [{
                "name": "main",
                "kind": "normal",
                "body": [
                    {
                        "sid": "s1",
                        "kind": "seq_hole",
                        "id": "h1",
                        "updates_modeled": [{"target": "m", "expr": "1"}]
                    },
                    {"sid": "s2", "kind": "return"}
                ]
            }]
        }]
    }"#;
    let p2: Program = serde_json::from_str(mutex_target).unwrap();
    let rep2 = validate(&p2);
    assert!(!rep2.valid);
    assert!(rep2.diagnostics.iter().any(|d| d.code == "E310"), "{:?}", rep2.diagnostics);
}

// ──────────────────── 3: SCIR v4.0 structured lowering ────────────────────

const SCIR_PRODUCER_CONSUMER: &str = r#"{
    "program": "scir_pc",
    "version": "4.0.0",
    "entry": "main::main",
    "modules": [{
        "name": "main",
        "req": ["R0"],
        "resources": [
            {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"},
            {"name": "cv", "kind": "sync", "type": "Condvar", "mode": "Sync"},
            {"name": "count", "kind": "var", "type": "Var", "base": {"Int": [0, 2]}, "init": 0}
        ],
        "protection": [
            {"var": "count", "lock": "m"}
        ],
        "functions": [
            {
                "name": "main",
                "kind": "normal",
                "structured_body": [
                    {
                        "id": "b_scope",
                        "kind": "stmt",
                        "req": ["R1"],
                        "op": {"kind": "scope", "funcs": ["main::producer", "main::consumer"]}
                    }
                ]
            },
            {
                "name": "producer",
                "kind": "normal",
                "form": "closure",
                "structured_body": [
                    {
                        "id": "b_prod_guard",
                        "kind": "lock_guard",
                        "resource": "m",
                        "req": ["R2"],
                        "body": [
                            {
                                "id": "b_early",
                                "kind": "if_else",
                                "cond": "count == 2",
                                "then_body": [
                                    {"id": "b_ret", "kind": "return"}
                                ],
                                "else_body": [
                                    {
                                        "id": "b_inc",
                                        "kind": "seq_hole",
                                        "reads": ["count"],
                                        "writes": ["count"],
                                        "updates_modeled": [{"target": "count", "expr": "count + 1"}],
                                        "expected_type": "()",
                                        "req": ["R2a"]
                                    },
                                    {
                                        "id": "b_notify",
                                        "kind": "stmt",
                                        "op": {"kind": "condvar_notify_all", "condvar": "cv"}
                                    }
                                ]
                            }
                        ]
                    }
                ]
            },
            {
                "name": "consumer",
                "kind": "normal",
                "form": "closure",
                "structured_body": [
                    {
                        "id": "b_cons_guard",
                        "kind": "lock_guard",
                        "resource": "m",
                        "req": ["R3"],
                        "body": [
                            {
                                "id": "b_wait",
                                "kind": "while_wait",
                                "cond": "count == 0",
                                "condvar": "cv",
                                "lock": "m"
                            },
                            {
                                "id": "b_dec",
                                "kind": "write_shared",
                                "resource": "count",
                                "expr": "count - 1"
                            }
                        ]
                    }
                ]
            }
        ]
    }]
}"#;

#[test]
fn scir_lowering_handles_lock_guard_early_return_if_else_while_wait_and_trace_map() {
    let mut prog: Program = serde_json::from_str(SCIR_PRODUCER_CONSUMER).expect("parse SCIR");
    assert!(prog.has_structured_bodies());
    let trace_map = prog.ensure_lowered().expect("lower SCIR");
    assert!(!prog.has_structured_bodies());
    assert_eq!(trace_map.schema, "concir-trace-map/1");

    // Validation must pass cleanly (including lock/drop pairing on early return inside lock_guard!).
    let vrep = validate(&prog);
    assert!(vrep.valid, "diagnostics: {:?}", vrep.diagnostics);

    // Check TraceMap provenance for the seq_hole inside producer.
    let hole_entry = trace_map
        .entries
        .values()
        .find(|e| e.hole_id.as_deref() == Some("b_inc"))
        .expect("hole entry in trace_map");
    assert_eq!(hole_entry.function, "producer");
    assert!(hole_entry.req.contains(&"R0".to_string()));
    assert!(hole_entry.req.contains(&"R2".to_string()));
    assert!(hole_entry.req.contains(&"R2a".to_string()));

    // Explore directly on original unlowered SCIR as well as lowered program.
    let orig: Program = serde_json::from_str(SCIR_PRODUCER_CONSUMER).unwrap();
    let contract: ContractSpec = serde_json::from_str(r#"{
        "name": "pc_ok",
        "properties": [{"kind": "deadlock_free", "id": "dl"}],
        "preserved": [
            {
                "kind": "reachable",
                "description": "consumer finishes",
                "goal": {"kind": "function_completed", "function": "main::consumer"}
            }
        ]
    }"#).unwrap();

    let rep_petri = verify_program(&orig, &contract, EngineKind::Petri);
    let rep_interp = verify_program(&orig, &contract, EngineKind::Interpreter);
    assert_eq!(rep_petri.outcome, Outcome::Pass, "{rep_petri:?}");
    assert_eq!(rep_interp.outcome, Outcome::Pass, "{rep_interp:?}");

    // Test CLI `lower` subcommand.
    let in_path = tmp("scir_in.json");
    let out_path = tmp("scir_out.json");
    let tm_path = tmp("scir_tm.json");
    std::fs::write(&in_path, SCIR_PRODUCER_CONSUMER).unwrap();
    let (code, stdout, stderr) = run_cli(&[
        "lower",
        in_path.to_str().unwrap(),
        "--out",
        out_path.to_str().unwrap(),
        "--trace-map",
        tm_path.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let lowered_from_stdout: Program = serde_json::from_str(&stdout).expect("parse lowered stdout");
    assert!(!lowered_from_stdout.has_structured_bodies());
    let tm_val: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&tm_path).unwrap()).unwrap();
    assert_eq!(tm_val["schema"], "concir-trace-map/1");
    assert!(!tm_val["entries"].as_object().unwrap().is_empty());
}

// ──────────────────── 4: MIR-clean codegen ────────────────────

#[test]
fn mir_clean_codegen_emits_features_sync_var_atomics_and_enriched_map() {
    let json = r#"{
        "program": "mir_clean",
        "entry": "main::main",
        "modules": [{
            "name": "main",
            "resources": [
                {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"},
                {"name": "v", "kind": "var", "type": "Var", "base": "Int", "init": 0},
                {"name": "a_i", "kind": "var", "type": "Atomic", "base": "Int", "init": 1},
                {"name": "a_b", "kind": "var", "type": "Atomic", "base": "Bool", "init": false}
            ],
            "protection": [{"var": "v", "lock": "m"}],
            "functions": [{
                "name": "main",
                "kind": "normal",
                "locals": [
                    {"name": "old", "type": "Int", "modeled": true},
                    {"name": "flag", "type": "Bool", "modeled": true}
                ],
                "body": [
                    {"sid": "s1", "kind": "mutex_lock", "resource": "m"},
                    {
                        "sid": "s2",
                        "kind": "seq_hole",
                        "id": "h_work",
                        "reads": ["v"],
                        "writes": ["v"],
                        "updates_modeled": [{"target": "v", "expr": "v + 1"}],
                        "expected_type": "Int",
                        "req": ["R4"]
                    },
                    {"sid": "s3", "kind": "mutex_unlock", "resource": "m"},
                    {"sid": "s4", "kind": "atomic_cas", "resource": "a_i", "expected": "1", "desired": "2", "dst": "old"},
                    {"sid": "s5", "kind": "atomic_store", "resource": "a_b", "value": "true"},
                    {"sid": "s6", "kind": "atomic_load", "resource": "a_b", "dst": "flag"},
                    {"sid": "s7", "kind": "return"}
                ]
            }]
        }]
    }"#;

    let prog: Program = serde_json::from_str(json).unwrap();
    assert!(validate(&prog).valid);
    let sem = program::lower(&prog).unwrap();
    let generated = generate(&sem).expect("codegen");

    // 1. Cargo.toml features
    assert!(generated.cargo_toml.contains("[features]"));
    assert!(generated.cargo_toml.contains("default = [\"cir_trace\"]"));
    assert!(generated.cargo_toml.contains("cir_trace = []"));

    // 2. cir_trace.rs feature-gated EVENTS & SyncVar
    assert!(generated.trace_rs.contains("#[cfg(feature = \"cir_trace\")]"));
    assert!(generated.trace_rs.contains("pub struct SyncVar<T>"));
    assert!(generated.trace_rs.contains("UnsafeCell<T>"));

    // 3. Shared fields use SyncVar, AtomicI64, AtomicBool (NO fake Mutex on Var/Atomic!)
    assert!(generated.main_rs.contains("Arc<crate::cir_trace::SyncVar<i64>>"));
    assert!(generated.main_rs.contains("Arc<std::sync::atomic::AtomicI64>"));
    assert!(generated.main_rs.contains("Arc<std::sync::atomic::AtomicBool>"));
    assert!(generated.main_rs.contains("Ordering::SeqCst"));
    assert!(generated.main_rs.contains("shared.r_main__v.write("));
    assert!(generated.main_rs.contains("shared.r_main__v.read()"));
    assert!(generated.main_rs.contains("/* HOLE(h_work) expected: i64 */ Default::default()"));

    // 4. Enriched codegen.json map
    assert_eq!(generated.map.resources.len(), 4);
    assert!(generated.map.resources.iter().any(|r| r.fqn == "main::m" && r.kind == "Mutex"));
    assert!(generated.map.resources.iter().any(|r| r.fqn == "main::v" && r.kind == "Var"));
    assert!(generated.map.resources.iter().any(|r| r.fqn == "main::a_i" && r.kind == "Atomic"));

    assert_eq!(generated.map.holes.len(), 1);
    let h = &generated.map.holes[0];
    assert_eq!(h.id, "h_work");
    assert_eq!(h.module, "main");
    assert_eq!(h.function, "main");
    assert_eq!(h.sid, "s2");
    assert_eq!(h.expected, "i64");
    assert!(h.line > 0);
    assert_eq!(h.reads, vec!["v"]);
    assert_eq!(h.writes, vec!["v"]);
    assert_eq!(h.req, vec!["R4"]);
    // Verify line number actually points at the HOLE comment!
    let actual_line = generated.main_rs.lines().nth(h.line - 1).unwrap_or("");
    assert!(
        actual_line.contains("/* HOLE(h_work)"),
        "line {} was: {actual_line}",
        h.line
    );
}

// ──────────────────── 5: Compositional module summary (`summarize`) ────────────────────

#[test]
fn summarize_detects_cross_module_lock_cycle_and_collapses_quotient() {
    let cyclic_json = r#"{
        "program": "cross_mod_abba",
        "entry": "mod_a::main",
        "modules": [
            {
                "name": "mod_a",
                "provides": {"resources": ["la"], "functions": ["main", "lock_a"]},
                "requires": {"resources": ["mod_b::lb"], "functions": ["mod_b::lock_b"]},
                "resources": [
                    {"name": "la", "kind": "sync", "type": "Mutex", "mode": "Sync"}
                ],
                "protection": [],
                "functions": [
                    {
                        "name": "main",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "nop"},
                            {"sid": "s2", "kind": "mutex_lock", "resource": "la"},
                            {"sid": "s3", "kind": "call", "func": "mod_b::lock_b"},
                            {"sid": "s4", "kind": "mutex_unlock", "resource": "la"},
                            {"sid": "s5", "kind": "return"}
                        ]
                    },
                    {
                        "name": "lock_a",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "mutex_lock", "resource": "la"},
                            {"sid": "s2", "kind": "mutex_unlock", "resource": "la"},
                            {"sid": "s3", "kind": "return"}
                        ]
                    }
                ]
            },
            {
                "name": "mod_b",
                "provides": {"resources": ["lb"], "functions": ["lock_b", "entry_b"]},
                "requires": {"resources": ["mod_a::la"], "functions": ["mod_a::lock_a"]},
                "resources": [
                    {"name": "lb", "kind": "sync", "type": "Mutex", "mode": "Sync"}
                ],
                "protection": [],
                "functions": [
                    {
                        "name": "lock_b",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "mutex_lock", "resource": "lb"},
                            {"sid": "s2", "kind": "mutex_unlock", "resource": "lb"},
                            {"sid": "s3", "kind": "return"}
                        ]
                    },
                    {
                        "name": "entry_b",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "mutex_lock", "resource": "lb"},
                            {"sid": "s2", "kind": "call", "func": "mod_a::lock_a"},
                            {"sid": "s3", "kind": "mutex_unlock", "resource": "lb"},
                            {"sid": "s4", "kind": "return"}
                        ]
                    }
                ]
            }
        ]
    }"#;

    let prog: Program = serde_json::from_str(cyclic_json).unwrap();
    let vrep = validate(&prog);
    assert!(vrep.valid, "{:?}", vrep.diagnostics);

    let summary = summarize_program(&prog, None).expect("summarize");
    assert_eq!(summary.schema, "concir-module-summary/1");
    assert!(summary.cross_module_lock_cycle);
    assert_eq!(summary.lock_cycles.len(), 1);
    assert_eq!(summary.lock_cycles[0], vec!["mod_a::la", "mod_b::lb"]);

    // CLI exits with code 1 when a cycle is detected.
    let cyc_path = tmp("cyclic.json");
    let sum_out = tmp("summary.json");
    std::fs::write(&cyc_path, cyclic_json).unwrap();
    let (code, stdout, stderr) = run_cli(&[
        "summarize",
        cyc_path.to_str().unwrap(),
        "--module",
        "mod_a",
        "--out",
        sum_out.to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "expected exit 1 on lock cycle, stderr: {stderr}");
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["schema"], "concir-module-summary/1");
    assert_eq!(v["cross_module_lock_cycle"], true);
    assert_eq!(v["modules"].as_array().unwrap().len(), 1);
    assert_eq!(v["modules"][0]["name"], "mod_a");
}

#[test]
fn summarize_acyclic_program_exits_zero() {
    let prog_path = tmp("acyclic.json");
    std::fs::write(&prog_path, SCIR_PRODUCER_CONSUMER).unwrap();
    let (code, stdout, stderr) = run_cli(&["summarize", prog_path.to_str().unwrap()]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["schema"], "concir-module-summary/1");
    assert_eq!(v["cross_module_lock_cycle"], false);
    assert!(!v["modules"][0]["quotient_transitions"].as_array().unwrap().is_empty());
}

// ──────────────────── 6: Stuttering refinement (`verify-refinement`) ────────────────────

const SPEC_TWO_LOCKS: &str = r#"{
    "program": "two_locks",
    "entry": "main::main",
    "modules": [{
        "name": "main",
        "resources": [
            {"name": "a", "kind": "sync", "type": "Mutex", "mode": "Sync"},
            {"name": "b", "kind": "sync", "type": "Mutex", "mode": "Sync"}
        ],
        "protection": [],
        "functions": [
            {
                "name": "main",
                "kind": "normal",
                "body": [
                    {"sid": "s1", "kind": "scope", "funcs": ["main::worker"]},
                    {"sid": "s2", "kind": "return"}
                ]
            },
            {
                "name": "worker",
                "kind": "normal",
                "form": "closure",
                "body": [
                    {"sid": "s1", "kind": "mutex_lock", "resource": "a"},
                    {"sid": "s2", "kind": "mutex_lock", "resource": "b"},
                    {"sid": "s3", "kind": "seq_hole", "id": "h1", "req": ["R7"]},
                    {"sid": "s4", "kind": "mutex_unlock", "resource": "b"},
                    {"sid": "s5", "kind": "mutex_unlock", "resource": "a"},
                    {"sid": "s6", "kind": "return"}
                ]
            }
        ]
    }]
}"#;

const CONTRACT_TWO_LOCKS: &str = r#"{
    "name": "two_locks_contract",
    "properties": [{"kind": "deadlock_free", "id": "dl"}],
    "preserved": [
        {
            "kind": "reachable",
            "description": "worker completes",
            "goal": {"kind": "function_completed", "function": "main::worker"}
        }
    ]
}"#;

#[test]
fn refinement_passes_for_valid_impl_with_tau_and_spawn_join() {
    let spec: Program = serde_json::from_str(SPEC_TWO_LOCKS).unwrap();
    let contract: ContractSpec = serde_json::from_str(CONTRACT_TWO_LOCKS).unwrap();
    let sem = program::lower(&spec).unwrap();
    let generated = generate(&sem).unwrap();

    let impl_net: ImplNet = serde_json::from_str(r#"{
        "schema": "concir-impl-net/1",
        "crate": "cir_generated",
        "entry_mode": "main",
        "truncated": false,
        "transitions": [
            {"id": 0, "name": "spawn_worker", "observable": true, "op": "spawn", "resource": "main::cf_worker"},
            {"id": 1, "name": "lock_a", "observable": true, "op": "mutex_lock", "resource": "shared.r_main__a", "span": "src/main.rs:20:5"},
            {"id": 2, "name": "tau_work", "observable": false, "op": "tau"},
            {"id": 3, "name": "lock_b", "observable": true, "op": "mutex_lock", "resource": "shared.r_main__b", "span": "src/main.rs:25:5"},
            {"id": 4, "name": "unlock_b", "observable": true, "op": "mutex_unlock", "resource": "shared.r_main__b", "span": "src/main.rs:30:5"},
            {"id": 5, "name": "unlock_a", "observable": true, "op": "mutex_unlock", "resource": "shared.r_main__a", "span": "src/main.rs:35:5"},
            {"id": 6, "name": "join_worker", "observable": true, "op": "join", "resource": "main::cf_worker"}
        ],
        "states": [
            {"id": 0, "initial": true, "terminal": false, "deadlock": false, "held_locks": []},
            {"id": 1, "initial": false, "terminal": false, "deadlock": false, "held_locks": []},
            {"id": 2, "initial": false, "terminal": false, "deadlock": false, "held_locks": ["main::a"]},
            {"id": 3, "initial": false, "terminal": false, "deadlock": false, "held_locks": ["main::a"]},
            {"id": 4, "initial": false, "terminal": false, "deadlock": false, "held_locks": ["main::a", "main::b"]},
            {"id": 5, "initial": false, "terminal": false, "deadlock": false, "held_locks": ["main::a"]},
            {"id": 6, "initial": false, "terminal": false, "deadlock": false, "held_locks": []},
            {"id": 7, "initial": false, "terminal": true, "deadlock": false, "held_locks": []}
        ],
        "edges": [
            {"from": 0, "to": 1, "transition": 0},
            {"from": 1, "to": 2, "transition": 1},
            {"from": 2, "to": 3, "transition": 2},
            {"from": 3, "to": 4, "transition": 3},
            {"from": 4, "to": 5, "transition": 4},
            {"from": 5, "to": 6, "transition": 5},
            {"from": 6, "to": 7, "transition": 6}
        ],
        "purity_violations": [],
        "completed_functions": ["cir_generated::main::cf_worker", "cir_generated::main::cf_main"]
    }"#).unwrap();

    let rep = verify_refinement(&spec, &impl_net, Some(&contract), Some(&generated.map)).unwrap();
    assert_eq!(rep.schema, "concir-refinement-report/1");
    assert_eq!(rep.outcome, "pass", "{rep:?}");
    assert!(rep.checks.hole_purity);
    assert!(rep.checks.weak_simulation);
    assert!(rep.checks.deadlock_free);
    assert!(rep.checks.lock_divergence_free);
    assert!(rep.checks.preserved_goals);
    assert!(rep.violations.is_empty());
}

#[test]
fn refinement_allows_branch_pruning_and_rejects_swapped_locks_purity_deadlock_divergence_and_noop() {
    let spec: Program = serde_json::from_str(SPEC_TWO_LOCKS).unwrap();
    let contract: ContractSpec = serde_json::from_str(CONTRACT_TWO_LOCKS).unwrap();
    let sem = program::lower(&spec).unwrap();
    let generated = generate(&sem).unwrap();

    // 1. Swapped lock order (b before a) -> weak_simulation fails with shortest witness!
    let swapped: ImplNet = serde_json::from_str(r#"{
        "schema": "concir-impl-net/1",
        "transitions": [
            {"id": 0, "observable": true, "op": "mutex_lock", "resource": "r_main__b", "span": "src/main.rs:10:5"},
            {"id": 1, "observable": true, "op": "mutex_lock", "resource": "r_main__a", "span": "src/main.rs:11:5"}
        ],
        "states": [
            {"id": 0, "initial": true, "held_locks": []},
            {"id": 1, "held_locks": ["main::b"]},
            {"id": 2, "terminal": true, "held_locks": []}
        ],
        "edges": [
            {"from": 0, "to": 1, "transition": 0},
            {"from": 1, "to": 2, "transition": 1}
        ],
        "completed_functions": ["main::cf_worker"]
    }"#).unwrap();
    let rep_swap = verify_refinement(&spec, &swapped, Some(&contract), Some(&generated.map)).unwrap();
    assert_eq!(rep_swap.outcome, "fail");
    assert!(!rep_swap.checks.weak_simulation);
    let v = rep_swap
        .violations
        .iter()
        .find(|v| v.kind == "unexpected_sync_step")
        .expect("unexpected_sync_step violation");
    assert_eq!(v.witness_trace.len(), 1);
    assert_eq!(v.witness_trace[0].op, "mutex_lock");
    assert_eq!(v.witness_trace[0].resource, "main::b");
    assert!(v.expected_spec_ops.contains(&"mutex_lock(main::a)".to_string()));

    // 2. Hole purity violation -> attributes sid & req_clauses from codegen.json!
    let impure: ImplNet = serde_json::from_str(r#"{
        "schema": "concir-impl-net/1",
        "transitions": [
            {"id": 0, "observable": true, "op": "mutex_lock", "resource": "main::a"},
            {"id": 1, "observable": true, "op": "mutex_lock", "resource": "main::b"},
            {"id": 2, "observable": true, "op": "mutex_unlock", "resource": "main::b"},
            {"id": 3, "observable": true, "op": "mutex_unlock", "resource": "main::a"}
        ],
        "states": [
            {"id": 0, "initial": true}, {"id": 1}, {"id": 2}, {"id": 3}, {"id": 4, "terminal": true}
        ],
        "edges": [
            {"from": 0, "to": 1, "transition": 0},
            {"from": 1, "to": 2, "transition": 1},
            {"from": 2, "to": 3, "transition": 2},
            {"from": 3, "to": 4, "transition": 3}
        ],
        "purity_violations": [{
            "hole_id": "h1",
            "op": "mutex_lock",
            "span": "src/main.rs:42:5",
            "message": "Mutex::lock inside hole h1"
        }],
        "completed_functions": ["main::cf_worker"]
    }"#).unwrap();
    let rep_impure = verify_refinement(&spec, &impure, Some(&contract), Some(&generated.map)).unwrap();
    assert_eq!(rep_impure.outcome, "fail");
    assert!(!rep_impure.checks.hole_purity);
    let pv = rep_impure
        .violations
        .iter()
        .find(|v| v.kind == "sync_in_hole")
        .unwrap();
    assert_eq!(pv.hole_id.as_deref(), Some("h1"));
    assert_eq!(pv.sid.as_deref(), Some("s3"));
    assert_eq!(pv.req_clauses, vec!["R7"]);

    // 3. Deadlock + lock-divergence + missing preserved goal + vacuous no-op!
    let div_and_dl: ImplNet = serde_json::from_str(r#"{
        "schema": "concir-impl-net/1",
        "transitions": [
            {"id": 0, "observable": true, "op": "mutex_lock", "resource": "main::a"},
            {"id": 1, "observable": false, "op": "tau", "hole_id": "h1", "span": "src/main.rs:43:5"}
        ],
        "states": [
            {"id": 0, "initial": true, "held_locks": []},
            {"id": 1, "held_locks": ["main::a"]},
            {"id": 2, "deadlock": true, "held_locks": ["main::a"]}
        ],
        "edges": [
            {"from": 0, "to": 1, "transition": 0},
            {"from": 1, "to": 1, "transition": 1},
            {"from": 1, "to": 2, "transition": 1}
        ],
        "completed_functions": []
    }"#).unwrap();
    let rep_bad = verify_refinement(&spec, &div_and_dl, Some(&contract), Some(&generated.map)).unwrap();
    assert_eq!(rep_bad.outcome, "fail");
    assert!(!rep_bad.checks.deadlock_free);
    assert!(!rep_bad.checks.lock_divergence_free);
    assert!(!rep_bad.checks.preserved_goals);

    // 4. Truncated impl_net with no violations -> outcome "unknown" (exit 3 via CLI).
    let truncated: ImplNet = serde_json::from_str(r#"{
        "schema": "concir-impl-net/1",
        "truncated": true,
        "transitions": [
            {"id": 0, "observable": true, "op": "mutex_lock", "resource": "main::a"}
        ],
        "states": [
            {"id": 0, "initial": true},
            {"id": 1, "terminal": true}
        ],
        "edges": [
            {"from": 0, "to": 1, "transition": 0}
        ],
        "completed_functions": ["cf_worker"]
    }"#).unwrap();
    let spec_p = tmp("ref_spec.json");
    let net_p = tmp("ref_trunc.json");
    let contract_p = tmp("ref_contract.json");
    let out_p = tmp("ref_report.json");
    std::fs::write(&spec_p, SPEC_TWO_LOCKS).unwrap();
    std::fs::write(&net_p, serde_json::to_string(&truncated).unwrap()).unwrap();
    std::fs::write(&contract_p, CONTRACT_TWO_LOCKS).unwrap();
    let (code, stdout, stderr) = run_cli(&[
        "verify-refinement",
        "--spec",
        spec_p.to_str().unwrap(),
        "--impl-net",
        net_p.to_str().unwrap(),
        "--contract",
        contract_p.to_str().unwrap(),
        "--out",
        out_p.to_str().unwrap(),
    ]);
    assert_eq!(code, 3, "expected exit 3 for truncated net, stderr: {stderr}\nstdout: {stdout}");
    let rep_json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(rep_json["outcome"], "unknown");
}

#[test]
fn refinement_fallback_explores_places_and_transitions_when_states_empty() {
    let spec: Program = serde_json::from_str(SPEC_TWO_LOCKS).unwrap();
    let net_places_only: ImplNet = serde_json::from_str(r#"{
        "schema": "concir-impl-net/1",
        "places": [
            {"id": 0, "name": "p0", "class": "control", "initial_tokens": 1},
            {"id": 1, "name": "p1", "class": "control", "initial_tokens": 0},
            {"id": 2, "name": "p2", "class": "control", "initial_tokens": 0},
            {"id": 3, "name": "p3", "class": "control", "initial_tokens": 0},
            {"id": 4, "name": "end", "class": "control", "initial_tokens": 0},
            {"id": 5, "name": "r_main__a", "class": "lock", "resource_kind": "Mutex", "initial_tokens": 1},
            {"id": 6, "name": "r_main__b", "class": "lock", "resource_kind": "Mutex", "initial_tokens": 1}
        ],
        "transitions": [
            {"id": 0, "observable": true, "op": "mutex_lock", "resource": "r_main__a", "inputs": [0, 5], "outputs": [1]},
            {"id": 1, "observable": true, "op": "mutex_lock", "resource": "r_main__b", "inputs": [1, 6], "outputs": [2]},
            {"id": 2, "observable": true, "op": "mutex_unlock", "resource": "r_main__b", "inputs": [2], "outputs": [3, 6]},
            {"id": 3, "observable": true, "op": "mutex_unlock", "resource": "r_main__a", "inputs": [3], "outputs": [4, 5]}
        ],
        "states": [],
        "edges": [],
        "completed_functions": ["main::cf_worker"]
    }"#).unwrap();

    let rep = verify_refinement(&spec, &net_places_only, None, None).unwrap();
    assert_eq!(rep.outcome, "pass", "{rep:?}");
    assert_eq!(rep.impl_states, 5);
}

#[test]
fn refinement_permits_branch_pruning_and_rejects_vacuous_noop() {
    // Spec has two threads racing on `a` then `b` vs single thread; even simpler:
    // two concurrent threads t1 (locks a) and t2 (locks b) can interleave either
    // a-first or b-first. Implementation that deterministically only performs
    // a-first then b-second (pruning the b-first interleaving) MUST pass!
    let spec_json = r#"{
        "program": "branch_prune",
        "entry": "main::main",
        "modules": [{
            "name": "main",
            "resources": [
                {"name": "a", "kind": "sync", "type": "Mutex", "mode": "Sync"},
                {"name": "b", "kind": "sync", "type": "Mutex", "mode": "Sync"}
            ],
            "protection": [],
            "functions": [
                {"name": "main", "kind": "normal", "body": [
                    {"sid": "s1", "kind": "scope", "funcs": ["main::t1", "main::t2"]},
                    {"sid": "s2", "kind": "return"}
                ]},
                {"name": "t1", "kind": "normal", "form": "closure", "body": [
                    {"sid": "s1", "kind": "mutex_lock", "resource": "a"},
                    {"sid": "s2", "kind": "mutex_unlock", "resource": "a"},
                    {"sid": "s3", "kind": "return"}
                ]},
                {"name": "t2", "kind": "normal", "form": "closure", "body": [
                    {"sid": "s1", "kind": "mutex_lock", "resource": "b"},
                    {"sid": "s2", "kind": "mutex_unlock", "resource": "b"},
                    {"sid": "s3", "kind": "return"}
                ]}
            ]
        }]
    }"#;
    let spec: Program = serde_json::from_str(spec_json).unwrap();

    // Pruned impl: only takes the t1-then-t2 interleaving (prunes t2-first branch).
    let pruned: ImplNet = serde_json::from_str(r#"{
        "schema": "concir-impl-net/1",
        "transitions": [
            {"id": 0, "observable": true, "op": "mutex_lock", "resource": "main::a"},
            {"id": 1, "observable": true, "op": "mutex_unlock", "resource": "main::a"},
            {"id": 2, "observable": true, "op": "mutex_lock", "resource": "main::b"},
            {"id": 3, "observable": true, "op": "mutex_unlock", "resource": "main::b"}
        ],
        "states": [
            {"id": 0, "initial": true},
            {"id": 1, "held_locks": ["main::a"]},
            {"id": 2},
            {"id": 3, "held_locks": ["main::b"]},
            {"id": 4, "terminal": true}
        ],
        "edges": [
            {"from": 0, "to": 1, "transition": 0},
            {"from": 1, "to": 2, "transition": 1},
            {"from": 2, "to": 3, "transition": 2},
            {"from": 3, "to": 4, "transition": 3}
        ],
        "completed_functions": ["main::cf_t1", "main::cf_t2"]
    }"#).unwrap();
    let rep_ok = verify_refinement(&spec, &pruned, None, None).unwrap();
    assert_eq!(rep_ok.outcome, "pass", "{rep_ok:?}");

    // Vacuous no-op impl: zero observable sync ops -> rejected by non-triviality check!
    let noop: ImplNet = serde_json::from_str(r#"{
        "schema": "concir-impl-net/1",
        "transitions": [{"id": 0, "observable": false, "op": "tau"}],
        "states": [
            {"id": 0, "initial": true},
            {"id": 1, "terminal": true}
        ],
        "edges": [{"from": 0, "to": 1, "transition": 0}],
        "completed_functions": ["main::cf_t1", "main::cf_t2"]
    }"#).unwrap();
    let rep_noop = verify_refinement(&spec, &noop, None, None).unwrap();
    assert_eq!(rep_noop.outcome, "fail", "{rep_noop:?}");
    assert!(!rep_noop.checks.preserved_goals);
}

#[test]
fn generated_mir_clean_crate_compiles_with_and_without_cir_trace_feature() {
    let json = r#"{
        "program": "compile_check",
        "entry": "main::main",
        "modules": [{
            "name": "main",
            "resources": [
                {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"},
                {"name": "cv", "kind": "sync", "type": "Condvar", "mode": "Sync"},
                {"name": "v", "kind": "var", "type": "Var", "base": "Int", "init": 0},
                {"name": "a_i", "kind": "var", "type": "Atomic", "base": "Int", "init": 1},
                {"name": "a_b", "kind": "var", "type": "Atomic", "base": "Bool", "init": false}
            ],
            "protection": [{"var": "v", "lock": "m"}],
            "functions": [{
                "name": "main",
                "kind": "normal",
                "locals": [
                    {"name": "old", "type": "Int", "modeled": true},
                    {"name": "flag", "type": "Bool", "modeled": true}
                ],
                "body": [
                    {"sid": "s1", "kind": "mutex_lock", "resource": "m"},
                    {
                        "sid": "s2",
                        "kind": "seq_hole",
                        "id": "h_inc",
                        "reads": ["v"],
                        "writes": ["v"],
                        "updates_modeled": [{"target": "v", "expr": "v + 1"}],
                        "expected_type": "()"
                    },
                    {"sid": "s3", "kind": "condvar_notify", "condvar": "cv"},
                    {"sid": "s4", "kind": "mutex_unlock", "resource": "m"},
                    {"sid": "s5", "kind": "atomic_cas", "resource": "a_i", "expected": "1", "desired": "2", "dst": "old"},
                    {"sid": "s6", "kind": "atomic_store", "resource": "a_b", "value": "true"},
                    {"sid": "s7", "kind": "atomic_load", "resource": "a_b", "dst": "flag"},
                    {"sid": "s8", "kind": "return"}
                ]
            }]
        }]
    }"#;

    let prog: Program = serde_json::from_str(json).unwrap();
    let sem = program::lower(&prog).unwrap();
    let generated = generate(&sem).expect("codegen");
    let dir = tmp("crate_compile");
    concir::codegen::write_project(&dir, &generated).expect("write_project");

    // Compile with default features (cir_trace enabled) using rustc directly.
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let out_with = dir.join("with_trace.rlib");
    let st1 = Command::new(&rustc)
        .args([
            "--edition=2021",
            "--cfg",
            r#"feature="cir_trace""#,
            "--crate-type=bin",
            "-o",
            out_with.to_str().unwrap(),
            dir.join("src/main.rs").to_str().unwrap(),
        ])
        .output()
        .expect("invoke rustc with cir_trace");
    assert!(
        st1.status.success(),
        "rustc with cir_trace failed:\n{}",
        String::from_utf8_lossy(&st1.stderr)
    );

    // Compile WITHOUT cir_trace feature (MIR-clean mode for spn).
    let out_no = dir.join("no_trace.rlib");
    let st2 = Command::new(&rustc)
        .args([
            "--edition=2021",
            "--crate-type=bin",
            "-o",
            out_no.to_str().unwrap(),
            dir.join("src/main.rs").to_str().unwrap(),
        ])
        .output()
        .expect("invoke rustc without cir_trace");
    assert!(
        st2.status.success(),
        "rustc without cir_trace failed:\n{}",
        String::from_utf8_lossy(&st2.stderr)
    );
}
