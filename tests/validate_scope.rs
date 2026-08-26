//! Scope statement / function kind / spawn pairing (E010, E401, E410).

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
fn scope_statement_joins_implicitly() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "kind": "scope", "func": "worker", "count": 4},
                {"sid": "s2", "kind": "return"}
            ]},
            {"name": "worker", "kind": "normal", "form": "closure",
             "body": [{"sid": "s1", "kind": "return"}]}
        ]"#,
    );
    assert!(
        report.valid,
        "scope statement is spawn N + implicit join_all. got: {:?}",
        report.diagnostics
    );
    assert!(
        !codes(&report).contains(&"E401"),
        "scope does not leave unpaired handles. got: {:?}",
        report.diagnostics
    );
}

#[test]
fn spawn_without_join_is_e401_warning() {
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
fn scope_count_zero_is_e410() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "kind": "scope", "func": "worker", "count": 0},
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
fn function_kind_scope_is_e010() {
    let report = wrap(
        "[]",
        r#"[{"name": "main", "kind": "scope",
            "body": [{"sid": "s1", "kind": "return"}]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E010"),
        "got: {:?}",
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

#[test]
fn spawn_batch_is_unknown_kind() {
    let json = r#"{
        "program": "p",
        "modules": [{
            "name": "main",
            "functions": [{
                "name": "main", "kind": "normal",
                "body": [{"sid": "s1", "kind": "spawn_batch", "func": "w"}]
            }]
        }],
        "entry": "main::main"
    }"#;
    let result: Result<Program, _> = serde_json::from_str(json);
    assert!(result.is_err(), "spawn_batch is not a statement kind");
}

#[test]
fn join_all_is_unknown_kind() {
    let json = r#"{
        "program": "p",
        "modules": [{
            "name": "main",
            "functions": [{
                "name": "main", "kind": "normal",
                "body": [{"sid": "s1", "kind": "join_all"}]
            }]
        }],
        "entry": "main::main"
    }"#;
    let result: Result<Program, _> = serde_json::from_str(json);
    assert!(result.is_err(), "join_all is implicit on scope, not a kind");
}
