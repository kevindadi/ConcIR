//! `Module` is a first-class grammar declaration: same payload as `Program`,
//! identified by `name`. Concatenation into a `Program` is not covered here.

use concir::ast::{Function, Module, Program};

fn sample_module_json() -> &'static str {
    r#"{
        "name": "producer",
        "provides": ["producer"],
        "requires": [],
        "resources": [
            {"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}
        ],
        "protection": [],
        "functions": [
            {
                "name": "producer",
                "kind": "closure",
                "body": [
                    {"sid": "s1", "op": ["res_op", "mtx", "lock"], "transfer": ["next", "s2"]},
                    {"sid": "s2", "op": ["res_op", "mtx", "drop"], "transfer": ["next", "s3"]},
                    {"sid": "s3", "op": "return", "transfer": "return"}
                ]
            }
        ]
    }"#
}

#[test]
fn module_deserializes_with_program_payload() {
    let module: Module = serde_json::from_str(sample_module_json()).expect("module must parse");

    assert_eq!(module.name, "producer");
    assert_eq!(module.provides, vec!["producer"]);
    assert!(module.requires.is_empty());
    assert_eq!(module.resources.len(), 1);
    assert_eq!(module.resources[0].name, "mtx");
    assert!(module.protection.is_empty());
    assert_eq!(module.functions.len(), 1);
    assert_eq!(module.functions[0].name, "producer");
    assert!(module.entry.is_none());
    assert!(module.goals.is_empty());
}

#[test]
fn module_defaults_omitted_payload_fields() {
    let module: Module = serde_json::from_str(r#"{"name": "main"}"#).expect("minimal module");

    assert_eq!(module.name, "main");
    assert!(module.provides.is_empty());
    assert!(module.requires.is_empty());
    assert!(module.resources.is_empty());
    assert!(module.protection.is_empty());
    assert!(module.functions.is_empty());
    assert!(module.entry.is_none());
    assert!(module.goals.is_empty());
}

#[test]
fn module_roundtrips_through_json() {
    let original: Module = serde_json::from_str(sample_module_json()).unwrap();
    let encoded = serde_json::to_string(&original).unwrap();
    let again: Module = serde_json::from_str(&encoded).unwrap();

    assert_eq!(again.name, original.name);
    assert_eq!(again.provides, original.provides);
    assert_eq!(again.functions[0].name, original.functions[0].name);
}

#[test]
fn module_rejects_unknown_fields() {
    let result: Result<Module, _> = serde_json::from_str(r#"{"name": "x", "extra": 1}"#);
    assert!(result.is_err(), "deny_unknown_fields must reject extra keys");
}

#[test]
fn program_and_module_share_function_shape() {
    let module: Module = serde_json::from_str(sample_module_json()).unwrap();
    let function: &Function = &module.functions[0];

    let program: Program = serde_json::from_str(&format!(
        r#"{{
            "program": "assembled",
            "resources": [],
            "protection": [],
            "functions": [{}],
            "entry": "producer"
        }}"#,
        serde_json::to_string(function).unwrap()
    ))
    .expect("function JSON from a module must be valid inside a Program");

    assert_eq!(program.functions[0].name, "producer");
    assert!(program.functions[0].module.is_none());
}
