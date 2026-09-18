//! Self-check: the machine schema must match the real serde definitions.

use concir::ast::{Resource, Stmt};
use concir::schema::{minimal_resource, minimal_statement, resources, statements};

#[test]
fn every_statement_kind_deserializes_from_its_schema() {
    for (kind, _, _) in statements() {
        let value = minimal_statement(kind);
        let parsed: Result<Stmt, _> = serde_json::from_value(value.clone());
        assert!(parsed.is_ok(), "statement {kind} rejected: {:?} ({value})", parsed.err());
    }
}

#[test]
fn every_resource_kind_deserializes_from_its_schema() {
    for (kind, _, _, _) in resources() {
        let value = minimal_resource(kind);
        let parsed: Result<Resource, _> = serde_json::from_value(value.clone());
        assert!(parsed.is_ok(), "resource {kind} rejected: {:?} ({value})", parsed.err());
    }
}

#[test]
fn schema_lists_the_expected_kinds() {
    let names: Vec<&str> = statements().into_iter().map(|(k, _, _)| k).collect();
    for expected in ["mutex_lock", "condvar_wait", "channel_send", "write_shared",
                     "branch", "scope", "return"] {
        assert!(names.contains(&expected), "missing statement kind {expected}");
    }
}
