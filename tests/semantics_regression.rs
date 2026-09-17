//! Hand-designed semantic regression tests for the backend.
//!
//! These exercise the reference interpreter directly and the explorer through
//! `verify`, covering the cases listed in the project brief.

use concir::ast::Program;
use concir::explore::contract::ContractSpec;
use concir::explore::{explore, verify, VerificationReport};
use concir::interp::Interpreter;
use concir::petri::PetriEngine;
use concir::sem::outcome::{AnalysisBounds, Outcome};
use concir::sem::program;
use concir::sem::system::TransitionSystem;

fn lower(src: &str) -> concir::sem::program::SemProgram {
    let p: Program = serde_json::from_str(src).unwrap();
    program::lower(&p).unwrap()
}

fn bounds() -> AnalysisBounds {
    AnalysisBounds {
        max_threads: 8,
        max_frames_per_thread: 8,
        max_states: 20_000,
        max_depth: 200,
        max_boundary_events: 256,
    }
}

fn report(program_src: &str, contract_src: &str) -> VerificationReport {
    let sp = lower(program_src);
    let spec: ContractSpec = serde_json::from_str(contract_src).unwrap();
    let contract = spec.resolve(&sp).unwrap();
    let engine = Interpreter::new(&sp, contract.bounds.clone());
    verify(&engine, &contract)
}

fn report_petri(program_src: &str, contract_src: &str) -> VerificationReport {
    let sp = lower(program_src);
    let spec: ContractSpec = serde_json::from_str(contract_src).unwrap();
    let contract = spec.resolve(&sp).unwrap();
    let engine = PetriEngine::new(&sp, contract.bounds.clone());
    verify(&engine, &contract)
}

const DEADLOCK_FREE: &str = r#"{"name":"c","properties":[{"kind":"deadlock_free","id":"d"}]}"#;

// ── 1. locals isolation ─────────────────────────────────────────────

const LOCALS_ISOLATION: &str = r#"{
  "program": "locals",
  "modules": [{
    "name": "main",
    "resources": [{"name": "x", "kind": "var", "type": "Var", "base": "Int", "init": 0}],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "spawn", "func": "f", "handle": "h1"},
        {"sid": "s2", "kind": "spawn", "func": "f", "handle": "h2"},
        {"sid": "s3", "kind": "join", "handle": "h1"},
        {"sid": "s4", "kind": "join", "handle": "h2"},
        {"sid": "s5", "kind": "return"}
      ]},
      {"name": "f", "kind": "normal", "form": "closure",
       "locals": [{"name": "tmp", "type": "Int"}],
       "body": [
        {"sid": "s1", "kind": "assign_local", "target": "tmp", "expr": "1"},
        {"sid": "s2", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

#[test]
fn two_threads_same_function_have_independent_locals() {
    let sp = lower(LOCALS_ISOLATION);
    let it = Interpreter::new(&sp, bounds());
    let r = explore(&it, &bounds());
    // Find a reachable state with two live frames of function f whose `tmp`
    // local is present in both, and assert they are distinct frame ids.
    let f_id = sp.function_by_name("main::f").unwrap();
    let mut checked = false;
    for s in &r.states {
        let frames: Vec<_> = s
            .store
            .frames
            .values()
            .filter(|fr| fr.function == f_id)
            .collect();
        if frames.len() == 2 {
            let a = &frames[0];
            let b = &frames[1];
            assert_ne!(a.id, b.id);
            assert_eq!(a.locals.get(&0), b.locals.get(&0));
            checked = true;
            break;
        }
    }
    assert!(checked, "expected a reachable state with two f frames");
}

// ── 2. overlapping calls: returns are matched ────────────────────────

const OVERLAPPING_CALLS: &str = r#"{
  "program": "calls",
  "modules": [{
    "name": "main",
    "resources": [
      {"name": "ra", "kind": "var", "type": "Var", "base": "Int", "init": 0},
      {"name": "rb", "kind": "var", "type": "Var", "base": "Int", "init": 0}
    ],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "scope", "funcs": ["a", "b"]},
        {"sid": "s2", "kind": "return"}
      ]},
      {"name": "a", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "call", "func": "id", "args": ["11"], "dst": "ra"},
        {"sid": "s2", "kind": "return"}
      ]},
      {"name": "b", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "call", "func": "id", "args": ["22"], "dst": "rb"},
        {"sid": "s2", "kind": "return"}
      ]},
      {"name": "id", "kind": "normal",
       "params": [{"name": "n", "type": "Int", "modeled": true}],
       "returns": {"name": "out", "type": "Int", "modeled": true},
       "body": [
        {"sid": "s1", "kind": "return", "value": "n"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

#[test]
fn overlapping_calls_do_not_cross_returns() {
    // Each call must return its own argument to its own destination. `ra`
    // must never receive 22 and `rb` must never receive 11.
    let sp = lower(OVERLAPPING_CALLS);
    let it = Interpreter::new(&sp, bounds());
    let r = explore(&it, &bounds());
    let ra = sp.resolve_resource(concir::sem::ids::ModuleId(0), "ra").unwrap();
    let rb = sp.resolve_resource(concir::sem::ids::ModuleId(0), "rb").unwrap();
    let mut finished_state = false;
    for s in &r.states {
        if let Some(v) = s.store.vars.get(&ra) {
            assert_ne!(v.as_int(), Some(22), "crossed return into ra");
        }
        if let Some(v) = s.store.vars.get(&rb) {
            assert_ne!(v.as_int(), Some(11), "crossed return into rb");
        }
        if it.is_finished(s) {
            assert_eq!(s.store.vars.get(&ra).and_then(|v| v.as_int()), Some(11));
            assert_eq!(s.store.vars.get(&rb).and_then(|v| v.as_int()), Some(22));
            finished_state = true;
        }
    }
    assert!(finished_state, "both calls should complete");
}

// ── 3. cross-module same-named resources ────────────────────────────

const CROSS_MODULE: &str = r#"{
  "program": "modules",
  "modules": [
    {
      "name": "a",
      "provides": {"resources": ["x"], "functions": ["main", "fa"]},
      "requires": {"resources": ["b::x"], "functions": ["b::fb"]},
      "resources": [{"name": "x", "kind": "var", "type": "Var", "base": "Int", "init": 0}],
      "functions": [
        {"name": "main", "kind": "normal", "body": [
          {"sid": "s1", "kind": "call", "func": "fa", "args": []},
          {"sid": "s2", "kind": "call", "func": "b::fb", "args": []},
          {"sid": "s3", "kind": "return"}
        ]},
        {"name": "fa", "kind": "normal", "body": [
          {"sid": "s1", "kind": "write_shared", "resource": "x", "expr": "1"},
          {"sid": "s2", "kind": "return"}
        ]}
      ]
    },
    {
      "name": "b",
      "provides": {"resources": ["x"], "functions": ["fb"]},
      "requires": {"resources": [], "functions": []},
      "resources": [{"name": "x", "kind": "var", "type": "Var", "base": "Int", "init": 0}],
      "functions": [
        {"name": "fb", "kind": "normal", "body": [
          {"sid": "s1", "kind": "write_shared", "resource": "x", "expr": "2"},
          {"sid": "s2", "kind": "return"}
        ]}
      ]
    }
  ],
  "entry": "a::main"
}"#;

#[test]
fn same_named_resources_in_different_modules_do_not_collide() {
    let sp = lower(CROSS_MODULE);
    let ax = sp.resolve_resource(concir::sem::ids::ModuleId(0), "x").unwrap();
    let bx = sp.resolve_resource(concir::sem::ids::ModuleId(0), "b::x").unwrap();
    assert_ne!(ax, bx);
    let contract = r#"{
      "name": "cross",
      "properties": [{"kind": "reachability", "id": "both", "goal":
        {"kind": "and", "predicates": [
          {"kind": "var_eq", "resource": "a::x", "value": 1},
          {"kind": "var_eq", "resource": "b::x", "value": 2}
        ]}}]
    }"#;
    let rep = report(CROSS_MODULE, contract);
    assert_eq!(rep.outcome, Outcome::Pass, "{:?}", rep.properties);
    // And b::x is never 1 (which would indicate a collision with a::x).
    let it = Interpreter::new(&sp, bounds());
    let r = explore(&it, &bounds());
    for s in &r.states {
        if let Some(v) = s.store.vars.get(&bx) {
            assert_ne!(v.as_int(), Some(1));
        }
    }
}

// ── 4. non-owner unlock is Invalid ──────────────────────────────────

const NON_OWNER_UNLOCK: &str = r#"{
  "program": "unlock",
  "modules": [{
    "name": "main",
    "resources": [{"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"}],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "mutex_lock", "resource": "m"},
        {"sid": "s2", "kind": "mutex_unlock", "resource": "m"},
        {"sid": "s3", "kind": "mutex_unlock", "resource": "m"},
        {"sid": "s4", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

#[test]
fn non_owner_unlock_is_rejected() {
    let sp = lower(NON_OWNER_UNLOCK);
    let it = Interpreter::new(&sp, bounds());
    let init = it.initial().unwrap();
    let step1 = it.successors(&init).unwrap().steps.remove(0).state;
    let step2 = it.successors(&step1).unwrap().steps.remove(0).state;
    // The lock is free; unlocking again must be a semantic error.
    assert!(it.successors(&step2).is_err());
}

// ── 5. condvar wait without the lock is Invalid ─────────────────────

const WAIT_WITHOUT_LOCK: &str = r#"{
  "program": "wait",
  "modules": [{
    "name": "main",
    "resources": [
      {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"},
      {"name": "cv", "kind": "sync", "type": "Condvar", "mode": "Sync"}
    ],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "condvar_wait", "condvar": "cv", "lock": "m"},
        {"sid": "s2", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

#[test]
fn condvar_wait_without_lock_is_rejected() {
    let sp = lower(WAIT_WITHOUT_LOCK);
    let it = Interpreter::new(&sp, bounds());
    let init = it.initial().unwrap();
    assert!(it.successors(&init).is_err());
}

// ── 6/7. notify before wait leaves no permit ────────────────────────

const EARLY_NOTIFY: &str = r#"{
  "program": "early",
  "modules": [{
    "name": "main",
    "resources": [
      {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"},
      {"name": "cv", "kind": "sync", "type": "Condvar", "mode": "Sync"}
    ],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "scope", "funcs": ["notifier", "waiter"]},
        {"sid": "s2", "kind": "return"}
      ]},
      {"name": "notifier", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "mutex_lock", "resource": "m"},
        {"sid": "s2", "kind": "condvar_notify", "condvar": "cv"},
        {"sid": "s3", "kind": "mutex_unlock", "resource": "m"},
        {"sid": "s4", "kind": "return"}
      ]},
      {"name": "waiter", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "mutex_lock", "resource": "m"},
        {"sid": "s2", "kind": "condvar_wait", "condvar": "cv", "lock": "m"},
        {"sid": "s3", "kind": "mutex_unlock", "resource": "m"},
        {"sid": "s4", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

#[test]
fn notify_before_wait_leaves_no_future_notification() {
    // If the notifier runs entirely before the waiter waits, the waiter blocks
    // forever: notifications are not stored.
    let rep = report(EARLY_NOTIFY, DEADLOCK_FREE);
    assert_eq!(rep.outcome, Outcome::Fail, "{:?}", rep.properties);
}

#[test]
fn notify_all_does_not_wake_a_later_waiter() {
    // Same shape, with notify_all: a waiter arriving after the notify must not
    // consume the old notification.
    let src = EARLY_NOTIFY.replace("condvar_notify", "condvar_notify_all");
    let rep = report(&src, DEADLOCK_FREE);
    assert_eq!(rep.outcome, Outcome::Fail);
}

// ── 9/10. channel capacity + FIFO and rendezvous ────────────────────

const BUFFERED_FIFO: &str = r#"{
  "program": "fifo",
  "modules": [{
    "name": "main",
    "resources": [{"name": "ch", "kind": "sync", "type": "Channel", "mode": "Sync", "base": "Int", "capacity": 1}],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "scope", "funcs": ["p", "c"]},
        {"sid": "s2", "kind": "return"}
      ]},
      {"name": "p", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "channel_send", "channel": "ch", "value": "1"},
        {"sid": "s2", "kind": "channel_send", "channel": "ch", "value": "2"},
        {"sid": "s3", "kind": "return"}
      ]},
      {"name": "c", "kind": "normal", "form": "closure",
       "locals": [{"name": "a", "type": "Int"}, {"name": "b", "type": "Int"}],
       "body": [
        {"sid": "s1", "kind": "channel_recv", "channel": "ch", "dst": "a"},
        {"sid": "s2", "kind": "channel_recv", "channel": "ch", "dst": "b"},
        {"sid": "s3", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

#[test]
fn buffered_channel_has_fifo_order() {
    let sp = lower(BUFFERED_FIFO);
    let it = Interpreter::new(&sp, bounds());
    let r = explore(&it, &bounds());
    // No reachable state may have the receiver's locals as (a=2, b=1): FIFO
    // forbids receiving 2 before 1.
    let c_id = sp.function_by_name("main::c").unwrap();
    for s in &r.states {
        for fr in s.store.frames.values().filter(|f| f.function == c_id) {
            if let (Some(a), Some(b)) = (fr.locals.get(&0), fr.locals.get(&1)) {
                assert!(!(a.as_int() == Some(2) && b.as_int() == Some(1)));
            }
        }
    }
    assert_eq!(report(BUFFERED_FIFO, DEADLOCK_FREE).outcome, Outcome::Pass);
}

const RENDEZVOUS: &str = r#"{
  "program": "rdv",
  "modules": [{
    "name": "main",
    "resources": [{"name": "ch", "kind": "sync", "type": "Channel", "mode": "Sync", "base": "Int", "capacity": 0}],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "scope", "funcs": ["p", "c"]},
        {"sid": "s2", "kind": "return"}
      ]},
      {"name": "p", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "channel_send", "channel": "ch", "value": "7"},
        {"sid": "s2", "kind": "return"}
      ]},
      {"name": "c", "kind": "normal", "form": "closure",
       "locals": [{"name": "m", "type": "Int"}],
       "body": [
        {"sid": "s1", "kind": "channel_recv", "channel": "ch", "dst": "m"},
        {"sid": "s2", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

#[test]
fn zero_capacity_channel_rendezvous() {
    let sp = lower(RENDEZVOUS);
    let it = Interpreter::new(&sp, bounds());
    let r = explore(&it, &bounds());
    let c_id = sp.function_by_name("main::c").unwrap();
    let mut saw_seven = false;
    for s in &r.states {
        for fr in s.store.frames.values().filter(|f| f.function == c_id) {
            if fr.locals.get(&0).and_then(|v| v.as_int()) == Some(7) {
                saw_seven = true;
            }
        }
    }
    assert!(saw_seven, "receiver must receive the sent value");
    assert_eq!(report(RENDEZVOUS, DEADLOCK_FREE).outcome, Outcome::Pass);
}

// ── 11. local permanent deadlock with a bystander thread ────────────

const LOCAL_DEADLOCK_WITH_BYSTANDER: &str = r#"{
  "program": "bystander",
  "modules": [{
    "name": "main",
    "resources": [
      {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"},
      {"name": "cv", "kind": "sync", "type": "Condvar", "mode": "Sync"}
    ],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "scope", "funcs": ["waiter", "spinner"]},
        {"sid": "s2", "kind": "return"}
      ]},
      {"name": "waiter", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "mutex_lock", "resource": "m"},
        {"sid": "s2", "kind": "condvar_wait", "condvar": "cv", "lock": "m"},
        {"sid": "s3", "kind": "mutex_unlock", "resource": "m"},
        {"sid": "s4", "kind": "return"}
      ]},
      {"name": "spinner", "kind": "normal", "form": "closure", "bound": 1, "body": [
        {"sid": "s1", "kind": "goto", "target": "s1"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

#[test]
fn local_deadlock_is_detected_even_with_a_running_bystander() {
    // The spinner loops forever, but the waiter blocks forever; the global
    // deadlock definition requires no enabled program step, and the infinite
    // spinner keeps stepping, so this is *not* a global deadlock. Instead we
    // assert that the waiter never completes (AG EF of completion fails).
    let contract = r#"{
      "name": "live",
      "properties": [{"kind": "always_reachable", "id": "waiter-progress",
        "goal": {"kind": "function_completed", "function": "main::waiter"}}]
    }"#;
    let rep = report(LOCAL_DEADLOCK_WITH_BYSTANDER, contract);
    assert_eq!(rep.outcome, Outcome::Fail, "{:?}", rep.properties);
}

// ── 12. EF holds but AG EF fails ────────────────────────────────────

const EF_NOT_AGEF: &str = r#"{
  "program": "efagef",
  "modules": [{
    "name": "main",
    "resources": [
      {"name": "flag", "kind": "var", "type": "Var", "base": "Int", "init": 0},
      {"name": "x", "kind": "var", "type": "Var", "base": "Int", "init": 0}
    ],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "scope", "funcs": ["a", "b"]},
        {"sid": "s2", "kind": "return"}
      ]},
      {"name": "a", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "read_shared", "resource": "flag"},
        {"sid": "s2", "kind": "branch", "cond": "flag == 0", "then": "s3", "else": "s5"},
        {"sid": "s3", "kind": "write_shared", "resource": "x", "expr": "1"},
        {"sid": "s4", "kind": "goto", "target": "s6"},
        {"sid": "s5", "kind": "write_shared", "resource": "x", "expr": "2"},
        {"sid": "s6", "kind": "return"}
      ]},
      {"name": "b", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "write_shared", "resource": "flag", "expr": "1"},
        {"sid": "s2", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

#[test]
fn ef_holds_but_agef_fails() {
    let contract = r#"{
      "name": "efagef",
      "properties": [
        {"kind": "reachability", "id": "ef", "goal": {"kind": "var_eq", "resource": "x", "value": 1}},
        {"kind": "always_reachable", "id": "agef", "goal": {"kind": "var_eq", "resource": "x", "value": 1}}
      ]
    }"#;
    let rep = report(EF_NOT_AGEF, contract);
    assert_eq!(rep.properties[0].id, "ef");
    assert_eq!(rep.properties[0].outcome, Outcome::Pass);
    assert_eq!(rep.properties[1].outcome, Outcome::Fail);
    assert_eq!(rep.outcome, Outcome::Fail);
    // The Petri engine agrees.
    let pn = report_petri(EF_NOT_AGEF, contract);
    assert_eq!(pn.properties[0].outcome, Outcome::Pass);
    assert_eq!(pn.properties[1].outcome, Outcome::Fail);
}

// ── 13. normal termination is not a deadlock ────────────────────────

#[test]
fn normal_termination_is_not_reported_as_deadlock() {
    let rep = report(include_str!("../examples/producer_consumer.json"), DEADLOCK_FREE);
    assert_eq!(rep.outcome, Outcome::Pass);
}

// ── 14. truncation must not produce a Pass ──────────────────────────

const COUNT_LOOP: &str = r#"{
  "program": "loop",
  "modules": [{
    "name": "main",
    "resources": [{"name": "x", "kind": "var", "type": "Var",
      "base": {"Int": [0, 1001]}, "init": 0}],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "read_shared", "resource": "x"},
        {"sid": "s2", "kind": "branch", "cond": "x < 1000", "then": "s3", "else": "s5"},
        {"sid": "s3", "kind": "write_shared", "resource": "x", "expr": "x + 1"},
        {"sid": "s4", "kind": "goto", "target": "s1"},
        {"sid": "s5", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

#[test]
fn truncated_search_is_unknown_not_pass() {
    let sp = lower(COUNT_LOOP);
    let spec: ContractSpec = serde_json::from_str(DEADLOCK_FREE).unwrap();
    let mut contract = spec.resolve(&sp).unwrap();
    contract.bounds.max_states = 10;
    let engine = Interpreter::new(&sp, contract.bounds.clone());
    let rep = verify(&engine, &contract);
    assert!(!rep.complete);
    assert_eq!(rep.outcome, Outcome::Unknown, "{:?}", rep.properties);
    assert!(rep.states_explored <= 11);
}
