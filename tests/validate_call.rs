//! `call` targets are resolved by the translator after merge: a call expands
//! into the callee's entry/return skeleton, so bodied callees (including those
//! with synchronization operations) are legitimate. No E409/E410 diagnostics
//! are emitted; only the standard name-resolution checks (E102) apply.

use concir::ast::Program;
use concir::validate::validate;

fn program_with_callee_body(callee_body: &str) -> Program {
    let json = format!(
        r#"{{
        "program": "call_check",
        "resources": [
            {{"name": "m1", "kind": "sync", "type": "Mutex", "mode": "Sync"}}
        ],
        "protection": [],
        "functions": [
            {{
                "name": "main", "kind": "normal",
                "body": [
                    {{"sid": "s1", "op": ["call", "helper"], "transfer": ["next", "s2"]}},
                    {{"sid": "s2", "op": "return", "transfer": "return"}}
                ]
            }},
            {{
                "name": "helper", "kind": "normal",
                "body": [{callee_body}]
            }}
        ],
        "entry": "main"
    }}"#
    );
    serde_json::from_str(&json).expect("test ConcIR must parse")
}

#[test]
fn call_to_sync_bodied_function_is_valid() {
    let program = program_with_callee_body(
        r#"{"sid": "s1", "op": ["res_op", "m1", "lock"], "transfer": ["next", "s2"]},
           {"sid": "s2", "op": ["res_op", "m1", "drop"], "transfer": ["next", "s3"]},
           {"sid": "s3", "op": "return", "transfer": "return"}"#,
    );

    let report = validate(&program);

    assert!(
        report.valid,
        "call to a bodied sync function must stay valid, got: {:?}",
        report.diagnostics
    );
    assert!(
        !report.diagnostics.iter().any(|d| d.code == "E409" || d.code == "E410"),
        "E409/E410 are removed; call targets are resolved after merge"
    );
}

#[test]
fn call_to_bodyless_function_is_valid() {
    let json = r#"{
        "program": "call_check",
        "resources": [],
        "protection": [],
        "functions": [
            {
                "name": "main", "kind": "normal",
                "body": [
                    {"sid": "s1", "op": ["call", "compute"], "transfer": ["next", "s2"]},
                    {"sid": "s2", "op": "return", "transfer": "return"}
                ]
            },
            {
                "name": "compute", "kind": "normal",
                "body": [],
                "effects": {"reads": [], "writes": ["result"]}
            }
        ],
        "entry": "main"
    }"#;
    let program: Program = serde_json::from_str(json).expect("test ConcIR must parse");

    let report = validate(&program);

    assert!(
        report.valid,
        "calling a body-less function with effects is valid, got: {:?}",
        report.diagnostics
    );
}

#[test]
fn call_to_undefined_function_is_an_error() {
    let json = r#"{
        "program": "call_check",
        "resources": [],
        "protection": [],
        "functions": [
            {
                "name": "main", "kind": "normal",
                "body": [
                    {"sid": "s1", "op": ["call", "missing"], "transfer": ["next", "s2"]},
                    {"sid": "s2", "op": "return", "transfer": "return"}
                ]
            }
        ],
        "entry": "main"
    }"#;
    let program: Program = serde_json::from_str(json).expect("test ConcIR must parse");

    let report = validate(&program);

    assert!(!report.valid);
    assert!(
        report.diagnostics.iter().any(|d| d.code == "E102"),
        "undefined call target must be E102, got: {:?}",
        report.diagnostics.iter().map(|d| &d.code).collect::<Vec<_>>()
    );
}
