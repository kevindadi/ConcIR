//! Module + FQN + statement-level CFG grammar.

use concir::ast::{Function, Module, Op, Program, Stmt};
use concir::fqn;

fn sample_module_json() -> &'static str {
    r#"{
        "name": "producer",
        "provides": { "resources": ["mtx"], "functions": ["producer"] },
        "requires": { "resources": [], "functions": [] },
        "resources": [
            {"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}
        ],
        "protection": [],
        "functions": [
            {
                "name": "producer",
                "kind": "normal",
                "form": "closure",
                "body": [
                    {"sid": "s1", "kind": "mutex_lock", "resource": "mtx"},
                    {"sid": "s2", "kind": "mutex_unlock", "resource": "mtx"},
                    {"sid": "s3", "kind": "return"}
                ]
            }
        ]
    }"#
}

#[test]
fn module_deserializes() {
    let module: Module = serde_json::from_str(sample_module_json()).unwrap();
    assert_eq!(module.name, "producer");
    assert_eq!(module.provides.functions, vec!["producer"]);
    assert_eq!(module.functions[0].body.len(), 3);
    assert!(matches!(
        module.functions[0].body[2].op,
        Op::Return { value: None }
    ));
}

#[test]
fn program_uses_modules_and_fqn_entry() {
    let json = format!(
        r#"{{
            "program": "assembled",
            "modules": [{}],
            "entry": "producer::producer"
        }}"#,
        sample_module_json()
    );
    let program: Program = serde_json::from_str(&json).unwrap();
    assert_eq!(program.entry, "producer::producer");
    assert_eq!(
        fqn::split_fqn(&program.entry),
        Some(("producer", "producer"))
    );
    assert!(program.lookup_function("producer", "producer").is_some());
}

#[test]
fn stmt_rejects_legacy_call_field() {
    let json = r#"{
        "sid": "s1",
        "call": {"kind": "mutex_lock", "resource": "m", "target": "s2"},
        "kind": "return"
    }"#;
    let result: Result<Stmt, _> = serde_json::from_str(json);
    assert!(result.is_err());
}

#[test]
fn stmt_requires_kind() {
    let json = r#"{"sid": "s1"}"#;
    let result: Result<Stmt, _> = serde_json::from_str(json);
    assert!(result.is_err());
}

#[test]
fn stmt_rejects_legacy_block_shape() {
    let json = r#"{"sid": "s1", "statements": [{"kind": "nop"}]}"#;
    let result: Result<Stmt, _> = serde_json::from_str(json);
    assert!(result.is_err());
}

#[test]
fn return_is_a_statement() {
    let f: Function = serde_json::from_str(
        r#"{
            "name": "f",
            "kind": "normal",
            "body": [{"sid": "s1", "kind": "return"}]
        }"#,
    )
    .unwrap();
    assert!(f.body[0].is_return());
}

#[test]
fn fqn_helpers() {
    assert_eq!(fqn::fqn("core", "main"), "core::main");
    assert_eq!(fqn::split_fqn("core::main"), Some(("core", "main")));
    assert_eq!(
        fqn::split_location("core::main.s3"),
        Some(("core", "main", "s3"))
    );
    assert!(!fqn::is_fqn("crate::main::entry"));
}
