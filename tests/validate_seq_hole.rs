//! seq_hole: sequential fill site, distinct from abstract_step.

use concir::ast::{Op, Program};
use concir::validate::validate;

fn wrap(resources: &str, protection: &str, body: &str) -> concir::diagnostic::ValidationReport {
    let json = format!(
        r#"{{
            "program": "p",
            "modules": [{{
                "name": "main",
                "provides": {{"resources": [], "functions": ["main"]}},
                "resources": {resources},
                "protection": {protection},
                "functions": [{{
                    "name": "main",
                    "kind": "normal",
                    "body": {body}
                }}]
            }}],
            "entry": "main::main"
        }}"#
    );
    let program: Program = serde_json::from_str(&json).expect("parse");
    validate(&program)
}

fn codes(report: &concir::diagnostic::ValidationReport) -> Vec<&str> {
    report.diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn seq_hole_deserializes_and_is_valid() {
    let json = r#"{
        "sid": "s1",
        "kind": "seq_hole",
        "id": "validate_payload",
        "desc": "checksum then store",
        "reads": ["buf"],
        "writes": []
    }"#;
    let stmt: concir::ast::Stmt = serde_json::from_str(json).unwrap();
    assert!(matches!(stmt.op, Op::SeqHole { .. }));
}

#[test]
fn seq_hole_on_var_with_lock_is_valid() {
    let report = wrap(
        r#"[
            {"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"},
            {"name": "buf", "kind": "var", "type": "Var", "base": "Int", "init": 0}
        ]"#,
        r#"[{"var": "buf", "lock": "mtx"}]"#,
        r#"[
            {"sid": "s1", "kind": "mutex_lock", "resource": "mtx"},
            {"sid": "s2", "kind": "seq_hole", "id": "validate_payload", "reads": ["buf"]},
            {"sid": "s3", "kind": "mutex_unlock", "resource": "mtx"},
            {"sid": "s4", "kind": "return"}
        ]"#,
    );
    assert!(report.valid, "got: {:?}", report.diagnostics);
}

#[test]
fn seq_hole_id_not_ident_is_e006() {
    let report = wrap(
        "[]",
        "[]",
        r#"[
            {"sid": "s1", "kind": "seq_hole", "id": "1bad"},
            {"sid": "s2", "kind": "return"}
        ]"#,
    );
    assert!(codes(&report).contains(&"E006"));
}

#[test]
fn duplicate_seq_hole_id_is_e109() {
    let report = wrap(
        "[]",
        "[]",
        r#"[
            {"sid": "s1", "kind": "seq_hole", "id": "h"},
            {"sid": "s2", "kind": "seq_hole", "id": "h"},
            {"sid": "s3", "kind": "return"}
        ]"#,
    );
    assert!(codes(&report).contains(&"E109"));
}

#[test]
fn seq_hole_naming_mutex_is_e310() {
    let report = wrap(
        r#"[{"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}]"#,
        "[]",
        r#"[
            {"sid": "s1", "kind": "seq_hole", "id": "h", "writes": ["mtx"]},
            {"sid": "s2", "kind": "return"}
        ]"#,
    );
    assert!(codes(&report).contains(&"E310"));
}

#[test]
fn seq_hole_protected_var_without_lock_is_e309() {
    let report = wrap(
        r#"[
            {"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"},
            {"name": "buf", "kind": "var", "type": "Var", "base": "Int", "init": 0}
        ]"#,
        r#"[{"var": "buf", "lock": "mtx"}]"#,
        r#"[
            {"sid": "s1", "kind": "seq_hole", "id": "h", "reads": ["buf"]},
            {"sid": "s2", "kind": "return"}
        ]"#,
    );
    assert!(codes(&report).contains(&"E309"));
}
