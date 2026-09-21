//! Unit tests for `codegen` and `conform` (no cargo build of the output).

use concir::ast::Program;
use concir::codegen::{generate, is_observable};
use concir::conform::{conform, conform_options};
use concir::sem::program;

const ABBA_FIXED: &str = r#"
{
  "program": "abba_fixed",
  "version": "3.5.0",
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
        {"sid": "s1", "kind": "mutex_lock", "resource": "main::a"},
        {"sid": "s2", "kind": "mutex_lock", "resource": "main::b"},
        {"sid": "s3", "kind": "mutex_unlock", "resource": "main::b"},
        {"sid": "s4", "kind": "mutex_unlock", "resource": "main::a"},
        {"sid": "s5", "kind": "return"}
      ]},
      {"name": "t2", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "mutex_lock", "resource": "main::a"},
        {"sid": "s2", "kind": "mutex_lock", "resource": "main::b"},
        {"sid": "s3", "kind": "mutex_unlock", "resource": "main::b"},
        {"sid": "s4", "kind": "mutex_unlock", "resource": "main::a"},
        {"sid": "s5", "kind": "return"}
      ]}
    ]
  }]
}
"#;

fn lowered() -> program::SemProgram {
    let program: Program = serde_json::from_str(ABBA_FIXED).expect("parse");
    program::lower(&program).expect("lower")
}

fn trace(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(t, s)| (t.to_string(), s.to_string())).collect()
}

#[test]
fn codegen_emits_trace_calls_and_sid_map() {
    let sem = lowered();
    let generated = generate(&sem).expect("codegen");
    // v2: operation-bound events (no standalone `ev` in generated code).
    assert!(generated.main_rs.contains("cir_trace::scope(tag, \"s1\")"));
    assert!(generated.main_rs.contains("cir_trace::lock(&shared."));
    assert!(!generated.main_rs.contains("cir_trace::ev(tag"));
    assert!(generated.main_rs.contains("// @cir s1"));
    assert!(generated.map.sids.contains_key("main::s1"));
    assert!(generated.map.holes.is_empty());
}

#[test]
fn conform_accepts_a_valid_trace() {
    let sem = lowered();
    let ok = trace(&[
        ("t0", "s1"),
        ("ts1_1", "s1"), ("ts1_1", "s2"), ("ts1_1", "s3"), ("ts1_1", "s4"),
        ("ts1_2", "s1"), ("ts1_2", "s2"), ("ts1_2", "s3"), ("ts1_2", "s4"),
    ]);
    let result = conform(&sem, &ok);
    assert_eq!(result.status, "conformant", "{:?}", result);
    // coverage is keyed per (function, sid): t1 and t2 each contribute four.
    assert!(result.coverage.sids_total >= 8);
    assert_eq!(result.coverage.sids_seen, result.coverage.sids_total);
}

#[test]
fn conform_rejects_a_swapped_trace() {
    let sem = lowered();
    let bad = trace(&[("t0", "s1"), ("ts1_1", "s2")]);
    let result = conform(&sem, &bad);
    assert_eq!(result.status, "violation");
    assert_eq!(result.event_index, Some(1));
    assert!(result.expected.contains(&"s1".to_string()));
}

#[test]
fn conform_reports_unknown_sid() {
    let sem = lowered();
    let bad = trace(&[("t0", "s1"), ("ts1_1", "s9")]);
    let result = conform(&sem, &bad);
    assert_eq!(result.status, "unknown_sid");
}

#[test]
fn lenient_unlock_allows_missing_unlock_events() {
    let sem = lowered();
    // t1 locks a then b; t2 then locks a. Without unlock events this is only
    // legal in extraction mode, where a guard drop is implicit.
    let no_unlocks = trace(&[
        ("t0", "s1"),
        ("ts1_1", "s1"), ("ts1_1", "s2"),
        ("ts1_2", "s1"),
    ]);
    assert_eq!(conform(&sem, &no_unlocks).status, "violation");
    let relaxed = conform_options(&sem, &no_unlocks, true, false);
    assert_eq!(relaxed.status, "conformant", "{relaxed:?}");
}

#[test]
fn observable_op_classification() {
    use concir::sem::program::SemOp;
    assert!(is_observable(&SemOp::MutexLock {
        resource: concir::sem::ids::ResourceId(0)
    }));
    assert!(!is_observable(&SemOp::Nop));
}
