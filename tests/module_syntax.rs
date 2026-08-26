//! Module + FQN + Block/Call/Terminator grammar.

use concir::ast::{Block, Function, Module, Program, Terminator};
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
                "kind": "closure",
                "body": [
                    {"sid": "s1", "call": {"kind": "mutex_lock", "resource": "mtx", "target": "s2"}},
                    {"sid": "s2", "call": {"kind": "mutex_unlock", "resource": "mtx", "target": "s3"}},
                    {"sid": "s3", "terminator": {"kind": "return"}}
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
        module.functions[0].body[2].terminator,
        Some(Terminator::Return { value: None })
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
fn block_rejects_both_call_and_terminator() {
    let json = r#"{
        "sid": "s1",
        "call": {"kind": "mutex_lock", "resource": "m", "target": "s2"},
        "terminator": {"kind": "return"}
    }"#;
    let result: Result<Block, _> = serde_json::from_str(json);
    assert!(result.is_err());
}

#[test]
fn return_lives_only_on_terminator() {
    let f: Function = serde_json::from_str(
        r#"{
            "name": "f",
            "kind": "normal",
            "body": [{"sid": "s1", "terminator": {"kind": "return"}}]
        }"#,
    )
    .unwrap();
    assert!(f.body[0].is_return());
    assert!(f.body[0].call.is_none());
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
