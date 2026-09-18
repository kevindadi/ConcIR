use std::collections::HashSet;

use concir::ast::Program;
use concir::explore;
use concir::explore::contract::{ContractSpec, PredicateSpec};
use concir::interp::Interpreter;
use concir::petri::PetriEngine;
use concir::sem::outcome::AnalysisBounds;
use concir::sem::program;
use concir::sem::system::TransitionSystem;

fn bounds() -> AnalysisBounds {
    AnalysisBounds {
        max_threads: 8,
        max_frames_per_thread: 8,
        max_states: 50_000,
        max_depth: 100,
        ..Default::default()
    }
}

fn lower(src: &str) -> concir::sem::program::SemProgram {
    let p: Program = serde_json::from_str(src).unwrap();
    program::lower(&p).unwrap()
}

fn finished_reachable_it(sp: &concir::sem::program::SemProgram) -> (usize, bool) {
    let b = bounds();
    let it = Interpreter::new(sp, b.clone());
    let r = explore::explore(&it, &b);
    let fin = r.states.iter().any(|s| it.is_finished(s));
    (r.states.len(), fin)
}

fn finished_reachable_pn(sp: &concir::sem::program::SemProgram) -> (usize, bool) {
    let b = bounds();
    let pn = PetriEngine::new(sp, b.clone());
    let r = explore::explore(&pn, &b);
    let fin = r.states.iter().any(|s| pn.is_finished(s));
    (r.states.len(), fin)
}

#[test]
fn both_engines_terminate_producer_consumer() {
    let sp = lower(include_str!("../examples/producer_consumer.json"));
    let (it_states, it_fin) = finished_reachable_it(&sp);
    let (pn_states, pn_fin) = finished_reachable_pn(&sp);
    eprintln!("pc: interp states={it_states} fin={it_fin}; petri states={pn_states} fin={pn_fin}");
    assert!(it_fin, "interpreter must reach termination");
    assert!(pn_fin, "petri net must reach termination");
}

#[test]
fn both_engines_terminate_state_machine() {
    let sp = lower(include_str!("../examples/state_machine.json"));
    let (it_states, it_fin) = finished_reachable_it(&sp);
    let (pn_states, pn_fin) = finished_reachable_pn(&sp);
    eprintln!("sm: interp states={it_states} fin={it_fin}; petri states={pn_states} fin={pn_fin}");
    assert!(it_fin);
    assert!(pn_fin);
}

fn contract_json() -> &'static str {
    r#"{
      "name": "basic",
      "properties": [
        {"kind": "deadlock_free", "id": "no-deadlock"}
      ]
    }"#
}

fn verify_outcome(
    sp: &concir::sem::program::SemProgram,
    src: &str,
) -> concir::sem::outcome::Outcome {
    let spec: ContractSpec = serde_json::from_str(src).unwrap();
    let contract = spec.resolve(sp).unwrap();
    let b = contract.bounds.clone();
    let it = Interpreter::new(sp, b.clone());
    let report = explore::verify(&it, &contract);
    report.outcome
}

#[test]
fn deadlock_property_agrees() {
    let sp = lower(include_str!("../examples/producer_consumer.json"));
    let spec: ContractSpec = serde_json::from_str(contract_json()).unwrap();
    let contract = spec.resolve(&sp).unwrap();
    let b = contract.bounds.clone();
    let it = Interpreter::new(&sp, b.clone());
    let pn = PetriEngine::new(&sp, b);
    let itr = explore::verify(&it, &contract);
    let pnr = explore::verify(&pn, &contract);
    eprintln!("interp={:?} petri={:?}", itr.outcome, pnr.outcome);
    for d in &pnr.diagnostics {
        eprintln!("PETRI DIAG {}: {}", d.property, d.message);
        eprintln!("  instances: {:?}", d.final_instances);
        eprintln!("  blocked: {:?}", d.blocked);
    }
    for d in &itr.diagnostics {
        eprintln!("INTERP DIAG {}: {}", d.property, d.message);
        eprintln!("  instances: {:?}", d.final_instances);
        eprintln!("  blocked: {:?}", d.blocked);
    }
    assert_eq!(itr.outcome, pnr.outcome);
    let _ = verify_outcome;
}

#[test]
fn ef_and_agef_on_small_program() {
    // A program where a goal is initially reachable but a branch can make it
    // unreachable: AG EF should fail, EF should pass.
    let src = r#"{
      "program": "agef",
      "version": "3.5.0",
      "modules": [{
        "name": "main",
        "resources": [
          {"name": "x", "kind": "var", "type": "Var", "base": "Int", "init": 0},
          {"name": "y", "kind": "var", "type": "Var", "base": "Int", "init": 0}
        ],
        "protection": [],
        "functions": [
          {"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "scope", "funcs": ["a", "b"]},
            {"sid": "s2", "kind": "return"}
          ]},
          {"name": "a", "kind": "normal", "form": "closure", "body": [
            {"sid": "s1", "kind": "write_shared", "resource": "x", "expr": "1"},
            {"sid": "s2", "kind": "return"}
          ]},
          {"name": "b", "kind": "normal", "form": "closure", "body": [
            {"sid": "s1", "kind": "write_shared", "resource": "y", "expr": "1"},
            {"sid": "s2", "kind": "return"}
          ]}
        ]
      }],
      "entry": "main::main"
    }"#;
    let sp = lower(src);
    let spec_src = r#"{
      "name": "agef",
      "properties": [
        {"kind": "reachability", "id": "ef-x", "goal": {"kind": "var_eq", "resource": "x", "value": 1}},
        {"kind": "always_reachable", "id": "agef-x", "goal": {"kind": "var_eq", "resource": "x", "value": 1}}
      ]
    }"#;
    let spec: ContractSpec = serde_json::from_str(spec_src).unwrap();
    let contract = spec.resolve(&sp).unwrap();
    let b = contract.bounds.clone();
    let it = Interpreter::new(&sp, b.clone());
    let pn = PetriEngine::new(&sp, b);
    let itr = explore::verify(&it, &contract);
    let pnr = explore::verify(&pn, &contract);
    for r in &itr.properties {
        eprintln!("interp prop {} -> {:?}", r.id, r.outcome);
    }
    for r in &pnr.properties {
        eprintln!("petri  prop {} -> {:?}", r.id, r.outcome);
    }
    // x is always eventually written by `a`, so both EF and AG EF hold.
    assert_eq!(itr.outcome, pnr.outcome);
    let _ = PredicateSpec::True;
    let _ = HashSet::<u8>::new();
}
