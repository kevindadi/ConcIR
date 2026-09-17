//! Round-3 review regressions (B1–B8) plus strengthened independent checks.

use std::collections::VecDeque;

use concir::ast::Program;
use concir::explore::contract::ContractSpec;
use concir::explore::{verify_program, EngineKind, VerificationReport};
use concir::interp::Interpreter;
use concir::petri::exec::PetriEngine;
use concir::petri::net::{NetState, PlaceKey};
use concir::repair::candidates::{FileCandidateProvider, LockOrderEnumerator};
use concir::repair::{run_repair, RepairOutcome};
use concir::sem::outcome::{AnalysisBounds, Outcome};
use concir::sem::program;
use concir::sem::system::TransitionSystem;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("tests/repro_round3/{name}")).expect(name)
}

fn prog(name: &str) -> Program {
    serde_json::from_str(&fixture(name)).unwrap()
}

fn spec(name: &str) -> ContractSpec {
    serde_json::from_str(&fixture(name)).unwrap()
}

fn run(program_name: &str, contract_name: &str, engine: EngineKind) -> VerificationReport {
    verify_program(&prog(program_name), &spec(contract_name), engine)
}

fn run_str(src: &str, contract_src: &str, engine: EngineKind) -> VerificationReport {
    verify_program(
        &serde_json::from_str(src).unwrap(),
        &serde_json::from_str(contract_src).unwrap(),
        engine,
    )
}

fn both(program_name: &str, contract_name: &str) -> (Outcome, Outcome, bool, bool) {
    let i = run(program_name, contract_name, EngineKind::Interpreter);
    let p = run(program_name, contract_name, EngineKind::Petri);
    (i.outcome, p.outcome, i.complete, p.complete)
}

// ── B1 ──────────────────────────────────────────────────────────────

#[test]
fn b1_contract_only_resources_are_materialized() {
    for (p, c) in [
        ("r2_query_only_Var.json", "r2_query_only_Var_contract.json"),
        ("r2_query_only_Atomic.json", "r2_query_only_Atomic_contract.json"),
    ] {
        let (i, pe, ic, pc) = both(p, c);
        assert_eq!(i, Outcome::Pass, "{p}");
        assert_eq!(pe, Outcome::Pass, "{p}");
        assert!(ic && pc);
    }
    for (p, c) in [
        ("r2_query_only_Var_negated.json", "r2_query_only_Var_negated_contract.json"),
        ("r2_query_only_Atomic_negated.json", "r2_query_only_Atomic_negated_contract.json"),
    ] {
        let (i, pe, _, _) = both(p, c);
        assert_eq!(i, Outcome::Fail, "{p} must not be a false PASS");
        assert_eq!(pe, Outcome::Fail, "{p} must not be a false PASS");
    }
}

#[test]
fn b1_repair_must_reject_deleting_the_last_write() {
    // The initial state x=0 still violates AG(!(x==0)) after the write is
    // deleted, so the candidate must be rejected in both engines.
    let p = prog("r2_query_only_repair_original.json");
    let s = spec("r2_query_only_repair_contract.json");
    let mut provider =
        FileCandidateProvider::from_json(&fixture("r2_query_only_delete_patch.json")).unwrap();
    let report = run_repair(&p, &s, &mut provider, 4);
    assert_ne!(report.outcome, RepairOutcome::Repaired, "{:?}", report.rounds);
    assert!(report.rounds.iter().any(|r| !r.accepted));

    let (i, pe, _, _) = both("r2_query_only_repair_after.json", "r2_query_only_repair_contract.json");
    assert_eq!(i, Outcome::Fail);
    assert_eq!(pe, Outcome::Fail);
}

// ── B2 ──────────────────────────────────────────────────────────────

#[test]
fn b2_condvar_waiters_carry_their_own_locks() {
    let (i, pe, ic, pc) = both("r2_multi_lock_cv.json", "r2_multi_lock_cv_contract.json");
    assert_eq!(i, Outcome::Pass, "interpreter must accept per-waiter locks");
    assert_eq!(pe, Outcome::Pass);
    assert!(ic && pc);
}

// ── B3 ──────────────────────────────────────────────────────────────

#[test]
fn b3_rendezvous_enumerates_every_receiver() {
    let p = prog("r2_rendezvous_two_waiters.json");
    let sp = program::lower(&p).unwrap();
    let b = AnalysisBounds::default();

    // Interpreter.
    let it = Interpreter::new(&sp, b.clone());
    let mut q: VecDeque<_> = VecDeque::from([it.initial().unwrap()]);
    let mut seen = std::collections::HashSet::new();
    let mut interp_found = false;
    while let Some(s) = q.pop_front() {
        if !seen.insert(it.canonical(&s)) {
            continue;
        }
        let en = it.successors(&s).unwrap();
        if s.store.channels.values().any(|c| c.pending_recv.len() == 2) {
            assert_eq!(
                en.steps.len(),
                2,
                "a send with two waiting receivers must yield two matches"
            );
            let mut remaining: Vec<Vec<u64>> = en
                .steps
                .iter()
                .map(|st| {
                    st.state
                        .store
                        .channels
                        .values()
                        .flat_map(|c| c.pending_recv.iter().map(|t| t.0))
                        .collect()
                })
                .collect();
            remaining.sort();
            assert_ne!(remaining[0], remaining[1], "the two branches differ");
            interp_found = true;
            break;
        }
        for st in en.steps {
            q.push_back(st.state);
        }
    }
    assert!(interp_found);

    // Petri.
    let pn = PetriEngine::new(&sp, b.clone());
    let mut q: VecDeque<NetState> = VecDeque::from([pn.initial().unwrap()]);
    let mut seen = std::collections::HashSet::new();
    let mut petri_found = false;
    while let Some(s) = q.pop_front() {
        if !seen.insert(pn.canonical(&s)) {
            continue;
        }
        let en = pn.successors(&s).unwrap();
        let two = pn.net.places.iter().any(|pl| {
            matches!(pl.key, PlaceKey::ChannelRecv(_)) && s.place_tokens(pl.id).len() == 2
        });
        if two {
            assert_eq!(en.steps.len(), 2, "net must enumerate both receivers");
            petri_found = true;
            break;
        }
        for st in en.steps {
            q.push_back(st.state);
        }
    }
    assert!(petri_found);
}

// ── B4 ──────────────────────────────────────────────────────────────

#[test]
fn b4_contract_names_bind_to_the_entry_module() {
    // `x` must mean the entry module's `x`; reordering modules must not change
    // the verified object.
    for c in [
        "r2_namespace_used_False_contract.json",
        "r2_namespace_used_True_contract.json",
    ] {
        let (i, pe, _, _) = both("r2_namespace_used_False.json", c);
        assert_eq!(i, Outcome::Pass, "{c}");
        assert_eq!(pe, Outcome::Pass, "{c}");
        let (i2, pe2, _, _) = both("r2_namespace_used_True.json", c);
        assert_eq!(i2, Outcome::Pass, "{c} reversed modules");
        assert_eq!(pe2, Outcome::Pass);
    }
}

#[test]
fn b4_declaration_reordering_keeps_a_real_target() {
    // Same logical contract, modules/functions/resources reordered: the bound
    // predicate must still refer to main::x and stay stable.
    let src = r#"{
      "program": "reorder",
      "modules": [{"name": "main",
        "resources": [{"name": "x", "kind": "var", "type": "Var", "base": "Int", "init": 0}],
        "functions": [
          {"name": "main", "kind": "normal", "body": [{"sid":"s1","kind":"return"}]},
          {"name": "helper", "kind": "normal", "body": [{"sid":"s1","kind":"return"}]}
        ]}],
      "entry": "main::main"
    }"#;
    let reordered = r#"{
      "program": "reorder",
      "modules": [{"name": "main",
        "resources": [{"name": "x", "kind": "var", "type": "Var", "base": "Int", "init": 0}],
        "functions": [
          {"name": "helper", "kind": "normal", "body": [{"sid":"s1","kind":"return"}]},
          {"name": "main", "kind": "normal", "body": [{"sid":"s1","kind":"return"}]}
        ]}],
      "entry": "main::main"
    }"#;
    // Contract observes main::x == 0.
    let contract = r#"{"name":"c","properties":[{"kind":"safety","id":"z","invariant":{"kind":"var_eq","resource":"x","value":0}}]}"#;
    let a = run_str(src, contract, EngineKind::Petri);
    let b = run_str(reordered, contract, EngineKind::Petri);
    assert_eq!(a.outcome, Outcome::Pass);
    assert_eq!(b.outcome, Outcome::Pass);
    // And a non-entry-module `x = 1` must not be observed.
    let with_other = r#"{
      "program": "reorder",
      "modules": [
        {"name": "other", "resources": [{"name": "x", "kind": "var", "type": "Var", "base": "Int", "init": 1}], "functions": []},
        {"name": "main", "resources": [{"name": "x", "kind": "var", "type": "Var", "base": "Int", "init": 0}], "functions": [{"name": "main", "kind": "normal", "body": [{"sid":"s1","kind":"return"}]}]}
      ],
      "entry": "main::main"
    }"#;
    assert_eq!(
        run_str(with_other, contract, EngineKind::Petri).outcome,
        Outcome::Pass
    );
}

// ── B5 ──────────────────────────────────────────────────────────────

#[test]
fn b5_bounded_dst_paths_respect_the_domain() {
    // atomic_load dst out of domain.
    let (i, pe, ic, pc) = both(
        "r2_bounded_dst_with_place.json",
        "r2_bounded_dst_with_place_contract.json",
    );
    assert_eq!(i, Outcome::Fail, "out-of-domain write must be unreachable");
    assert_eq!(pe, Outcome::Fail);
    assert!(ic && pc);

    // channel_recv dst out of domain: the recv is disabled, so the goal is
    // unreachable and the program deadlocks (channel holds the value).
    let chan = r#"{
      "program": "chan_dst",
      "modules": [{"name": "main",
        "resources": [
          {"name": "ch", "kind": "sync", "type": "Channel", "mode": "Sync", "base": "Int", "capacity": 1},
          {"name": "x", "kind": "var", "type": "Var", "base": {"Int": [0, 1]}, "init": 0}
        ],
        "functions": [
          {"name": "main", "kind": "normal", "body": [{"sid":"s1","kind":"scope","funcs":["p","c"]},{"sid":"s2","kind":"return"}]},
          {"name": "p", "kind": "normal", "form": "closure", "body": [{"sid":"s1","kind":"channel_send","channel":"ch","value":"2"},{"sid":"s2","kind":"return"}]},
          {"name": "c", "kind": "normal", "form": "closure", "body": [{"sid":"s1","kind":"channel_recv","channel":"ch","dst":"x"},{"sid":"s2","kind":"return"}]}
        ]}],
      "entry": "main::main"
    }"#;
    let goal = r#"{"name":"c","properties":[{"kind":"reachability","id":"bad","goal":{"kind":"not","predicate":{"kind":"or","predicates":[{"kind":"var_eq","resource":"main::x","value":0},{"kind":"var_eq","resource":"main::x","value":1}]}}}]}"#;
    let a = run_str(chan, goal, EngineKind::Interpreter);
    let b = run_str(chan, goal, EngineKind::Petri);
    assert_eq!(a.outcome, Outcome::Fail, "{:?}", a.properties);
    assert_eq!(b.outcome, Outcome::Fail, "{:?}", b.properties);

    // call return dst out of domain.
    let call = r#"{
      "program": "call_dst",
      "modules": [{"name": "main",
        "resources": [{"name": "x", "kind": "var", "type": "Var", "base": {"Int": [0, 1]}, "init": 0}],
        "functions": [
          {"name": "main", "kind": "normal", "body": [{"sid":"s1","kind":"call","func":"two","args":[],"dst":"x"},{"sid":"s2","kind":"return"}]},
          {"name": "two", "kind": "normal", "returns": {"name":"r","type":"Int","modeled":true}, "body": [{"sid":"s1","kind":"return","value":"2"}]}
        ]}],
      "entry": "main::main"
    }"#;
    let a = run_str(call, goal, EngineKind::Interpreter);
    let b = run_str(call, goal, EngineKind::Petri);
    assert_eq!(a.outcome, Outcome::Fail, "call return dst: {:?}", a.properties);
    assert_eq!(b.outcome, Outcome::Fail, "call return dst: {:?}", b.properties);
}

// ── B6 ──────────────────────────────────────────────────────────────

#[test]
fn b6_finite_concurrent_loops_complete() {
    for (p, c) in [
        ("r2_scope_loop.json", "r2_scope_loop_contract.json"),
        ("r2_spawn_join_loop.json", "r2_spawn_join_loop_contract.json"),
    ] {
        let i = run(p, c, EngineKind::Interpreter);
        let pe = run(p, c, EngineKind::Petri);
        assert_eq!(i.outcome, Outcome::Pass, "{p} interp: {:?}", i.properties);
        assert_eq!(pe.outcome, Outcome::Pass, "{p} petri: {:?}", pe.properties);
        assert!(i.complete && pe.complete, "{p} must be complete");
        assert!(i.states_explored < 30 && pe.states_explored < 30, "{p}");
    }
}

#[test]
fn b6_nested_scopes_are_reclaimed() {
    let src = r#"{
      "program": "nested_scope",
      "modules": [{"name": "main",
        "resources": [],
        "functions": [
          {"name": "main", "kind": "normal", "body": [{"sid":"s1","kind":"scope","funcs":["outer"]},{"sid":"s2","kind":"return"}]},
          {"name": "outer", "kind": "normal", "form": "closure", "body": [{"sid":"s1","kind":"scope","funcs":["worker"]},{"sid":"s2","kind":"return"}]},
          {"name": "worker", "kind": "normal", "form": "closure", "body": [{"sid":"s1","kind":"return"}]}
        ]}],
      "entry": "main::main"
    }"#;
    let contract = r#"{"name":"c","properties":[{"kind":"deadlock_free","id":"d"}]}"#;
    let a = run_str(src, contract, EngineKind::Interpreter);
    let b = run_str(src, contract, EngineKind::Petri);
    assert_eq!(a.outcome, Outcome::Pass);
    assert_eq!(b.outcome, Outcome::Pass);
    assert!(a.complete && b.complete);
}

#[test]
fn b6_stale_handle_second_join_is_invalid() {
    let src = r#"{
      "program": "double_join",
      "modules": [{"name": "main",
        "resources": [],
        "functions": [
          {"name": "main", "kind": "normal", "body": [
            {"sid":"s1","kind":"spawn","func":"worker","handle":"h"},
            {"sid":"s2","kind":"join","handle":"h"},
            {"sid":"s3","kind":"join","handle":"h"},
            {"sid":"s4","kind":"return"}
          ]},
          {"name": "worker", "kind": "normal", "form": "closure", "body": [{"sid":"s1","kind":"return"}]}
        ]}],
      "entry": "main::main"
    }"#;
    let contract = r#"{"name":"c","properties":[{"kind":"deadlock_free","id":"d"}]}"#;
    let a = run_str(src, contract, EngineKind::Interpreter);
    let b = run_str(src, contract, EngineKind::Petri);
    assert_eq!(a.outcome, Outcome::Invalid, "double join must be a semantic error");
    assert_eq!(b.outcome, Outcome::Invalid);
}

#[test]
fn b6_completion_threshold_saturates() {
    let src = r#"{
      "program": "calls",
      "modules": [{"name": "main",
        "resources": [],
        "functions": [
          {"name": "main", "kind": "normal", "body": [
            {"sid":"s1","kind":"call","func":"worker","args":[]},
            {"sid":"s2","kind":"call","func":"worker","args":[]},
            {"sid":"s3","kind":"return"}
          ]},
          {"name": "worker", "kind": "normal", "body": [{"sid":"s1","kind":"return"}]}
        ]}],
      "entry": "main::main"
    }"#;
    let at2 = r#"{"name":"c","properties":[{"kind":"reachability","id":"two","goal":{"kind":"function_completed_at_least","function":"main::worker","n":2}}]}"#;
    let at3 = r#"{"name":"c","properties":[{"kind":"reachability","id":"three","goal":{"kind":"function_completed_at_least","function":"main::worker","n":3}}]}"#;
    assert_eq!(run_str(src, at2, EngineKind::Petri).outcome, Outcome::Pass);
    assert_eq!(run_str(src, at3, EngineKind::Petri).outcome, Outcome::Fail);
    assert_eq!(run_str(src, at2, EngineKind::Interpreter).outcome, Outcome::Pass);
    assert_eq!(run_str(src, at3, EngineKind::Interpreter).outcome, Outcome::Fail);
}

// ── B7 ──────────────────────────────────────────────────────────────

#[test]
fn b7_fqn_patch_scope_is_exact() {
    let p = prog("r2_scope_bug.json");
    let s = spec("r2_fqn_scope_contract.json");
    assert_eq!(s.allowed_scope.functions, vec!["main::t1".to_string()]);

    // File candidate on main::t1 is allowed and repairs.
    let mut provider =
        FileCandidateProvider::from_json(&fixture("r2_scope_swap_patch.json")).unwrap();
    let report = run_repair(&p, &s, &mut provider, 4);
    assert_eq!(report.outcome, RepairOutcome::Repaired, "{:?}", report.rounds);

    // Automatic enumerator produces the same allowed target.
    let mut provider = LockOrderEnumerator::new(&p, &s.allowed_scope);
    let report = run_repair(&p, &s, &mut provider, 4);
    assert_eq!(report.outcome, RepairOutcome::Repaired, "{:?}", report.rounds);
    assert_eq!(report.accepted.as_ref().unwrap().module, "main");
    assert_eq!(report.accepted.as_ref().unwrap().function, "t1");
}

#[test]
fn b7_patch_scope_excludes_other_module_same_function() {
    // A scope naming only other::t1 must not allow main::t1.
    let p = prog("r2_scope_bug.json");
    let mut s = spec("r2_fqn_scope_contract.json");
    s.allowed_scope.functions = vec!["other::t1".to_string()];
    let mut provider =
        FileCandidateProvider::from_json(&fixture("r2_scope_swap_patch.json")).unwrap();
    let report = run_repair(&p, &s, &mut provider, 4);
    assert_ne!(report.outcome, RepairOutcome::Repaired);
    assert!(report
        .rounds
        .iter()
        .any(|r| r.reason.contains("outside the allowed patch scope")));
    // The automatic enumerator also yields nothing.
    let mut provider = LockOrderEnumerator::new(&p, &s.allowed_scope);
    let report = run_repair(&p, &s, &mut provider, 4);
    assert_eq!(report.outcome, RepairOutcome::NoAcceptableCandidate);
    assert_eq!(report.candidates_tried, 0);
}

// ── B8 ──────────────────────────────────────────────────────────────

#[test]
fn b8_semaphore_overflow_is_structured_invalid() {
    let (i, pe, _, _) = both("r2_semaphore_overflow.json", "r2_semaphore_overflow_contract.json");
    assert_eq!(i, Outcome::Invalid);
    assert_eq!(pe, Outcome::Invalid);
    let r = run("r2_semaphore_overflow.json", "r2_semaphore_overflow_contract.json", EngineKind::Petri);
    assert!(r.invalid.iter().any(|x| x.code == "E905"), "{:?}", r.invalid);
}

// ── Metadata ────────────────────────────────────────────────────────

#[test]
fn early_exit_reports_requested_config_not_default() {
    let r = run(
        "invalid_protected_write_fixed_fixture.json",
        "invalid_protected_write_fixed_fixture_contract.json",
        EngineKind::Petri,
    );
    assert_eq!(r.outcome, Outcome::Invalid);
    assert!(!r.analysis_started);
    // Requested bounds (max_states 10000) are recorded, not the 200000 default.
    assert_eq!(r.bounds.max_states, 10_000);

    let ok = run(
        "r2_scope_loop.json",
        "r2_scope_loop_contract.json",
        EngineKind::Petri,
    );
    assert!(ok.analysis_started);
}
