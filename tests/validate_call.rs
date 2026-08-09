//! E409/E410: `call` targets with a body are modeled atomically, so bodies
//! containing synchronization must be rejected (and pure bodies flagged).

use cir::ast::Program;
use cir::validate::validate;

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
        "fn_summaries": [],
        "entry": "main"
    }}"#
    );
    serde_json::from_str(&json).expect("test CIR must parse")
}

#[test]
fn e409_call_to_sync_bodied_function_is_an_error() {
    let program = program_with_callee_body(
        r#"{"sid": "s1", "op": ["res_op", "m1", "lock"], "transfer": ["next", "s2"]},
           {"sid": "s2", "op": ["res_op", "m1", "drop"], "transfer": ["next", "s3"]},
           {"sid": "s3", "op": "return", "transfer": "return"}"#,
    );

    let report = validate(&program);

    assert!(!report.valid, "sync-bodied call target must invalidate the model");
    assert!(
        report.diagnostics.iter().any(|d| d.code == "E409"),
        "expected E409, got: {:?}",
        report.diagnostics.iter().map(|d| &d.code).collect::<Vec<_>>()
    );
}

#[test]
fn e410_call_to_pure_bodied_function_is_a_warning_only() {
    let program = program_with_callee_body(
        r#"{"sid": "s1", "op": "nop", "transfer": ["next", "s2"]},
           {"sid": "s2", "op": "return", "transfer": "return"}"#,
    );

    let report = validate(&program);

    assert!(report.valid, "pure-computation callee stays valid");
    assert!(
        report.diagnostics.iter().any(|d| d.code == "E410"),
        "expected E410 warning, got: {:?}",
        report.diagnostics.iter().map(|d| &d.code).collect::<Vec<_>>()
    );
}

#[test]
fn summary_only_call_target_produces_no_e409_or_e410() {
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
            }
        ],
        "fn_summaries": [
            {"name": "compute", "reads": [], "writes": [], "callees": [], "has_concurrency": false}
        ],
        "entry": "main"
    }"#;
    let program: Program = serde_json::from_str(json).expect("test CIR must parse");

    let report = validate(&program);

    assert!(
        !report
            .diagnostics
            .iter()
            .any(|d| d.code == "E409" || d.code == "E410"),
        "summary-only calls are the supported form, got: {:?}",
        report.diagnostics
    );
}
