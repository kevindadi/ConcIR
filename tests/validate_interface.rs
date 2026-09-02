//! E8xx: function concurrency interface and imported signatures.

use concir::ast::{FunctionSig, Program, RequiredFunction};
use concir::validate::validate;

fn codes(report: &concir::diagnostic::ValidationReport) -> Vec<&str> {
    report.diagnostics.iter().map(|d| d.code).collect()
}

fn two_module_program(storage_fn: &str, core_requires_fn: &str, core_body: &str) -> Program {
    let json = format!(
        r#"{{
            "program": "iface",
            "modules": [
                {{
                    "name": "storage",
                    "provides": {{ "resources": ["log_mtx"], "functions": ["flush"] }},
                    "requires": {{ "resources": [], "functions": [] }},
                    "resources": [
                        {{ "name": "log_mtx", "kind": "sync", "type": "Mutex", "mode": "Sync" }}
                    ],
                    "functions": [ {storage_fn} ]
                }},
                {{
                    "name": "core",
                    "provides": {{ "resources": [], "functions": ["main"] }},
                    "requires": {{
                        "resources": ["storage::log_mtx"],
                        "functions": [ {core_requires_fn} ]
                    }},
                    "functions": [
                        {{
                            "name": "main",
                            "kind": "normal",
                            "body": {core_body}
                        }}
                    ]
                }}
            ],
            "entry": "core::main"
        }}"#
    );
    serde_json::from_str(&json).expect("test CIR must parse")
}

#[test]
fn name_only_requires_still_parses() {
    let program = two_module_program(
        r#"{
            "name": "flush",
            "kind": "normal",
            "body": [
                {"sid": "s1", "kind": "mutex_lock", "resource": "log_mtx"},
                {"sid": "s2", "kind": "mutex_unlock", "resource": "log_mtx"},
                {"sid": "s3", "kind": "return"}
            ]
        }"#,
        r#""storage::flush""#,
        r#"[
            {"sid": "s1", "kind": "call", "func": "storage::flush"},
            {"sid": "s2", "kind": "return"}
        ]"#,
    );
    assert!(matches!(
        program.modules[1].requires.functions[0],
        RequiredFunction::Name(_)
    ));
    let report = validate(&program);
    assert!(report.valid, "got: {:?}", report.diagnostics);
}

#[test]
fn imported_sig_matching_definition_is_valid() {
    let program = two_module_program(
        r#"{
            "name": "flush",
            "kind": "normal",
            "may_block": false,
            "locks": { "requires_held": ["log_mtx"] },
            "body": [
                {"sid": "s1", "kind": "return"}
            ]
        }"#,
        r#"{
            "name": "storage::flush",
            "kind": "normal",
            "may_block": false,
            "locks": { "requires_held": ["log_mtx"] }
        }"#,
        r#"[
            {"sid": "s1", "kind": "mutex_lock", "resource": "storage::log_mtx"},
            {"sid": "s2", "kind": "call", "func": "storage::flush"},
            {"sid": "s3", "kind": "mutex_unlock", "resource": "storage::log_mtx"},
            {"sid": "s4", "kind": "return"}
        ]"#,
    );
    assert!(matches!(
        program.modules[1].requires.functions[0],
        RequiredFunction::Sig(FunctionSig { .. })
    ));
    let report = validate(&program);
    assert!(report.valid, "got: {:?}", report.diagnostics);
}

#[test]
fn lock_effect_on_non_lock_is_e801() {
    let json = r#"{
        "program": "p",
        "modules": [{
            "name": "main",
            "provides": { "resources": ["count"], "functions": ["main"] },
            "resources": [
                { "name": "count", "kind": "var", "type": "Var", "base": "Int", "init": 0 }
            ],
            "functions": [{
                "name": "main",
                "kind": "normal",
                "locks": { "acquires": ["count"] },
                "body": [{"sid": "s1", "kind": "return"}]
            }]
        }],
        "entry": "main::main"
    }"#;
    let program: Program = serde_json::from_str(json).unwrap();
    let report = validate(&program);
    assert!(codes(&report).contains(&"E801"));
}

#[test]
fn may_block_false_with_blocking_body_is_e802() {
    let json = r#"{
        "program": "p",
        "modules": [{
            "name": "main",
            "provides": { "resources": ["sem"], "functions": ["main"] },
            "resources": [
                { "name": "sem", "kind": "sync", "type": "Semaphore", "mode": "Sync", "count": 1 }
            ],
            "functions": [{
                "name": "main",
                "kind": "normal",
                "may_block": false,
                "body": [
                    {"sid": "s1", "kind": "semaphore_acquire", "resource": "sem"},
                    {"sid": "s2", "kind": "return"}
                ]
            }]
        }],
        "entry": "main::main"
    }"#;
    let program: Program = serde_json::from_str(json).unwrap();
    let report = validate(&program);
    assert!(codes(&report).contains(&"E802"));
    assert!(!report.valid);
}

#[test]
fn call_without_required_lock_is_e803() {
    let program = two_module_program(
        r#"{
            "name": "flush",
            "kind": "normal",
            "locks": { "requires_held": ["log_mtx"] },
            "body": [{"sid": "s1", "kind": "return"}]
        }"#,
        r#""storage::flush""#,
        r#"[
            {"sid": "s1", "kind": "call", "func": "storage::flush"},
            {"sid": "s2", "kind": "return"}
        ]"#,
    );
    let report = validate(&program);
    assert!(
        codes(&report).contains(&"E803"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn imported_sig_kind_mismatch_is_e804() {
    let program = two_module_program(
        r#"{
            "name": "flush",
            "kind": "normal",
            "body": [{"sid": "s1", "kind": "return"}]
        }"#,
        r#"{ "name": "storage::flush", "kind": "async" }"#,
        r#"[
            {"sid": "s1", "kind": "call", "func": "storage::flush"},
            {"sid": "s2", "kind": "return"}
        ]"#,
    );
    let report = validate(&program);
    assert!(
        codes(&report).contains(&"E804"),
        "got: {:?}",
        report.diagnostics
    );
}
