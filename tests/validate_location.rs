//! Diagnostic `location` is `module::function.sid`.

use concir::ast::Program;
use concir::validate::validate;

#[test]
fn statement_error_carries_control_location() {
    let json = r#"{
        "program": "p",
        "modules": [{
            "name": "core",
            "provides": { "functions": ["main"] },
            "resources": [
                { "name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync" }
            ],
            "functions": [{
                "name": "worker",
                "kind": "normal",
                "body": [
                    { "sid": "s1", "kind": "mutex_lock", "resource": "mtx" },
                    { "sid": "s2", "kind": "return" }
                ]
            }]
        }],
        "entry": "core::worker"
    }"#;
    let program: Program = serde_json::from_str(json).unwrap();
    let report = validate(&program);
    let e501 = report
        .diagnostics
        .iter()
        .find(|d| d.code == "E501")
        .expect("E501");
    assert_eq!(e501.location.as_deref(), Some("core::worker.s2"));
}

#[test]
fn function_level_diagnostic_uses_fn_location() {
    let json = r#"{
        "program": "p",
        "modules": [{
            "name": "core",
            "provides": { "functions": ["main"] },
            "resources": [
                { "name": "sem", "kind": "sync", "type": "Semaphore", "mode": "Sync", "count": 1 }
            ],
            "functions": [{
                "name": "main",
                "kind": "normal",
                "may_block": false,
                "body": [
                    { "sid": "s1", "kind": "semaphore_acquire", "resource": "sem" },
                    { "sid": "s2", "kind": "return" }
                ]
            }]
        }],
        "entry": "core::main"
    }"#;
    let program: Program = serde_json::from_str(json).unwrap();
    let report = validate(&program);
    let e802 = report
        .diagnostics
        .iter()
        .find(|d| d.code == "E802")
        .expect("E802");
    assert_eq!(e802.location.as_deref(), Some("core::main"));
}
