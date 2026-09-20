//! `holds_all` / `never_holds_all` preservation failures carry a machine-generated
//! repair hint (no task names; function/resource names only).

use concir::ast::Program;
use concir::explore::contract::ContractSpec;
use concir::explore::{verify_program, EngineKind};
use concir::sem::outcome::Outcome;

const RELEASE_EARLY: &str = r#"{
  "program": "release_early",
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
        {"sid": "s1", "kind": "scope", "funcs": ["main::w"]},
        {"sid": "s2", "kind": "return"}
      ]},
      {"name": "w", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "mutex_lock", "resource": "main::a"},
        {"sid": "s2", "kind": "mutex_unlock", "resource": "main::a"},
        {"sid": "s3", "kind": "mutex_lock", "resource": "main::b"},
        {"sid": "s4", "kind": "mutex_unlock", "resource": "main::b"},
        {"sid": "s5", "kind": "return"}
      ]}
    ]
  }]
}"#;

const CONTRACT: &str = r#"{
  "name": "hint",
  "properties": [{"kind": "deadlock_free", "id": "no-deadlock"}],
  "preserved": [
    {"kind": "reachable", "description": "w holds a and b at once",
     "goal": {"kind": "holds_all", "function": "main::w",
              "resources": ["main::a", "main::b"]}}
  ],
  "allowed_scope": {"allow_lock_reorder": true}
}"#;

#[test]
fn holds_all_failure_has_template_hint() {
    let program: Program = serde_json::from_str(RELEASE_EARLY).unwrap();
    let spec: ContractSpec = serde_json::from_str(CONTRACT).unwrap();
    let report = verify_program(&program, &spec, EngineKind::Petri);
    assert_eq!(report.outcome, Outcome::Fail);
    let diag = report
        .diagnostics
        .iter()
        .find(|d| d.property.contains("holds a and b"))
        .expect("holds_all diagnostic present");
    let hints = diag.repair_hints.join(" | ");
    assert!(hints.contains("holds all of"), "hint text: {hints}");
    assert!(hints.contains("`w`"), "hint names function: {hints}");
    assert!(hints.contains("[a, b]"), "hint names resources: {hints}");
    assert!(hints.contains("releasing early"), "hint suggests the fix: {hints}");
}
