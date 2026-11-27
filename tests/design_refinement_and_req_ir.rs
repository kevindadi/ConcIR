use concir::ast::Program;
use concir::design_refinement::verify_design_refinement;
use concir::explore::contract::ContractSpec;
use concir::explore::{verify_program, EngineKind};
use concir::req_ir::{check_req_program, ReqProgram};
use concir::sem::outcome::{AnalysisBounds, Outcome};

fn producer_consumer_req_ir() -> ReqProgram {
    let json = r#"{
        "schema": "concir-req-ir/1",
        "name": "pc_req",
        "roles": [
            {
                "name": "producer",
                "instances": 1,
                "init_state": "ready",
                "states": ["ready", "done"],
                "terminal_states": ["done"],
                "req": ["R1"]
            },
            {
                "name": "consumer",
                "instances": 1,
                "init_state": "waiting",
                "states": ["waiting", "done"],
                "terminal_states": ["done"],
                "req": ["R2"]
            }
        ],
        "state_vars": [
            {
                "name": "items",
                "type": {"Int": [0, 1]},
                "init": 0,
                "req": ["R1", "R2"]
            }
        ],
        "milestones": [
            {"name": "item_produced", "description": "Producer adds an item", "req": ["R1"]},
            {"name": "item_consumed", "description": "Consumer drains the item", "req": ["R2"]}
        ],
        "steps": [
            {
                "id": "p_produce",
                "role": "producer",
                "from_state": "ready",
                "to_state": "done",
                "guard": "items == 0",
                "updates": [{"target": "items", "expr": "items + 1"}],
                "milestone": "item_produced",
                "req": ["R1"]
            },
            {
                "id": "c_consume",
                "role": "consumer",
                "from_state": "waiting",
                "to_state": "done",
                "guard": "items == 1",
                "updates": [{"target": "items", "expr": "items - 1"}],
                "milestone": "item_consumed",
                "req": ["R2"]
            }
        ]
    }"#;
    serde_json::from_str(json).expect("valid req_ir")
}

fn valid_scir_design(include_consume_milestone: bool, wrong_order: bool) -> Program {
    let consume_milestone = if wrong_order {
        ""
    } else if include_consume_milestone {
        r#","milestone": "item_consumed""#
    } else {
        ""
    };
    let wrong_consume_prefix = if wrong_order {
        r#"{ "id": "c_bad", "kind": "nop", "milestone": "item_consumed" },"#
    } else {
        ""
    };
    let json = format!(
        r#"{{
        "program": "pc_design",
        "version": "4.0.0",
        "entry": "main::main",
        "modules": [{{
            "name": "main",
            "resources": [
                {{"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}},
                {{"name": "cv", "kind": "sync", "type": "Condvar", "mode": "Sync"}},
                {{"name": "items", "kind": "var", "type": "Var", "base": {{"Int": [0, 1]}}, "init": 0}}
            ],
            "protection": [{{"var": "items", "lock": "mtx"}}],
            "functions": [
                {{
                    "name": "producer",
                    "kind": "normal",
                    "form": "closure",
                    "structured_body": [
                        {{
                            "id": "p_lg",
                            "kind": "lock_guard",
                            "resource": "mtx",
                            "body": [
                                {{
                                    "id": "p_hole",
                                    "kind": "stmt",
                                    "milestone": "item_produced",
                                    "op": {{
                                        "kind": "seq_hole",
                                        "id": "h_produce",
                                        "desc": "produce item",
                                        "reads": ["items"],
                                        "writes": ["items"],
                                        "updates_modeled": [{{"target": "items", "expr": "items + 1"}}],
                                        "expected_type": "()"
                                    }}
                                }},
                                {{"id": "p_sig", "kind": "condvar_notify_all", "condvar": "cv"}}
                            ]
                        }}
                    ]
                }},
                {{
                    "name": "consumer",
                    "kind": "normal",
                    "form": "closure",
                    "structured_body": [
                        {wrong_consume_prefix}
                        {{
                            "id": "c_lg",
                            "kind": "lock_guard",
                            "resource": "mtx",
                            "body": [
                                {{
                                    "id": "c_wait",
                                    "kind": "while_wait",
                                    "cond": "items == 0",
                                    "condvar": "cv",
                                    "lock": "mtx",
                                    "body": []
                                }},
                                {{
                                    "id": "c_hole",
                                    "kind": "stmt"
                                    {consume_milestone},
                                    "op": {{
                                        "kind": "seq_hole",
                                        "id": "h_consume",
                                        "desc": "consume item",
                                        "reads": ["items"],
                                        "writes": ["items"],
                                        "updates_modeled": [{{"target": "items", "expr": "items - 1"}}],
                                        "expected_type": "()"
                                    }}
                                }}
                            ]
                        }}
                    ]
                }},
                {{
                    "name": "main",
                    "kind": "normal",
                    "structured_body": [
                        {{"id": "m_sc", "kind": "scope", "funcs": ["producer", "consumer"]}}
                    ]
                }}
            ]
        }}]
    }}"#
    );
    serde_json::from_str(&json).expect("valid scir")
}

#[test]
fn req_ir_self_check_passes_and_catches_dead_milestone() {
    let req = producer_consumer_req_ir();
    let rep = check_req_program(&req, None, 10_000);
    assert_eq!(rep.outcome, "pass", "unexpected report: {rep:?}");
    assert!(rep.deadlock_free);
    assert!(rep.no_dead_milestones);
    assert_eq!(
        rep.reachable_milestones,
        vec!["item_consumed".to_string(), "item_produced".to_string()]
    );

    // Dead milestone declared in req.milestones but never referenced in steps.
    let mut bad_req = producer_consumer_req_ir();
    bad_req.milestones.push(concir::req_ir::MilestoneSpec {
        name: "never_happens".into(),
        description: "dead".into(),
        req: vec!["R3".into()],
    });
    let bad_rep = check_req_program(&bad_req, None, 10_000);
    assert_eq!(bad_rep.outcome, "fail");
    assert!(!bad_rep.no_dead_milestones);
    assert_eq!(bad_rep.missing_milestones, vec!["never_happens".to_string()]);
}

#[test]
fn design_refinement_accepts_valid_mutex_condvar_scir_and_rejects_violations() {
    let req = producer_consumer_req_ir();

    // 1. Valid Producer-Consumer SCIR with Mutex + Condvar refines abstract ReqIR.
    let ok_prog = valid_scir_design(true, false);
    let ok_rep = verify_design_refinement(&req, &ok_prog, None, AnalysisBounds::default());
    assert_eq!(ok_rep.outcome, "pass", "expected pass: {ok_rep:?}");
    assert!(ok_rep.checks.weak_simulation);
    assert!(ok_rep.checks.deadlock_free);
    assert!(ok_rep.checks.milestone_preservation);
    assert!(ok_rep.checks.health_obligations);

    // 2. Wrong milestone ordering (consume happens before produce) fails weak simulation.
    let wrong_order_prog = valid_scir_design(true, true);
    let wrong_rep = verify_design_refinement(&req, &wrong_order_prog, None, AnalysisBounds::default());
    assert_eq!(wrong_rep.outcome, "fail", "expected fail: {wrong_rep:?}");
    assert!(!wrong_rep.checks.weak_simulation);
    assert!(
        wrong_rep
            .violations
            .iter()
            .any(|v| v.kind == "unexpected_milestone_order"),
        "expected unexpected_milestone_order in {:?}",
        wrong_rep.violations
    );

    // 3. Omitting a declared milestone fails milestone preservation.
    let missing_ms_prog = valid_scir_design(false, false);
    let missing_rep =
        verify_design_refinement(&req, &missing_ms_prog, None, AnalysisBounds::default());
    assert_eq!(missing_rep.outcome, "fail", "expected fail: {missing_rep:?}");
    assert!(!missing_rep.checks.milestone_preservation);
    assert_eq!(missing_rep.missing_milestones, vec!["item_consumed".to_string()]);
}

#[test]
fn design_refinement_never_downgrades_truncated_to_pass() {
    let req = producer_consumer_req_ir();
    let ok_prog = valid_scir_design(true, false);
    let tiny_bounds = AnalysisBounds {
        max_states: 2,
        ..AnalysisBounds::default()
    };
    let rep = verify_design_refinement(&req, &ok_prog, None, tiny_bounds);
    assert_ne!(rep.outcome, "pass");
    assert_eq!(rep.outcome, "unknown", "truncated run must remain unknown: {rep:?}");
    assert!(!rep.complete);
}

#[test]
fn milestone_reached_predicate_works_on_petri_and_interp() {
    let prog = valid_scir_design(true, false);
    let contract_json = r#"{
        "name": "milestone-pred-test",
        "properties": [
            {
                "kind": "pattern",
                "id": "p_reach_produced",
                "pattern": "can_reach",
                "goal": {"kind": "milestone_reached", "milestone": "item_produced"}
            },
            {
                "kind": "pattern",
                "id": "p_precedence",
                "pattern": "precedence",
                "effect": {"kind": "milestone_reached", "milestone": "item_consumed"},
                "cause": {"kind": "milestone_reached", "milestone": "item_produced"},
                "non_vacuous": true
            }
        ]
    }"#;
    let spec: ContractSpec = serde_json::from_str(contract_json).expect("contract");
    for engine in [EngineKind::Petri, EngineKind::Interpreter] {
        let rep = verify_program(&prog, &spec, engine);
        assert_eq!(rep.outcome, Outcome::Pass, "engine {engine:?}: {rep:?}");
    }
}
