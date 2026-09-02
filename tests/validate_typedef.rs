//! Module-level named types (E110–E113).

use concir::ast::Program;
use concir::validate::validate;

fn codes(report: &concir::diagnostic::ValidationReport) -> Vec<&str> {
    report.diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn named_struct_type_on_var_is_valid() {
    let json = r#"{
        "program": "p",
        "modules": [{
            "name": "main",
            "provides": { "types": ["Record"], "functions": ["main"] },
            "types": [
                { "name": "Record", "type": { "Struct": { "size": "Int", "ready": "Bool" } } }
            ],
            "resources": [
                { "name": "shared", "kind": "var", "type": "Var", "base": "Record",
                  "init": { "size": 0, "ready": false } },
                { "name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync" }
            ],
            "protection": [{ "var": "shared", "lock": "mtx" }],
            "functions": [{
                "name": "main",
                "kind": "normal",
                "body": [
                    { "sid": "s1", "kind": "mutex_lock", "resource": "mtx" },
                    { "sid": "s2", "kind": "branch", "cond": "shared.ready == false",
                      "then": "s3", "else": "s3" },
                    { "sid": "s3", "kind": "mutex_unlock", "resource": "mtx" },
                    { "sid": "s4", "kind": "return" }
                ]
            }]
        }],
        "entry": "main::main"
    }"#;
    let program: Program = serde_json::from_str(json).unwrap();
    let report = validate(&program);
    assert!(report.valid, "got: {:?}", report.diagnostics);
}

#[test]
fn cross_module_type_import_is_valid() {
    let json = r#"{
        "program": "p",
        "modules": [
            {
                "name": "storage",
                "provides": { "types": ["Record"] },
                "types": [
                    { "name": "Record", "type": { "Enum": ["A", "B"] } }
                ]
            },
            {
                "name": "core",
                "provides": { "functions": ["main"] },
                "requires": { "types": ["storage::Record"] },
                "resources": [
                    { "name": "flag", "kind": "var", "type": "Atomic",
                      "base": "storage::Record", "init": "A" }
                ],
                "functions": [{
                    "name": "main",
                    "kind": "normal",
                    "body": [
                        { "sid": "s1", "kind": "switch", "var": "flag",
                          "cases": { "A": "s2", "B": "s2" }, "default": "s2" },
                        { "sid": "s2", "kind": "return" }
                    ]
                }]
            }
        ],
        "entry": "core::main"
    }"#;
    let program: Program = serde_json::from_str(json).unwrap();
    let report = validate(&program);
    assert!(report.valid, "got: {:?}", report.diagnostics);
}

#[test]
fn duplicate_type_is_e110() {
    let json = r#"{
        "program": "p",
        "modules": [{
            "name": "main",
            "provides": { "functions": ["main"] },
            "types": [
                { "name": "T", "type": "Int" },
                { "name": "T", "type": "Bool" }
            ],
            "functions": [{ "name": "main", "kind": "normal",
                "body": [{ "sid": "s1", "kind": "return" }] }]
        }],
        "entry": "main::main"
    }"#;
    let program: Program = serde_json::from_str(json).unwrap();
    assert!(codes(&validate(&program)).contains(&"E110"));
}

#[test]
fn undefined_type_as_base_is_e111() {
    let json = r#"{
        "program": "p",
        "modules": [{
            "name": "main",
            "provides": { "functions": ["main"] },
            "resources": [
                { "name": "x", "kind": "var", "type": "Var", "base": "Missing", "init": 0 }
            ],
            "functions": [{ "name": "main", "kind": "normal",
                "body": [{ "sid": "s1", "kind": "return" }] }]
        }],
        "entry": "main::main"
    }"#;
    let program: Program = serde_json::from_str(json).unwrap();
    assert!(codes(&validate(&program)).contains(&"E111"));
}

#[test]
fn builtin_type_name_is_e112() {
    let json = r#"{
        "program": "p",
        "modules": [{
            "name": "main",
            "provides": { "functions": ["main"] },
            "types": [{ "name": "Int", "type": "Bool" }],
            "functions": [{ "name": "main", "kind": "normal",
                "body": [{ "sid": "s1", "kind": "return" }] }]
        }],
        "entry": "main::main"
    }"#;
    let program: Program = serde_json::from_str(json).unwrap();
    assert!(codes(&validate(&program)).contains(&"E112"));
}

#[test]
fn cyclic_alias_is_e113() {
    let json = r#"{
        "program": "p",
        "modules": [{
            "name": "main",
            "provides": { "functions": ["main"] },
            "types": [
                { "name": "A", "type": "B" },
                { "name": "B", "type": "A" }
            ],
            "functions": [{ "name": "main", "kind": "normal",
                "body": [{ "sid": "s1", "kind": "return" }] }]
        }],
        "entry": "main::main"
    }"#;
    let program: Program = serde_json::from_str(json).unwrap();
    assert!(codes(&validate(&program)).contains(&"E113"));
}

#[test]
fn type_not_exported_is_e108() {
    let json = r#"{
        "program": "p",
        "modules": [
            {
                "name": "storage",
                "provides": { "types": [] },
                "types": [{ "name": "Record", "type": "Int" }]
            },
            {
                "name": "core",
                "provides": { "functions": ["main"] },
                "requires": { "types": ["storage::Record"] },
                "functions": [{ "name": "main", "kind": "normal",
                    "body": [{ "sid": "s1", "kind": "return" }] }]
            }
        ],
        "entry": "core::main"
    }"#;
    let program: Program = serde_json::from_str(json).unwrap();
    assert!(codes(&validate(&program)).contains(&"E108"));
}
