//! Reproductions for the static-validator risks in `doc/backend-design.md` §9,
//! now fixed. Each test first shows the condition and asserts the expected
//! diagnostic (or its absence) after the fix.

use concir::ast::Program;
use concir::diagnostic::ValidationReport;
use concir::validate;

fn check(src: &str) -> ValidationReport {
    let p: Program = serde_json::from_str(src).unwrap();
    validate::validate(&p)
}

fn codes(report: &ValidationReport) -> Vec<String> {
    report
        .diagnostics
        .iter()
        .map(|d| d.code.to_string())
        .collect()
}

// ── R1: dst writes into a protected Var must hold the lock ──────────

const R1_ATOMIC_LOAD: &str = r#"{
  "program": "r1a",
  "modules": [{
    "name": "main",
    "resources": [
      {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"},
      {"name": "v", "kind": "var", "type": "Var", "base": "Int", "init": 0},
      {"name": "a", "kind": "var", "type": "Atomic", "base": "Int", "init": 0}
    ],
    "protection": [{"var": "v", "lock": "m"}],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "atomic_load", "resource": "a", "dst": "v"},
        {"sid": "s2", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

#[test]
fn r1_atomic_load_dst_into_protected_var_needs_lock() {
    let report = check(R1_ATOMIC_LOAD);
    assert!(codes(&report).contains(&"E309".to_string()), "{:?}", codes(&report));
}

const R1_CHANNEL_RECV: &str = r#"{
  "program": "r1c",
  "modules": [{
    "name": "main",
    "resources": [
      {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"},
      {"name": "v", "kind": "var", "type": "Var", "base": "Int", "init": 0},
      {"name": "ch", "kind": "sync", "type": "Channel", "mode": "Sync", "base": "Int", "capacity": 1}
    ],
    "protection": [{"var": "v", "lock": "m"}],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "channel_recv", "channel": "ch", "dst": "v"},
        {"sid": "s2", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

#[test]
fn r1_channel_recv_dst_into_protected_var_needs_lock() {
    let report = check(R1_CHANNEL_RECV);
    assert!(codes(&report).contains(&"E309".to_string()), "{:?}", codes(&report));
}

const R1_CALL_DST: &str = r#"{
  "program": "r1call",
  "modules": [{
    "name": "main",
    "resources": [
      {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"},
      {"name": "v", "kind": "var", "type": "Var", "base": "Int", "init": 0}
    ],
    "protection": [{"var": "v", "lock": "m"}],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "call", "func": "id", "args": ["1"], "dst": "v"},
        {"sid": "s2", "kind": "return"}
      ]},
      {"name": "id", "kind": "normal",
       "params": [{"name": "n", "type": "Int", "modeled": true}],
       "returns": {"name": "out", "type": "Int", "modeled": true},
       "body": [{"sid": "s1", "kind": "return", "value": "n"}]}
    ]
  }],
  "entry": "main::main"
}"#;

#[test]
fn r1_call_dst_into_protected_var_needs_lock() {
    let report = check(R1_CALL_DST);
    assert!(codes(&report).contains(&"E309".to_string()), "{:?}", codes(&report));
}

// ── R2: condvar_wait must hold the paired lock ──────────────────────

const R2: &str = r#"{
  "program": "r2",
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
fn r2_condvar_wait_requires_lock_ownership() {
    let report = check(R2);
    assert!(codes(&report).contains(&"E512".to_string()), "{:?}", codes(&report));
}

// ── R3: may_block coverage and call propagation ─────────────────────

const R3_SEND: &str = r#"{
  "program": "r3send",
  "modules": [{
    "name": "main",
    "resources": [
      {"name": "ch", "kind": "sync", "type": "Channel", "mode": "Sync", "base": "Int", "capacity": 1}
    ],
    "functions": [
      {"name": "main", "kind": "normal", "may_block": false, "body": [
        {"sid": "s1", "kind": "channel_send", "channel": "ch", "value": "1"},
        {"sid": "s2", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

#[test]
fn r3_channel_send_counts_as_blocking() {
    let report = check(R3_SEND);
    assert!(codes(&report).contains(&"E802".to_string()), "{:?}", codes(&report));
}

const R3_PROPAGATION: &str = r#"{
  "program": "r3prop",
  "modules": [{
    "name": "main",
    "resources": [{"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"}],
    "functions": [
      {"name": "main", "kind": "normal", "may_block": false, "body": [
        {"sid": "s1", "kind": "call", "func": "g", "args": []},
        {"sid": "s2", "kind": "return"}
      ]},
      {"name": "g", "kind": "normal", "body": [
        {"sid": "s1", "kind": "mutex_lock", "resource": "m"},
        {"sid": "s2", "kind": "mutex_unlock", "resource": "m"},
        {"sid": "s3", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

#[test]
fn r3_may_block_propagates_through_calls() {
    let report = check(R3_PROPAGATION);
    assert!(codes(&report).contains(&"E802".to_string()), "{:?}", codes(&report));
}

// ── R4: requires_held is an analysis entry condition ────────────────

const R4: &str = r#"{
  "program": "r4",
  "modules": [{
    "name": "main",
    "resources": [
      {"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"},
      {"name": "v", "kind": "var", "type": "Var", "base": "Int", "init": 0}
    ],
    "protection": [{"var": "v", "lock": "m"}],
    "functions": [
      {"name": "main", "kind": "normal", "locks": {"requires_held": ["m"]}, "body": [
        {"sid": "s1", "kind": "read_shared", "resource": "v"},
        {"sid": "s2", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

#[test]
fn r4_requires_held_suppresses_false_e309() {
    let report = check(R4);
    assert!(
        !codes(&report).contains(&"E309".to_string()),
        "requires_held should be the entry condition: {:?}",
        codes(&report)
    );
}

// ── R5: cross-module resource identity ──────────────────────────────

const R5: &str = r#"{
  "program": "r5",
  "modules": [
    {
      "name": "a",
      "provides": {"resources": ["ma", "x"], "functions": ["fa"]},
      "requires": {"resources": [], "functions": []},
      "resources": [
        {"name": "ma", "kind": "sync", "type": "Mutex", "mode": "Sync"},
        {"name": "x", "kind": "var", "type": "Var", "base": "Int", "init": 0}
      ],
      "protection": [{"var": "x", "lock": "ma"}],
      "functions": [
        {"name": "fa", "kind": "normal", "body": [
          {"sid": "s1", "kind": "mutex_lock", "resource": "ma"},
          {"sid": "s2", "kind": "read_shared", "resource": "x"},
          {"sid": "s3", "kind": "mutex_unlock", "resource": "ma"},
          {"sid": "s4", "kind": "return"}
        ]}
      ]
    },
    {
      "name": "b",
      "provides": {"resources": ["mb", "x"], "functions": ["fb"]},
      "requires": {"resources": [], "functions": []},
      "resources": [
        {"name": "mb", "kind": "sync", "type": "Mutex", "mode": "Sync"},
        {"name": "x", "kind": "var", "type": "Var", "base": "Int", "init": 0}
      ],
      "protection": [{"var": "x", "lock": "mb"}],
      "functions": [
        {"name": "fb", "kind": "normal", "body": [
          {"sid": "s1", "kind": "read_shared", "resource": "x"},
          {"sid": "s2", "kind": "return"}
        ]}
      ]
    }
  ],
  "entry": "a::fa"
}"#;

#[test]
fn r5_protection_is_module_aware() {
    let report = check(R5);
    let e309: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|d| d.code == "E309")
        .collect();
    // Exactly one violation: b::fb reads b::x without b's lock. a::fa is fine,
    // so the same short name "x" must not be conflated across modules.
    assert_eq!(e309.len(), 1, "{:?}", e309);
    assert!(e309[0]
        .location
        .as_deref()
        .unwrap_or_default()
        .contains("b::fb"));
}
