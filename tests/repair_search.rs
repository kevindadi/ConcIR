//! Diagnostic-driven composite repair: the two-independent-lock-cycle case and
//! the strategy comparison (A single / B composite / C diagnostic).

use concir::ast::Program;
use concir::explore::contract::ContractSpec;
use concir::explore::{verify_program, EngineKind};
use concir::repair::search::{run_search, RepairStrategy, SearchConfig};
use concir::repair::RepairOutcome;
use concir::sem::outcome::Outcome;

const TWO_CYCLES: &str = r#"{
  "program": "two_cycles",
  "version": "3.5.0",
  "modules": [{
    "name": "main",
    "provides": {"resources": ["a", "b", "c", "d"], "functions": ["main", "t1", "t2", "t3", "t4"]},
    "resources": [
      {"name": "a", "kind": "sync", "type": "Mutex", "mode": "Sync"},
      {"name": "b", "kind": "sync", "type": "Mutex", "mode": "Sync"},
      {"name": "c", "kind": "sync", "type": "Mutex", "mode": "Sync"},
      {"name": "d", "kind": "sync", "type": "Mutex", "mode": "Sync"}
    ],
    "protection": [],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "scope", "funcs": ["t1", "t2", "t3", "t4"]},
        {"sid": "s2", "kind": "return"}
      ]},
      {"name": "t1", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "mutex_lock", "resource": "a"},
        {"sid": "s2", "kind": "mutex_lock", "resource": "b"},
        {"sid": "s3", "kind": "mutex_unlock", "resource": "b"},
        {"sid": "s4", "kind": "mutex_unlock", "resource": "a"},
        {"sid": "s5", "kind": "return"}
      ]},
      {"name": "t2", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "mutex_lock", "resource": "b"},
        {"sid": "s2", "kind": "mutex_lock", "resource": "a"},
        {"sid": "s3", "kind": "mutex_unlock", "resource": "a"},
        {"sid": "s4", "kind": "mutex_unlock", "resource": "b"},
        {"sid": "s5", "kind": "return"}
      ]},
      {"name": "t3", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "mutex_lock", "resource": "c"},
        {"sid": "s2", "kind": "mutex_lock", "resource": "d"},
        {"sid": "s3", "kind": "mutex_unlock", "resource": "d"},
        {"sid": "s4", "kind": "mutex_unlock", "resource": "c"},
        {"sid": "s5", "kind": "return"}
      ]},
      {"name": "t4", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "mutex_lock", "resource": "d"},
        {"sid": "s2", "kind": "mutex_lock", "resource": "c"},
        {"sid": "s3", "kind": "mutex_unlock", "resource": "c"},
        {"sid": "s4", "kind": "mutex_unlock", "resource": "d"},
        {"sid": "s5", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

const CONTRACT: &str = r#"{
  "name": "two-cycles",
  "properties": [{"kind": "deadlock_free", "id": "no-deadlock"}],
  "preserved": [
    {"kind": "reachable", "description": "t1 completes", "goal": {"kind": "function_completed", "function": "main::t1"}},
    {"kind": "reachable", "description": "t2 completes", "goal": {"kind": "function_completed", "function": "main::t2"}},
    {"kind": "reachable", "description": "t3 completes", "goal": {"kind": "function_completed", "function": "main::t3"}},
    {"kind": "reachable", "description": "t4 completes", "goal": {"kind": "function_completed", "function": "main::t4"}}
  ],
  "allowed_scope": {"allow_lock_reorder": true}
}"#;

fn program() -> Program {
    serde_json::from_str(TWO_CYCLES).unwrap()
}

fn spec() -> ContractSpec {
    serde_json::from_str(CONTRACT).unwrap()
}

fn config(strategy: RepairStrategy) -> SearchConfig {
    SearchConfig {
        strategy,
        ..SearchConfig::default()
    }
}

#[test]
fn two_cycles_root_fails() {
    let p = program();
    let s = spec();
    let r = verify_program(&p, &s, EngineKind::Petri);
    assert_eq!(r.outcome, Outcome::Fail);
}

#[test]
fn single_strategy_cannot_fix_two_cycles() {
    let report = run_search(&program(), &spec(), &config(RepairStrategy::Single));
    assert_ne!(
        report.outcome,
        RepairOutcome::Repaired,
        "{:?}",
        report.nodes
    );
    assert_eq!(report.outcome, RepairOutcome::NoAcceptableCandidate);
}

#[test]
fn composite_and_diagnostic_fix_two_cycles() {
    for strategy in [RepairStrategy::Composite, RepairStrategy::Diagnostic] {
        let report = run_search(&program(), &spec(), &config(strategy));
        assert_eq!(
            report.outcome,
            RepairOutcome::Repaired,
            "{strategy:?}: {:?}",
            report.nodes
        );
        assert_eq!(
            report.patch_chain.len(),
            2,
            "{strategy:?} should need two edits"
        );
        // The two edits touch different functions.
        let fns: Vec<&str> = report
            .patch_chain
            .iter()
            .map(|e| e.function.as_str())
            .collect();
        assert_ne!(fns[0], fns[1], "two independent cycles need two functions");
        // Every intermediate node is a real verified program.
        assert!(report
            .nodes
            .iter()
            .any(|n| n.report.outcome == Outcome::Fail));
        // The accepted program re-verifies from its exported JSON.
        let patched = report.accepted_program.as_ref().unwrap();
        let json = serde_json::to_string(patched).unwrap();
        let reparsed: Program = serde_json::from_str(&json).unwrap();
        let re = verify_program(&reparsed, &spec(), EngineKind::Petri);
        assert_eq!(re.outcome, Outcome::Pass);
        assert!(re.complete);
    }
}

#[test]
fn search_is_deterministic() {
    let a = run_search(&program(), &spec(), &config(RepairStrategy::Diagnostic));
    let b = run_search(&program(), &spec(), &config(RepairStrategy::Diagnostic));
    assert_eq!(a.outcome, b.outcome);
    assert_eq!(a.proposals, b.proposals);
    assert_eq!(a.verifications, b.verifications);
    assert_eq!(
        a.patch_chain
            .iter()
            .map(|e| format!("{}:{}", e.function, e.changes.len()))
            .collect::<Vec<_>>(),
        b.patch_chain
            .iter()
            .map(|e| format!("{}:{}", e.function, e.changes.len()))
            .collect::<Vec<_>>()
    );
    assert_eq!(a.nodes.len(), b.nodes.len());
}

#[test]
fn second_step_uses_the_first_step_program_and_diagnostics() {
    // Diagnostic strategy must expand a FAIL child (one cycle fixed) using that
    // child's own program and its new diagnostic.
    let report = run_search(&program(), &spec(), &config(RepairStrategy::Diagnostic));
    assert_eq!(report.outcome, RepairOutcome::Repaired);
    let chain = &report.patch_chain;
    assert_eq!(chain.len(), 2);
    // The first edit's program fingerprint is the second edit's parent.
    assert_eq!(
        chain[0].program_fingerprint, chain[1].parent_fingerprint,
        "the second edit must build on the first edit's program"
    );
}

#[test]
fn budget_exhaustion_is_reported() {
    let cfg = SearchConfig {
        strategy: RepairStrategy::Composite,
        candidate_budget: 2,
        ..SearchConfig::default()
    };
    let report = run_search(&program(), &spec(), &cfg);
    assert_eq!(report.outcome, RepairOutcome::BudgetExhausted);
    assert!(
        report.stop_reason.starts_with("candidate-budget")
            || report.stop_reason.starts_with("verification-budget"),
        "specific stop reason, got {}",
        report.stop_reason
    );
}

#[test]
fn already_satisfied_program_needs_no_patch() {
    // A single-cycle program is fixed by JSON; use a program with consistent
    // lock order and the same contract shape.
    let ok = TWO_CYCLES.replace(
        r#"{"sid": "s1", "kind": "mutex_lock", "resource": "b"},
        {"sid": "s2", "kind": "mutex_lock", "resource": "a"},
        {"sid": "s3", "kind": "mutex_unlock", "resource": "a"},
        {"sid": "s4", "kind": "mutex_unlock", "resource": "b"}"#,
        r#"{"sid": "s1", "kind": "mutex_lock", "resource": "a"},
        {"sid": "s2", "kind": "mutex_lock", "resource": "b"},
        {"sid": "s3", "kind": "mutex_unlock", "resource": "b"},
        {"sid": "s4", "kind": "mutex_unlock", "resource": "a"}"#,
    );
    let ok = ok.replace(
        r#"{"sid": "s1", "kind": "mutex_lock", "resource": "d"},
        {"sid": "s2", "kind": "mutex_lock", "resource": "c"},
        {"sid": "s3", "kind": "mutex_unlock", "resource": "c"},
        {"sid": "s4", "kind": "mutex_unlock", "resource": "d"}"#,
        r#"{"sid": "s1", "kind": "mutex_lock", "resource": "c"},
        {"sid": "s2", "kind": "mutex_lock", "resource": "d"},
        {"sid": "s3", "kind": "mutex_unlock", "resource": "d"},
        {"sid": "s4", "kind": "mutex_unlock", "resource": "c"}"#,
    );
    let p: Program = serde_json::from_str(&ok).unwrap();
    let report = run_search(&p, &spec(), &config(RepairStrategy::Diagnostic));
    assert_eq!(report.outcome, RepairOutcome::AlreadySatisfied);
    assert!(report.patch_chain.is_empty());
}

#[test]
fn counterexample_replays_with_concrete_bindings() {
    use concir::explore;
    use concir::interp::Interpreter;
    use concir::sem::program;
    use concir::sem::system::TransitionSystem;

    // Only deadlock_free, so the counterexample is a concrete deadlock path.
    let contract: ContractSpec = serde_json::from_str(
        r#"{"name":"c","properties":[{"kind":"deadlock_free","id":"no-deadlock"}],"bounds":{"max_states":20000,"max_depth":64}}"#,
    )
    .unwrap();
    let sp = program::lower(&program()).unwrap();
    let contract = contract.resolve(&sp).unwrap();
    let engine = Interpreter::new(&sp, contract.bounds.clone());
    let report = explore::verify(&engine, &contract);
    let diag = report
        .diagnostics
        .iter()
        .find(|d| d.property == "no-deadlock")
        .expect("deadlock diagnostic");
    assert!(!diag.counterexample.is_empty());

    // Replay the recorded labels from the initial state, matching the concrete
    // thread/frame bindings exactly.
    let mut state = engine.initial().unwrap();
    for (i, label) in diag.counterexample.iter().enumerate() {
        let en = engine.successors(&state).unwrap();
        let step = en
            .steps
            .into_iter()
            .find(|s| {
                s.label.origin == label.origin
                    && s.label.thread == label.thread
                    && s.label.frame == label.frame
            })
            .unwrap_or_else(|| panic!("step {i} is not enabled with the recorded binding"));
        state = step.state;
    }
    assert!(
        !engine.is_finished(&state),
        "replayed state must not be finished"
    );
    assert!(
        engine.successors(&state).unwrap().steps.is_empty(),
        "replayed state must be the deadlock"
    );
}

fn spec_with_max_states(n: usize) -> ContractSpec {
    let s = format!(
        r#"{{"name":"tiny","properties":[{{"kind":"deadlock_free","id":"no-deadlock"}}],"preserved":[{{"kind":"reachable","description":"t1","goal":{{"kind":"function_completed","function":"main::t1"}}}},{{"kind":"reachable","description":"t2","goal":{{"kind":"function_completed","function":"main::t2"}}}},{{"kind":"reachable","description":"t3","goal":{{"kind":"function_completed","function":"main::t3"}}}},{{"kind":"reachable","description":"t4","goal":{{"kind":"function_completed","function":"main::t4"}}}}],"bounds":{{"max_states":{n}}},"allowed_scope":{{"allow_lock_reorder":true}}}}"#
    );
    serde_json::from_str(&s).unwrap()
}

#[test]
fn e5_verification_budget_zero_is_rejected_before_work() {
    let cfg = SearchConfig {
        verification_budget: 0,
        ..SearchConfig::default()
    };
    let report = run_search(&program(), &spec(), &cfg);
    assert_eq!(report.outcome, RepairOutcome::InvalidConfig);
    assert_eq!(report.stop_reason, "verification-budget-zero");
    assert_eq!(report.verifications, 0, "no verification may run");
    assert!(report.nodes.is_empty());
}

#[test]
fn e5_depth_and_edits_truncation_are_reported() {
    let depth = SearchConfig {
        strategy: RepairStrategy::Composite,
        max_depth: 1,
        ..SearchConfig::default()
    };
    let r = run_search(&program(), &spec(), &depth);
    assert_eq!(r.outcome, RepairOutcome::BudgetExhausted);
    assert_eq!(r.stop_reason, "max-depth");
    assert!(r.truncation.is_some());

    let edits = SearchConfig {
        strategy: RepairStrategy::Composite,
        max_total_edits: 1,
        ..SearchConfig::default()
    };
    let r = run_search(&program(), &spec(), &edits);
    assert_eq!(r.outcome, RepairOutcome::BudgetExhausted);
    assert_eq!(r.stop_reason, "max-total-edits");
}

#[test]
fn e2_bounds_come_from_the_frozen_contract() {
    // A tiny contract bound makes the root Unknown; the search must not run at
    // the default scale.
    let report = run_search(
        &program(),
        &spec_with_max_states(1),
        &config(RepairStrategy::Composite),
    );
    assert_eq!(report.outcome, RepairOutcome::AnalysisUnknown);
    assert!(
        report.states_explored <= 2,
        "got {}",
        report.states_explored
    );
    assert!(report.verifications <= 1);
}

#[test]
fn e3_dedup_avoids_repeated_verification() {
    let report = run_search(&program(), &spec(), &config(RepairStrategy::Composite));
    assert_eq!(report.outcome, RepairOutcome::Repaired);
    assert!(report.cache_hits >= 1, "expected a duplicate candidate");
    assert_eq!(
        report.verifications, report.unique_programs,
        "every unique program must be verified exactly once"
    );
}

#[test]
fn e4_attempts_and_nodes_have_consistent_identity() {
    let report = run_search(&program(), &spec(), &config(RepairStrategy::Composite));
    for a in &report.attempts {
        assert!(a.parent < report.nodes.len(), "attempt parent out of range");
        match a.result.as_str() {
            "verified" => {
                let node = report
                    .nodes
                    .iter()
                    .find(|n| {
                        a.program_fingerprint.as_deref() == Some(n.program_fingerprint.as_str())
                    })
                    .expect("verified attempt must reference a node");
                assert_eq!(
                    node.parent,
                    Some(a.parent),
                    "a new node's parent must equal the attempt's actual parent"
                );
            }
            "reused" => {
                let node = a
                    .reused_node
                    .expect("reused attempt references an existing node");
                assert_eq!(
                    report.nodes[node].program_fingerprint,
                    *a.program_fingerprint.as_ref().unwrap()
                );
                assert!(report.nodes[node].parent.is_some() || node == 0);
                // The reused node keeps its own parent; the attempt's parent
                // records where this proposal actually came from.
            }
            _ => assert!(a.program_fingerprint.is_none()),
        }
    }
}

#[test]
fn e4_modules_denied_attempts_reference_the_root() {
    // allowed_scope.modules excluding the current module yields denied
    // attempts, all parented at node 0 (never a synthetic id=0 node).
    let mut s = spec();
    s.allowed_scope.modules = vec!["other".to_string()];
    let report = run_search(&program(), &s, &config(RepairStrategy::Composite));
    assert_eq!(report.outcome, RepairOutcome::NoAcceptableCandidate);
    assert!(report.nodes.len() == 1);
    assert!(!report.attempts.is_empty());
    for a in &report.attempts {
        assert_eq!(a.parent, 0);
        assert_eq!(a.result, "denied");
    }
}

#[test]
fn e8_artifact_replays_and_rejects_tampering() {
    use concir::repair::search::replay_artifact;
    let cfg = config(RepairStrategy::Diagnostic);
    let report = run_search(&program(), &spec(), &cfg);
    let artifact = report.artifact_with_config(&program(), &spec(), &cfg);
    let json = serde_json::to_string(&artifact).unwrap();
    replay_artifact(&json).expect("clean artifact must replay");

    // Tamper with a node's incoming patch base hash.
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let nodes = value["nodes"].as_array_mut().unwrap();
    let victim = nodes
        .iter_mut()
        .find(|n| n["incoming"].is_object())
        .expect("a node with an incoming patch");
    victim["incoming"]["original_function_hash"] = serde_json::Value::String("deadbeef".into());
    let tampered = serde_json::to_string(&value).unwrap();
    assert!(
        replay_artifact(&tampered).is_err(),
        "tampered patch base must be rejected"
    );

    // Tamper with a parent reference.
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let nodes = value["nodes"].as_array_mut().unwrap();
    if let Some(node) = nodes.iter_mut().find(|n| n["parent"].is_number()) {
        node["parent"] = serde_json::json!(999);
    }
    let tampered = serde_json::to_string(&value).unwrap();
    assert!(
        replay_artifact(&tampered).is_err(),
        "bad parent must be rejected"
    );
}

#[test]
fn e1_complex_types_survive_search_export() {
    // single_cycle plus non-lock shared values of every complex type.
    let mut p: Program =
        serde_json::from_str(include_str!("repro_bench/single_cycle.json")).unwrap();
    let single_spec: ContractSpec =
        serde_json::from_str(include_str!("repro_bench/single_cycle_contract.json")).unwrap();
    for json in [
        serde_json::json!({"name":"bi","kind":"var","type":"Var","base":{"Int":[0,5]},"init":2}),
        serde_json::json!({"name":"en","kind":"var","type":"Var","base":{"Enum":["X","Y"]},"init":"X"}),
        serde_json::json!({"name":"st","kind":"var","type":"Var","base":{"Struct":{"n":"Int"}},"init":{"n":1}}),
        serde_json::json!({"name":"ar","kind":"var","type":"Var","base":{"Array":{"elem":{"Int":[0,1]},"len":2}},"init":[0,1]}),
    ] {
        p.modules[0]
            .resources
            .push(serde_json::from_value(json).unwrap());
    }
    let report = run_search(&p, &single_spec, &config(RepairStrategy::Diagnostic));
    assert_eq!(
        report.outcome,
        RepairOutcome::Repaired,
        "{:?}",
        report.nodes
    );
    let accepted = report.accepted_program.unwrap();
    let json = serde_json::to_string(&accepted).unwrap();
    let reparsed: Program = serde_json::from_str(&json).expect("complex-type export must reload");
    let re = verify_program(&reparsed, &single_spec, EngineKind::Petri);
    assert_eq!(re.outcome, Outcome::Pass);
    assert!(re.complete);
}
