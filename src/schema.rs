//! Machine-readable CIR schema for prompt generation and normalisation.
//!
//! `schema` is the single source the generation prompt is rendered from. A test
//! in `tests/schema.rs` builds the minimal JSON for every statement and resource
//! kind from this table and deserialises it through the real `serde` types, so a
//! schema entry that drifts from `ast.rs` fails the build.

use serde_json::{json, Value};

pub const SID_REGEX: &str = "^s[0-9]+$";

/// Statement kind -> (required fields, optional fields).
pub fn statements() -> Vec<(&'static str, &'static [&'static str], &'static [&'static str])> {
    vec![
        ("nop", &[], &[]),
        ("assign_local", &["target", "expr"], &[]),
        ("read_shared", &["resource"], &["dst"]),
        ("write_shared", &["resource", "expr"], &[]),
        ("atomic_load", &["resource", "dst"], &[]),
        ("atomic_store", &["resource", "value"], &[]),
        ("atomic_cas", &["resource", "expected", "desired", "dst"], &[]),
        ("mutex_lock", &["resource"], &[]),
        ("mutex_unlock", &["resource"], &[]),
        ("channel_send", &["channel", "value"], &[]),
        ("channel_recv", &["channel", "dst"], &[]),
        ("condvar_wait", &["condvar", "lock"], &[]),
        ("condvar_notify", &["condvar"], &[]),
        ("condvar_notify_all", &["condvar"], &[]),
        ("semaphore_acquire", &["resource"], &["count"]),
        ("semaphore_release", &["resource"], &["count"]),
        ("call", &["func"], &["args", "dst"]),
        ("spawn", &["func", "handle"], &["args"]),
        ("scope", &["funcs"], &[]),
        ("join", &["handle"], &[]),
        ("goto", &["target"], &[]),
        ("branch", &["cond", "then", "else"], &[]),
        ("switch", &["var", "cases", "default"], &[]),
        ("return", &[], &["value"]),
    ]
}

/// Resource kind -> (JSON `kind`/`type` fields, required, optional).
pub fn resources() -> Vec<(&'static str, Value, &'static [&'static str], &'static [&'static str])> {
    vec![
        ("Mutex", json!({"kind": "sync", "type": "Mutex", "mode": "Sync"}),
         &["name", "kind", "type", "mode"], &[]),
        ("Condvar", json!({"kind": "sync", "type": "Condvar", "mode": "Sync"}),
         &["name", "kind", "type", "mode"], &[]),
        ("Semaphore", json!({"kind": "sync", "type": "Semaphore", "mode": "Sync"}),
         &["name", "kind", "type", "mode"], &["count"]),
        ("Channel", json!({"kind": "sync", "type": "Channel", "mode": "Sync"}),
         &["name", "kind", "type", "mode", "base", "capacity"], &[]),
        ("Var", json!({"kind": "var", "type": "Var"}),
         &["name", "kind", "type", "base", "init"], &[]),
        ("Atomic", json!({"kind": "var", "type": "Atomic"}),
         &["name", "kind", "type", "base", "init"], &[]),
    ]
}

pub fn schema() -> Value {
    let statements: Value = statements()
        .into_iter()
        .map(|(kind, required, optional)| {
            (kind.to_string(), json!({"required": required, "optional": optional}))
        })
        .collect::<serde_json::Map<_, _>>()
        .into();
    let resources: Value = resources()
        .into_iter()
        .map(|(kind, fixed, required, optional)| {
            (kind.to_string(),
             json!({"fields": fixed, "required": required, "optional": optional}))
        })
        .collect::<serde_json::Map<_, _>>()
        .into();
    json!({
        "version": "cir-schema-v1",
        "sid_regex": SID_REGEX,
        "expr": {"type": "string",
                 "note": "source expression as a JSON string, never an object"},
        "statements": statements,
        "resources": resources,
        "contract": {
            "properties": {
                "deadlock_free": {"required": ["kind", "id"]},
                "safety": {"required": ["kind", "id", "invariant"]},
                "reachability": {"required": ["kind", "id", "goal"]},
                "always_reachable": {"required": ["kind", "id", "goal"]},
                "unreachable": {"required": ["kind", "id", "bad"]}
            },
            "predicates": {
                "true": {"required": ["kind"]},
                "false": {"required": ["kind"]},
                "var_eq": {"required": ["kind", "resource", "value"]},
                "var_cmp": {"required": ["kind", "resource", "op", "value"]},
                "function_completed": {"required": ["kind", "function"]},
                "mutex_free": {"required": ["kind", "resource"]},
                "mutex_held": {"required": ["kind", "resource"]},
                "holds_all": {"required": ["kind", "function", "resources"]},
                "mutex_exclusive": {"required": ["kind", "resource"]},
                "never_holds_all": {"required": ["kind", "function", "resources"]},
                "channel_empty": {"required": ["kind", "resource"]},
                "channel_at_least": {"required": ["kind", "resource", "len"]},
                "not": {"required": ["kind", "predicate"]},
                "and": {"required": ["kind", "predicates"]},
                "or": {"required": ["kind", "predicates"]}
            },
            "preserved": {
                "reachable": {"required": ["kind", "description", "goal"]},
                "always": {"required": ["kind", "description", "invariant"]}
            },
            "bounds": ["max_threads", "max_frames_per_thread", "max_states",
                       "max_depth", "max_boundary_events"],
            "allowed_scope": ["functions", "modules", "allow_lock_reorder",
                              "allow_statement_delete"]
        }
    })
}

/// Minimal valid statement JSON for a kind (used by the self-check test).
pub fn minimal_statement(kind: &str) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("sid".into(), json!("s1"));
    obj.insert("kind".into(), json!(kind));
    let (_, required, _) = statements()
        .into_iter()
        .find(|(k, _, _)| *k == kind)
        .expect("known kind");
    for field in required {
        obj.insert((*field).into(), field_value(kind, field));
    }
    Value::Object(obj)
}

fn field_value(kind: &str, field: &str) -> Value {
    match (kind, field) {
        (_, "args" | "funcs") => json!(["main::f"]),
        ("switch", "cases") => json!({"A": "s2"}),
        (_, "target" | "then" | "else" | "default" | "dst") => json!("s2"),
        (_, "expr" | "value" | "expected" | "desired") => json!("x + 1"),
        (_, "cond") => json!("x == 1"),
        _ => json!("main::x"),
    }
}

/// Minimal valid resource JSON for a kind (used by the self-check test).
pub fn minimal_resource(kind: &str) -> Value {
    let (_, fixed, required, _) = resources()
        .into_iter()
        .find(|(k, _, _, _)| *k == kind)
        .expect("known resource kind");
    let mut obj = fixed.as_object().cloned().unwrap_or_default();
    for field in required {
        if obj.contains_key(*field) {
            continue;
        }
        obj.insert((*field).into(), match *field {
            "name" => json!("r"),
            "base" => json!("Int"),
            "capacity" => json!(1),
            "init" => json!(0),
            _ => json!("r"),
        });
    }
    Value::Object(obj)
}
