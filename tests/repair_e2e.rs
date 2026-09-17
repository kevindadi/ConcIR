//! End-to-end, LLM-free repair demonstration and rejection tests.

use concir::ast::Program;
use concir::explore::contract::ContractSpec;
use concir::explore::verify;
use concir::petri::PetriEngine;
use concir::repair::candidates::{CandidateProvider, FileCandidateProvider, LockOrderEnumerator};
use concir::repair::{run_repair, RepairOutcome};
use concir::sem::outcome::Outcome;
use concir::sem::program;
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

fn resolved(program_src: &str) -> (Program, concir::explore::contract::VerificationContract) {
    let p = parse(program_src);
    let sp = program::lower(&p).unwrap();
    let spec: ContractSpec = serde_json::from_str(CONTRACT).unwrap();
    let contract = spec.resolve(&sp).unwrap();
    (p, contract)
}

#[test]
fn buggy_program_is_statically_valid_but_deadlocks() {
    let p = parse(BUGGY);
    assert!(validate::validate(&p).valid, "buggy program must be statically valid");
    let (_, contract) = resolved(BUGGY);
    let sp = program::lower(&p).unwrap();
    let engine = PetriEngine::new(&sp, contract.bounds.clone());
    let report = verify(&engine, &contract);
    assert_eq!(report.outcome, Outcome::Fail);
    assert!(report
        .properties
        .iter()
        .any(|p| p.id == "no-deadlock" && p.outcome == Outcome::Fail));
}

#[test]
fn end_to_end_lock_order_repair_succeeds() {
    let (p, contract) = resolved(BUGGY);
    let mut provider = LockOrderEnumerator::new(&p, &contract.allowed_scope);
    let report = run_repair(&p, &contract, &mut provider, 8);
    assert_eq!(
        report.outcome,
        RepairOutcome::Repaired,
        "rounds: {:#?}",
        report.rounds
    );
    let accepted = report.accepted.as_ref().expect("accepted patch");
    assert!(
        accepted.function == "t1" || accepted.function == "t2",
        "unexpected target function {}",
        accepted.function
    );
    assert!(!accepted.changes.is_empty());
    let patched = report.accepted_program.as_ref().unwrap();
    assert!(validate::validate(patched).valid);
    let sp = program::lower(patched).unwrap();
    let engine = PetriEngine::new(&sp, contract.bounds.clone());
    let after = verify(&engine, &contract);
    assert_eq!(after.outcome, Outcome::Pass, "patched program must verify");
}

#[test]
fn rejects_patch_that_removes_required_behavior() {
    // Delete t2's lock b, its write, and its unlock b. This removes the
    // deadlock (old counterexample gone) but makes x == 2 unreachable, which
    // the contract requires. Full verification must reject it.
    let (p, mut contract) = resolved(BUGGY);
    contract
        .preserved
        .push(concir::explore::contract::PreservedBehavior {
            description: "x reaches 2".into(),
            behavior: concir::explore::contract::Preserved::Reachable(
                concir::sem::system::Predicate::VarEq {
                    resource: program::lower(&p)
                        .unwrap()
                        .resolve_resource(concir::sem::ids::ModuleId(0), "x")
                        .unwrap(),
                    value: concir::sem::value::Value::Int(2),
                },
            ),
        });
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
    let report = run_repair(&p, &contract, &mut provider, 4);
    assert_ne!(report.outcome, RepairOutcome::Repaired);
    assert!(report
        .rounds
        .iter()
        .any(|r| !r.accepted && r.reason.contains("preserved")));
}

#[test]
fn budget_exhaustion_when_no_candidate_satisfies() {
    // Add an impossible preserved reachability; both lock-order candidates are
    // rejected, and the budget bounds the search.
    let (p, mut contract) = resolved(BUGGY);
    contract.bounds.max_states = 20_000;
    contract
        .preserved
        .push(concir::explore::contract::PreservedBehavior {
            description: "impossible".into(),
            behavior: concir::explore::contract::Preserved::Reachable(
                concir::sem::system::Predicate::False,
            ),
        });
    let mut provider = LockOrderEnumerator::new(&p, &contract.allowed_scope);
    let report = run_repair(&p, &contract, &mut provider, 1);
    assert_eq!(report.outcome, RepairOutcome::BudgetExhausted);
    assert!(report.rounds.iter().all(|r| !r.accepted));
}
