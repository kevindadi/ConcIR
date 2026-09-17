//! End-to-end, LLM-free repair demonstration and rejection tests.

use concir::ast::Program;
use concir::explore::contract::ContractSpec;
use concir::explore::{verify_program, EngineKind};
use concir::repair::candidates::{FileCandidateProvider, LockOrderEnumerator};
use concir::repair::{run_repair, RepairOutcome};
use concir::sem::outcome::Outcome;
use concir::validate;

const BUGGY: &str = r#"{
  "program": "lockorder",
  "version": "3.5.0",
  "modules": [{
    "name": "main",
    "provides": {"resources": ["a", "b", "x"], "functions": ["main", "t1", "t2"]},
    "resources": [
      {"name": "a", "kind": "sync", "type": "Mutex", "mode": "Sync"},
      {"name": "b", "kind": "sync", "type": "Mutex", "mode": "Sync"},
      {"name": "x", "kind": "var", "type": "Var", "base": "Int", "init": 0}
    ],
    "protection": [],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "scope", "funcs": ["t1", "t2"]},
        {"sid": "s2", "kind": "return"}
      ]},
      {"name": "t1", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "mutex_lock", "resource": "a"},
        {"sid": "s2", "kind": "mutex_lock", "resource": "b"},
        {"sid": "s3", "kind": "write_shared", "resource": "x", "expr": "x + 1"},
        {"sid": "s4", "kind": "mutex_unlock", "resource": "b"},
        {"sid": "s5", "kind": "mutex_unlock", "resource": "a"},
        {"sid": "s6", "kind": "return"}
      ]},
      {"name": "t2", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "mutex_lock", "resource": "b"},
        {"sid": "s2", "kind": "mutex_lock", "resource": "a"},
        {"sid": "s3", "kind": "write_shared", "resource": "x", "expr": "x + 1"},
        {"sid": "s4", "kind": "mutex_unlock", "resource": "a"},
        {"sid": "s5", "kind": "mutex_unlock", "resource": "b"},
        {"sid": "s6", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

const CONTRACT: &str = r#"{
  "name": "lockorder",
  "properties": [{"kind": "deadlock_free", "id": "no-deadlock"}],
  "preserved": [
    {"kind": "reachable", "description": "t1 completes",
     "goal": {"kind": "function_completed", "function": "main::t1"}},
    {"kind": "reachable", "description": "t2 completes",
     "goal": {"kind": "function_completed", "function": "main::t2"}}
  ],
  "allowed_scope": {"allow_lock_reorder": true}
}"#;

fn parse(src: &str) -> Program {
    serde_json::from_str(src).unwrap()
}

fn spec(src: &str) -> ContractSpec {
    serde_json::from_str(src).unwrap()
}

#[test]
fn buggy_program_is_statically_valid_but_deadlocks() {
    let p = parse(BUGGY);
    assert!(validate::validate(&p).valid, "buggy program must be statically valid");
    let report = verify_program(&p, &spec(CONTRACT), EngineKind::Petri);
    assert_eq!(report.outcome, Outcome::Fail);
    assert!(report
        .properties
        .iter()
        .any(|p| p.id == "no-deadlock" && p.outcome == Outcome::Fail));
}

#[test]
fn end_to_end_lock_order_repair_succeeds() {
    let p = parse(BUGGY);
    let s = spec(CONTRACT);
    let mut provider = LockOrderEnumerator::new(&p, &s.allowed_scope);
    let report = run_repair(&p, &s, &mut provider, 8);
    assert_eq!(
        report.outcome,
        RepairOutcome::Repaired,
        "rounds: {:#?}",
        report.rounds
    );
    let accepted = report.accepted.as_ref().expect("accepted patch");
    assert!(accepted.function == "t1" || accepted.function == "t2");
    assert!(!accepted.changes.is_empty());
    let patched = report.accepted_program.as_ref().unwrap();
    assert!(validate::validate(patched).valid);
    let after = verify_program(patched, &s, EngineKind::Petri);
    assert_eq!(after.outcome, Outcome::Pass, "patched program must verify");
}

#[test]
fn rejects_patch_that_removes_required_behavior() {
    let p = parse(BUGGY);
    let s = spec(r#"{
      "name": "lockorder",
      "properties": [{"kind": "deadlock_free", "id": "no-deadlock"}],
      "preserved": [
        {"kind": "reachable", "description": "x reaches 2",
         "goal": {"kind": "var_eq", "resource": "x", "value": 2}}
      ],
      "allowed_scope": {"allow_statement_delete": true}
    }"#);
    let patch_json = r#"[{
      "module": "main",
      "function": "t2",
      "id": "delete-required",
      "changes": [
        {"kind": "delete_statement", "sid": "s1"},
        {"kind": "delete_statement", "sid": "s3"},
        {"kind": "delete_statement", "sid": "s5"}
      ]
    }]"#;
    let mut provider = FileCandidateProvider::from_json(patch_json).unwrap();
    let report = run_repair(&p, &s, &mut provider, 4);
    assert_ne!(report.outcome, RepairOutcome::Repaired);
    assert!(report.rounds.iter().any(|r| !r.accepted));
}

#[test]
fn budget_exhaustion_when_no_candidate_satisfies() {
    let p = parse(BUGGY);
    let s = spec(r#"{
      "name": "lockorder",
      "properties": [{"kind": "deadlock_free", "id": "no-deadlock"}],
      "preserved": [
        {"kind": "reachable", "description": "impossible",
         "goal": {"kind": "false"}}
      ],
      "allowed_scope": {"allow_lock_reorder": true}
    }"#);
    let mut provider = LockOrderEnumerator::new(&p, &s.allowed_scope);
    let report = run_repair(&p, &s, &mut provider, 1);
    assert_eq!(report.outcome, RepairOutcome::BudgetExhausted);
    assert!(report.rounds.iter().all(|r| !r.accepted));
}
