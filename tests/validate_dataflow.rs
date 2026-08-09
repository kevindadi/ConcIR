//! E9xx: typed data-flow checks (params, returns, call sites).

use concir::ast::Program;
use concir::validate::validate;

fn validate_json(json: &str) -> concir::diagnostic::ValidationReport {
    let program: Program = serde_json::from_str(json).expect("test CIR must parse");
    validate(&program)
}

fn codes(report: &concir::diagnostic::ValidationReport) -> Vec<&str> {
    report.diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn modeled_param_referenced_in_guard_is_valid() {
    let report = validate_json(
        r#"{
            "program": "p",
            "resources": [{"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}],
            "protection": [],
            "functions": [
                {
                    "name": "main", "kind": "normal",
                    "body": [
                        {"sid": "s1", "op": ["call", "worker", "n"], "transfer": ["next", "s2"]},
                        {"sid": "s2", "op": "return", "transfer": "return"}
                    ]
                },
                {
                    "name": "worker", "kind": "normal",
                    "params": [{"name": "n", "type": "Int", "modeled": true}],
                    "body": [
                        {"sid": "s1", "op": ["res_op", "mtx", "lock"], "transfer": ["next", "s2"]},
                        {"sid": "s2", "op": ["res_op", "mtx", "drop"], "transfer": "return"}
                    ]
                }
            ],
            "entry": "main"
        }"#,
    );
    assert!(report.valid, "got: {:?}", report.diagnostics);
}

#[test]
fn unmodeled_param_referenced_in_body_is_e912() {
    let report = validate_json(
        r#"{
            "program": "p",
            "resources": [],
            "protection": [],
            "functions": [
                {
                    "name": "main", "kind": "normal",
                    "body": [
                        {"sid": "s1", "op": ["call", "worker", "n"], "transfer": ["next", "s2"]},
                        {"sid": "s2", "op": "return", "transfer": "return"}
                    ]
                },
                {
                    "name": "worker", "kind": "normal",
                    "params": [{"name": "n", "type": "Int", "modeled": false}],
                    "body": [
                        {"sid": "s1", "op": "nop", "transfer": ["branch", "n > 3", "s2", "s3"]},
                        {"sid": "s2", "op": "return", "transfer": "return"},
                        {"sid": "s3", "op": "return", "transfer": "return"}
                    ]
                }
            ],
            "entry": "main"
        }"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E912"),
        "expected E912, got: {:?}",
        report.diagnostics
    );
}

#[test]
fn call_argument_arity_mismatch_is_e920() {
    let report = validate_json(
        r#"{
            "program": "p",
            "resources": [],
            "protection": [],
            "functions": [
                {
                    "name": "main", "kind": "normal",
                    "body": [
                        {"sid": "s1", "op": ["call", "worker", "a", "b"], "transfer": ["next", "s2"]},
                        {"sid": "s2", "op": "return", "transfer": "return"}
                    ]
                },
                {
                    "name": "worker", "kind": "normal",
                    "params": [{"name": "n", "type": "Int", "modeled": true}],
                    "body": [
                        {"sid": "s1", "op": "return", "transfer": "return"}
                    ]
                }
            ],
            "entry": "main"
        }"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E920"),
        "expected E920, got: {:?}",
        report.diagnostics
    );
}

#[test]
fn call_capture_into_non_var_resource_is_e921() {
    let report = validate_json(
        r#"{
            "program": "p",
            "resources": [
                {"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}
            ],
            "protection": [],
            "functions": [
                {
                    "name": "main", "kind": "normal",
                    "params": [{"name": "x", "type": "Int", "modeled": true}],
                    "body": [
                        {"sid": "s1", "op": ["call", "worker", "mtx", "x"], "transfer": ["next", "s2"]},
                        {"sid": "s2", "op": "return", "transfer": "return"}
                    ]
                },
                {
                    "name": "worker", "kind": "normal",
                    "params": [{"name": "n", "type": "Int", "modeled": true}],
                    "returns": {"name": "out", "type": "Int", "modeled": true},
                    "body": [
                        {"sid": "s1", "op": ["return", "n + 1"], "transfer": "return"}
                    ]
                }
            ],
            "entry": "main"
        }"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E921"),
        "expected E921, got: {:?}",
        report.diagnostics
    );
}

#[test]
fn modeled_return_with_bare_return_is_e913_warning() {
    let report = validate_json(
        r#"{
            "program": "p",
            "resources": [],
            "protection": [],
            "functions": [
                {
                    "name": "main", "kind": "normal",
                    "body": [
                        {"sid": "s1", "op": ["call", "worker", ""], "transfer": ["next", "s2"]},
                        {"sid": "s2", "op": "return", "transfer": "return"}
                    ]
                },
                {
                    "name": "worker", "kind": "normal",
                    "returns": {"name": "out", "type": "Int", "modeled": true},
                    "body": [
                        {"sid": "s1", "op": "return", "transfer": "return"}
                    ]
                }
            ],
            "entry": "main"
        }"#,
    );
    assert!(report.valid, "warning only, got: {:?}", report.diagnostics);
    assert!(
        codes(&report).contains(&"E913"),
        "expected E913 warning, got: {:?}",
        report.diagnostics
    );
}

#[test]
fn param_colliding_with_resource_is_e910() {
    let report = validate_json(
        r#"{
            "program": "p",
            "resources": [{"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}],
            "protection": [],
            "functions": [
                {
                    "name": "main", "kind": "normal",
                    "params": [{"name": "mtx", "type": "Int", "modeled": true}],
                    "body": [
                        {"sid": "s1", "op": "return", "transfer": "return"}
                    ]
                }
            ],
            "entry": "main"
        }"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E910"),
        "expected E910, got: {:?}",
        report.diagnostics
    );
}

#[test]
fn bounded_int_resource_is_valid() {
    let report = validate_json(
        r#"{
            "program": "p",
            "resources": [{"name": "count", "kind": "var", "type": "Var", "base": {"Int": [0, 10]}, "init": 3}],
            "protection": [],
            "functions": [
                {
                    "name": "main", "kind": "normal",
                    "body": [
                        {"sid": "s1", "op": ["res_op", "count", "write", "5"], "transfer": ["next", "s2"]},
                        {"sid": "s2", "op": "return", "transfer": "return"}
                    ]
                }
            ],
            "entry": "main"
        }"#,
    );
    assert!(report.valid, "got: {:?}", report.diagnostics);
}

#[test]
fn bounded_int_init_out_of_range_is_e208() {
    let report = validate_json(
        r#"{
            "program": "p",
            "resources": [{"name": "count", "kind": "var", "type": "Var", "base": {"Int": [0, 10]}, "init": 42}],
            "protection": [],
            "functions": [
                {
                    "name": "main", "kind": "normal",
                    "body": [
                        {"sid": "s1", "op": "return", "transfer": "return"}
                    ]
                }
            ],
            "entry": "main"
        }"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E208"),
        "expected E208, got: {:?}",
        report.diagnostics
    );
}

#[test]
fn bounded_int_write_literal_out_of_range_is_e203() {
    let report = validate_json(
        r#"{
            "program": "p",
            "resources": [{"name": "count", "kind": "var", "type": "Var", "base": {"Int": [0, 10]}, "init": 0}],
            "protection": [],
            "functions": [
                {
                    "name": "main", "kind": "normal",
                    "body": [
                        {"sid": "s1", "op": ["res_op", "count", "write", "11"], "transfer": ["next", "s2"]},
                        {"sid": "s2", "op": "return", "transfer": "return"}
                    ]
                }
            ],
            "entry": "main"
        }"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E203"),
        "expected E203, got: {:?}",
        report.diagnostics
    );
}

#[test]
fn bounded_int_lo_gt_hi_is_a_parse_error() {
    let json = r#"{
        "program": "p",
        "resources": [{"name": "count", "kind": "var", "type": "Var", "base": {"Int": [10, 0]}, "init": 0}],
        "protection": [],
        "functions": [],
        "entry": "main"
    }"#;
    let result: Result<Program, _> = serde_json::from_str(json);
    assert!(result.is_err(), "lo > hi must be rejected at parse");
}
