//! Call targets resolve by FQN or same-module short name (E102).

use concir::ast::Program;
use concir::validate::validate;

fn wrap(resources: &str, functions: &str) -> Program {
    let json = format!(
        r#"{{
        "program": "call_check",
        "modules": [{{
            "name": "main",
            "provides": {{"resources": [], "functions": ["main"]}},
            "requires": {{"resources": [], "functions": []}},
            "resources": {resources},
            "protection": [],
            "functions": {functions}
        }}],
        "entry": "main::main"
    }}"#
    );
    serde_json::from_str(&json).expect("test ConcIR must parse")
}

#[test]
fn call_to_sync_bodied_function_is_valid() {
    let program = wrap(
        r#"[{"name": "m1", "kind": "sync", "type": "Mutex", "mode": "Sync"}]"#,
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "statements": [{"kind": "call", "func": "helper", "args": []}], "terminator": {"kind": "goto", "target": "s2"}},
                {"sid": "s2", "terminator": {"kind": "return"}}
            ]},
            {"name": "helper", "kind": "normal", "body": [
                {"sid": "s1", "statements": [{"kind": "mutex_lock", "resource": "m1"}], "terminator": {"kind": "goto", "target": "s2"}},
                {"sid": "s2", "statements": [{"kind": "mutex_unlock", "resource": "m1"}], "terminator": {"kind": "goto", "target": "s3"}},
                {"sid": "s3", "terminator": {"kind": "return"}}
            ]}
        ]"#,
    );
    let report = validate(&program);
    assert!(report.valid, "got: {:?}", report.diagnostics);
}

#[test]
fn call_to_bodyless_function_is_valid() {
    let program = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "statements": [{"kind": "call", "func": "compute", "args": []}], "terminator": {"kind": "goto", "target": "s2"}},
                {"sid": "s2", "terminator": {"kind": "return"}}
            ]},
            {"name": "compute", "kind": "normal", "body": [],
             "effects": {"reads": [], "writes": ["result"]}}
        ]"#,
    );
    let report = validate(&program);
    assert!(report.valid, "got: {:?}", report.diagnostics);
}

#[test]
fn call_to_undefined_function_is_an_error() {
    let program = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "statements": [{"kind": "call", "func": "missing", "args": []}], "terminator": {"kind": "goto", "target": "s2"}},
                {"sid": "s2", "terminator": {"kind": "return"}}
            ]}
        ]"#,
    );
    let report = validate(&program);
    assert!(!report.valid);
    assert!(
        report.diagnostics.iter().any(|d| d.code == "E102"),
        "got: {:?}",
        report.diagnostics
    );
}
