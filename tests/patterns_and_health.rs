//! Tests for `ConstraintPattern` DSL (`always`, `exclude`, `at_most`,
//! `precedence`, `triggers`, `can_reach`, `always_can_reach`) and
//! `PropertySpec::Health` pure logic-driven bug detection across both
//! `EngineKind::Petri` (`UnipnEngine` over `unipn::hln`) and `EngineKind::Interpreter`.

use concir::ast::Program;
use concir::explore::contract::ContractSpec;
use concir::explore::{verify, verify_program, EngineKind};
use concir::interp::Interpreter;
use concir::petri::PetriEngine;
use concir::sem::outcome::Outcome;
use concir::sem::program;

fn parse_program(json: &str) -> Program {
    serde_json::from_str(json).expect("valid program JSON")
}

fn parse_contract(json: &str) -> ContractSpec {
    serde_json::from_str(json).expect("valid contract JSON")
}

fn counter_program() -> Program {
    parse_program(
        r#"{
            "program": "patterns_counter",
            "version": "4.0.0",
            "entry": "main::main",
            "modules": [{
                "name": "main",
                "resources": [
                    {"name": "m1", "kind": "sync", "type": "Mutex", "mode": "Sync"},
                    {"name": "m2", "kind": "sync", "type": "Mutex", "mode": "Sync"},
                    {"name": "ready", "kind": "var", "type": "Var", "base": {"Int": [0, 1]}, "init": 0},
                    {"name": "done", "kind": "var", "type": "Var", "base": {"Int": [0, 2]}, "init": 0}
                ],
                "protection": [
                    {"var": "ready", "lock": "m1"}
                ],
                "functions": [
                    {
                        "name": "main",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "scope", "funcs": ["producer", "consumer"]},
                            {"sid": "s2", "kind": "return"}
                        ]
                    },
                    {
                        "name": "producer",
                        "kind": "normal",
                        "structured_body": [
                            {
                                "id": "p1",
                                "kind": "lock_guard",
                                "resource": "m1",
                                "body": [
                                    {"id": "p2", "kind": "write_shared", "resource": "ready", "expr": "1"}
                                ]
                            },
                            {"id": "p3", "kind": "return"}
                        ]
                    },
                    {
                        "name": "consumer",
                        "kind": "normal",
                        "structured_body": [
                            {
                                "id": "c1",
                                "kind": "while_loop",
                                "cond": "done == 0",
                                "body": [
                                    {
                                        "id": "c2",
                                        "kind": "lock_guard",
                                        "resource": "m1",
                                        "body": [
                                            {
                                                "id": "c3",
                                                "kind": "if_else",
                                                "cond": "ready == 1",
                                                "then_body": [
                                                    {"id": "c4", "kind": "write_shared", "resource": "done", "expr": "1"}
                                                ],
                                                "else_body": []
                                            }
                                        ]
                                    }
                                ]
                            },
                            {"id": "c5", "kind": "return"}
                        ]
                    }
                ]
            }]
        }"#,
    )
}

#[test]
fn all_seven_constraint_patterns_and_explain_determinism() {
    let prog = counter_program();
    let contract_json = r#"{
        "name": "pattern-suite-pass",
        "bounds": {"max_states": 5000, "max_depth": 100},
        "properties": [
            {
                "id": "pat-always",
                "kind": "pattern",
                "pattern": "always",
                "cond": {"kind": "var_cmp", "resource": "main::done", "op": "<=", "value": 1}
            },
            {
                "id": "pat-exclude",
                "kind": "pattern",
                "pattern": "exclude",
                "preds": [
                    {"kind": "var_eq", "resource": "main::done", "value": 1},
                    {"kind": "var_eq", "resource": "main::ready", "value": 0}
                ]
            },
            {
                "id": "pat-at-most",
                "kind": "pattern",
                "pattern": "at_most",
                "max": 1,
                "conds": [
                    {"kind": "var_eq", "resource": "main::done", "value": 1},
                    {"kind": "var_eq", "resource": "main::done", "value": 2},
                    {"kind": "var_eq", "resource": "main::ready", "value": 0}
                ]
            },
            {
                "id": "pat-precedence",
                "kind": "pattern",
                "pattern": "precedence",
                "effect": {"kind": "var_eq", "resource": "main::done", "value": 1},
                "cause": {"kind": "var_eq", "resource": "main::ready", "value": 1}
            },
            {
                "id": "pat-triggers",
                "kind": "pattern",
                "pattern": "triggers",
                "cause": {"kind": "var_eq", "resource": "main::ready", "value": 1},
                "effect": {"kind": "var_eq", "resource": "main::done", "value": 1},
                "fairness": [
                    {"kind": "justice", "atom": {"kind": "function_completed", "function": "main::consumer"}}
                ]
            },
            {
                "id": "pat-can-reach",
                "kind": "pattern",
                "pattern": "can_reach",
                "goal": {"kind": "var_eq", "resource": "main::done", "value": 1}
            },
            {
                "id": "pat-always-can-reach",
                "kind": "pattern",
                "pattern": "always_can_reach",
                "goal": {"kind": "var_eq", "resource": "main::done", "value": 1}
            }
        ]
    }"#;

    let contract = parse_contract(contract_json);
    let e1 = serde_json::to_string(&contract.explain()).unwrap();
    let e2 = serde_json::to_string(&contract.explain()).unwrap();
    assert_eq!(e1, e2, "explain() must be deterministic");
    assert!(e1.contains("Pattern(always)"));
    assert!(e1.contains("Pattern(exclude)"));
    assert!(e1.contains("Pattern(at_most 1)"));
    assert!(e1.contains("Pattern(precedence)"));
    assert!(e1.contains("Pattern(triggers)"));
    assert!(e1.contains("Pattern(can_reach)"));
    assert!(e1.contains("Pattern(always_can_reach)"));

    for engine in [EngineKind::Petri, EngineKind::Interpreter] {
        let rep = verify_program(&prog, &contract, engine);
        assert_eq!(rep.outcome, Outcome::Pass, "engine {:?}: {:?}", engine, rep);
        for pr in &rep.properties {
            assert!(!pr.vacuous, "property {} unexpectedly vacuous", pr.id);
        }
    }

    // Precedence non-vacuity rejection: when `effect` (done == 2) is never reachable,
    // `Precedence` must reject with `Outcome::Fail` + `vacuous = true`!
    let vac_prec = parse_contract(
        r#"{
            "name": "precedence-nonvac",
            "properties": [
                {
                    "id": "prec-vacuous",
                    "kind": "pattern",
                    "pattern": "precedence",
                    "non_vacuous": true,
                    "effect": {"kind": "var_eq", "resource": "main::done", "value": 2},
                    "cause": {"kind": "var_eq", "resource": "main::ready", "value": 1}
                }
            ]
        }"#,
    );
    for engine in [EngineKind::Petri, EngineKind::Interpreter] {
        let rep = verify_program(&prog, &vac_prec, engine);
        assert_eq!(rep.outcome, Outcome::Fail, "{engine:?}: {rep:?}");
        assert!(rep.properties[0].vacuous);
    }

    // Precedence failure: require ready == 1 before done == 0 (false at initial state).
    // AtMost failure: max: 1 when both ready == 0 and done == 0 hold at start.
    // Exclude failure: exclude both ready == 0 and done == 0.
    let fail_pats = parse_contract(
        r#"{
            "name": "pattern-failures",
            "properties": [
                {
                    "id": "fail-exclude",
                    "kind": "pattern",
                    "pattern": "exclude",
                    "preds": [
                        {"kind": "var_eq", "resource": "main::ready", "value": 0},
                        {"kind": "var_eq", "resource": "main::done", "value": 0}
                    ]
                },
                {
                    "id": "fail-at-most",
                    "kind": "pattern",
                    "pattern": "at_most",
                    "max": 1,
                    "conds": [
                        {"kind": "var_eq", "resource": "main::ready", "value": 0},
                        {"kind": "var_eq", "resource": "main::done", "value": 0}
                    ]
                },
                {
                    "id": "fail-precedence",
                    "kind": "pattern",
                    "pattern": "precedence",
                    "effect": {"kind": "var_eq", "resource": "main::done", "value": 0},
                    "cause": {"kind": "var_eq", "resource": "main::ready", "value": 1}
                }
            ]
        }"#,
    );
    for engine in [EngineKind::Petri, EngineKind::Interpreter] {
        let rep = verify_program(&prog, &fail_pats, engine);
        assert_eq!(rep.outcome, Outcome::Fail, "{engine:?}");
        assert!(rep.properties.iter().all(|p| p.outcome == Outcome::Fail));
    }
}

#[test]
fn health_obligation_property_detects_races_leaks_double_locks_and_respects_unknown() {
    let clean = parse_program(
        r#"{
            "program": "clean_condvar",
            "version": "4.0.0",
            "entry": "main::main",
            "modules": [{
                "name": "main",
                "resources": [
                    {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"},
                    {"name": "cv", "kind": "sync", "type": "Condvar", "mode": "Sync"},
                    {"name": "ready", "kind": "var", "type": "Var", "base": {"Int": [0, 1]}, "init": 0}
                ],
                "protection": [{"var": "ready", "lock": "m"}],
                "functions": [
                    {
                        "name": "main",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "scope", "funcs": ["producer", "consumer"]},
                            {"sid": "s2", "kind": "return"}
                        ]
                    },
                    {
                        "name": "producer",
                        "kind": "normal",
                        "structured_body": [
                            {
                                "id": "p1",
                                "kind": "lock_guard",
                                "resource": "m",
                                "body": [
                                    {"id": "p2", "kind": "write_shared", "resource": "ready", "expr": "1"},
                                    {"id": "p3", "kind": "condvar_notify_all", "condvar": "cv"}
                                ]
                            },
                            {"id": "p4", "kind": "return"}
                        ]
                    },
                    {
                        "name": "consumer",
                        "kind": "normal",
                        "structured_body": [
                            {
                                "id": "c1",
                                "kind": "lock_guard",
                                "resource": "m",
                                "body": [
                                    {"id": "c2", "kind": "while_wait", "condvar": "cv", "lock": "m", "cond": "ready == 0"}
                                ]
                            },
                            {"id": "c3", "kind": "return"}
                        ]
                    }
                ]
            }]
        }"#,
    );
    let health_all = parse_contract(
        r#"{
            "name": "health-all",
            "properties": [
                {"id": "health", "kind": "health"}
            ]
        }"#,
    );
    for engine in [EngineKind::Petri, EngineKind::Interpreter] {
        let rep = verify_program(&clean, &health_all, engine);
        assert_eq!(rep.outcome, Outcome::Pass, "{engine:?}: {:?}", rep);
    }

    // Truncated bounds -> MUST return Unknown, NEVER downgrade to Pass!
    let trunc_contract = parse_contract(
        r#"{
            "name": "health-trunc",
            "bounds": {"max_states": 2, "max_depth": 2},
            "properties": [
                {"id": "health", "kind": "health"}
            ]
        }"#,
    );
    for engine in [EngineKind::Petri, EngineKind::Interpreter] {
        let rep = verify_program(&clean, &trunc_contract, engine);
        assert_eq!(rep.outcome, Outcome::Unknown, "{engine:?}: {:?}", rep);
    }

    // DataRace: two threads mutate shared `x` without holding any lock.
    let race_prog = parse_program(
        r#"{
            "program": "data_race",
            "version": "4.0.0",
            "entry": "main::main",
            "modules": [{
                "name": "main",
                "resources": [
                    {"name": "x", "kind": "var", "type": "Var", "base": {"Int": [0, 4]}, "init": 0}
                ],
                "functions": [
                    {
                        "name": "main",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "scope", "funcs": ["w", "w"]},
                            {"sid": "s2", "kind": "return"}
                        ]
                    },
                    {
                        "name": "w",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "write_shared", "resource": "x", "expr": "x + 1"},
                            {"sid": "s2", "kind": "return"}
                        ]
                    }
                ]
            }]
        }"#,
    );
    let race_contract = parse_contract(
        r#"{
            "name": "health-race",
            "properties": [
                {"id": "race", "kind": "health", "checks": ["data_race"]}
            ]
        }"#,
    );
    for engine in [EngineKind::Petri, EngineKind::Interpreter] {
        let rep = verify_program(&race_prog, &race_contract, engine);
        assert_eq!(rep.outcome, Outcome::Fail, "{engine:?}: {:?}", rep);
    }

    // ResourceLeak: main acquires lock m and returns without unlocking (bypassing static E501 to test Health formula).
    let leak_prog = parse_program(
        r#"{
            "program": "lock_leak",
            "version": "4.0.0",
            "entry": "main::main",
            "modules": [{
                "name": "main",
                "resources": [
                    {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"}
                ],
                "functions": [
                    {
                        "name": "main",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "mutex_lock", "resource": "m"},
                            {"sid": "s2", "kind": "return"}
                        ]
                    }
                ]
            }]
        }"#,
    );
    let leak_sem = program::lower(&leak_prog).unwrap();
    let leak_contract = parse_contract(
        r#"{
            "name": "health-leak",
            "properties": [
                {"id": "leak", "kind": "health", "checks": ["resource_leak"]}
            ]
        }"#,
    )
    .resolve(&leak_sem)
    .unwrap();
    assert_eq!(
        verify(&PetriEngine::new(&leak_sem, leak_contract.bounds.clone()), &leak_contract).outcome,
        Outcome::Fail
    );
    assert_eq!(
        verify(&Interpreter::new(&leak_sem, leak_contract.bounds.clone()), &leak_contract).outcome,
        Outcome::Fail
    );

    // DoubleLock: cross-function non-reentrant re-acquisition (passes static validate, caught by Health).
    let dbl_prog = parse_program(
        r#"{
            "program": "double_lock",
            "version": "4.0.0",
            "entry": "main::main",
            "modules": [{
                "name": "main",
                "resources": [
                    {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"}
                ],
                "functions": [
                    {
                        "name": "main",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "mutex_lock", "resource": "m"},
                            {"sid": "s2", "kind": "call", "func": "helper", "args": []},
                            {"sid": "s3", "kind": "mutex_unlock", "resource": "m"},
                            {"sid": "s4", "kind": "return"}
                        ]
                    },
                    {
                        "name": "helper",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "mutex_lock", "resource": "m"},
                            {"sid": "s2", "kind": "mutex_unlock", "resource": "m"},
                            {"sid": "s3", "kind": "return"}
                        ]
                    }
                ]
            }]
        }"#,
    );
    let dbl_contract = parse_contract(
        r#"{
            "name": "health-dbl",
            "properties": [
                {"id": "dbl", "kind": "health", "checks": ["double_lock"]}
            ]
        }"#,
    );
    for engine in [EngineKind::Petri, EngineKind::Interpreter] {
        let rep = verify_program(&dbl_prog, &dbl_contract, engine);
        assert_eq!(rep.outcome, Outcome::Fail, "{engine:?}: {:?}", rep);
    }

    // LostWakeup: notifier runs once without predicate guard while waiter waits unconditionally.
    let lost_prog = parse_program(
        r#"{
            "program": "lost_wakeup",
            "version": "4.0.0",
            "entry": "main::main",
            "modules": [{
                "name": "main",
                "resources": [
                    {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"},
                    {"name": "cv", "kind": "sync", "type": "Condvar", "mode": "Sync"}
                ],
                "functions": [
                    {
                        "name": "main",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "scope", "funcs": ["notifier", "waiter"]},
                            {"sid": "s2", "kind": "return"}
                        ]
                    },
                    {
                        "name": "notifier",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "mutex_lock", "resource": "m"},
                            {"sid": "s2", "kind": "condvar_notify", "condvar": "cv"},
                            {"sid": "s3", "kind": "mutex_unlock", "resource": "m"},
                            {"sid": "s4", "kind": "return"}
                        ]
                    },
                    {
                        "name": "waiter",
                        "kind": "normal",
                        "body": [
                            {"sid": "s1", "kind": "mutex_lock", "resource": "m"},
                            {"sid": "s2", "kind": "condvar_wait", "condvar": "cv", "lock": "m"},
                            {"sid": "s3", "kind": "mutex_unlock", "resource": "m"},
                            {"sid": "s4", "kind": "return"}
                        ]
                    }
                ]
            }]
        }"#,
    );
    let lost_contract = parse_contract(
        r#"{
            "name": "health-lost",
            "properties": [
                {"id": "lost", "kind": "health", "checks": ["lost_wakeup"]}
            ]
        }"#,
    );
    for engine in [EngineKind::Petri, EngineKind::Interpreter] {
        let rep = verify_program(&lost_prog, &lost_contract, engine);
        assert_eq!(rep.outcome, Outcome::Fail, "{engine:?}: {:?}", rep);
    }
}
