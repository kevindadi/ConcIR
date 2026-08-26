//! Scope / spawn_batch / form checks (E010, E401, E410–E412).

use concir::ast::Program;
use concir::validate::validate;

fn wrap(resources: &str, functions: &str) -> concir::diagnostic::ValidationReport {
    let json = format!(
        r#"{{
            "program": "p",
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
    let program: Program = serde_json::from_str(&json).expect("test CIR must parse");
    validate(&program)
}

fn codes(report: &concir::diagnostic::ValidationReport) -> Vec<&str> {
    report.diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn spawn_inside_scope_without_join_is_valid() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "scope", "body": [
                {"sid": "s1", "kind": "spawn", "func": "worker", "handle": "h_w"},
                {"sid": "s2", "kind": "return"}
            ]},
            {"name": "worker", "kind": "normal", "form": "closure",
             "body": [{"sid": "s1", "kind": "return"}]}
        ]"#,
    );
    assert!(
        report.valid,
        "scope return joins leftover spawns. got: {:?}",
        report.diagnostics
    );
    assert!(
        !codes(&report).contains(&"E401"),
        "E401 must not fire inside a scope. got: {:?}",
        report.diagnostics
    );
}

#[test]
fn spawn_outside_scope_without_join_is_e401_warning() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "kind": "spawn", "func": "worker", "handle": "h_w"},
                {"sid": "s2", "kind": "return"}
            ]},
            {"name": "worker", "kind": "normal",
             "body": [{"sid": "s1", "kind": "return"}]}
        ]"#,
    );
    assert!(
        report.valid,
        "E401 is a warning. got: {:?}",
        report.diagnostics
    );
    assert!(
        codes(&report).contains(&"E401"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn spawn_batch_of_scope_is_valid() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "kind": "spawn_batch", "func": "section"},
                {"sid": "s2", "kind": "return"}
            ]},
            {"name": "section", "kind": "scope", "body": [
                {"sid": "s1", "kind": "spawn", "func": "a", "handle": "ha"},
                {"sid": "s2", "kind": "spawn", "func": "b", "handle": "hb"},
                {"sid": "s3", "kind": "return"}
            ]},
            {"name": "a", "kind": "normal", "form": "function",
             "body": [{"sid": "s1", "kind": "return"}]},
            {"name": "b", "kind": "normal", "form": "closure",
             "body": [{"sid": "s1", "kind": "return"}]}
        ]"#,
    );
    assert!(
        report.valid,
        "spawn_batch enters a scope; children may be function or closure. got: {:?}",
        report.diagnostics
    );
}

#[test]
fn spawn_batch_of_non_scope_is_e410() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "kind": "spawn_batch", "func": "worker"},
                {"sid": "s2", "kind": "return"}
            ]},
            {"name": "worker", "kind": "normal",
             "body": [{"sid": "s1", "kind": "return"}]}
        ]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E410"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn call_of_scope_is_e411() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "kind": "call", "func": "section"},
                {"sid": "s2", "kind": "return"}
            ]},
            {"name": "section", "kind": "scope",
             "body": [{"sid": "s1", "kind": "return"}]}
        ]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E411"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn spawn_of_scope_is_e411() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "kind": "spawn", "func": "section", "handle": "hs"},
                {"sid": "s2", "kind": "join", "handle": "hs"},
                {"sid": "s3", "kind": "return"}
            ]},
            {"name": "section", "kind": "scope",
             "body": [{"sid": "s1", "kind": "return"}]}
        ]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E411"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn join_all_outside_scope_is_e412() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "kind": "join_all"},
                {"sid": "s2", "kind": "return"}
            ]}
        ]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E412"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn join_all_inside_scope_is_valid() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "scope", "body": [
                {"sid": "s1", "kind": "spawn", "func": "worker", "handle": "h_w"},
                {"sid": "s2", "kind": "join_all"},
                {"sid": "s3", "kind": "return"}
            ]},
            {"name": "worker", "kind": "normal",
             "body": [{"sid": "s1", "kind": "return"}]}
        ]"#,
    );
    assert!(
        report.valid,
        "join_all is the mid-scope barrier. got: {:?}",
        report.diagnostics
    );
}

#[test]
fn kind_closure_is_e010() {
    let report = wrap(
        "[]",
        r#"[{"name": "main", "kind": "closure",
            "body": [{"sid": "s1", "kind": "return"}]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E010"),
        "got: {:?}",
        report.diagnostics
    );
}
