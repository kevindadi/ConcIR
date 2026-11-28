mod lower;
mod serde_impl;

pub use lower::lower_program;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ast::Op;

pub const TRACE_MAP_SCHEMA: &str = "concir-trace-map/1";

/// Structured statement in `Function.structured_body` (SCIR v4.0).
#[derive(Debug, Clone)]
pub enum StructuredStmt {
    /// Embeds a single flat [`Op`].
    Stmt {
        id: String,
        req: Vec<String>,
        milestone: Option<String>,
        op: Op,
    },
    /// Lexical mutex guard: locks `resource` on entry and unlocks on normal
    /// fallthrough AND before any `return` inside `body` (in LIFO order).
    LockGuard {
        id: String,
        req: Vec<String>,
        resource: String,
        body: Vec<StructuredStmt>,
    },
    /// Lexical rwlock read guard: acquires shared read on `resource` on entry
    /// and unlocks on normal fallthrough AND before any `return` inside `body` (in LIFO order).
    RwLockReadGuard {
        id: String,
        req: Vec<String>,
        resource: String,
        body: Vec<StructuredStmt>,
    },
    /// Lexical rwlock write guard: acquires exclusive write on `resource` on entry
    /// and unlocks on normal fallthrough AND before any `return` inside `body` (in LIFO order).
    RwLockWriteGuard {
        id: String,
        req: Vec<String>,
        resource: String,
        body: Vec<StructuredStmt>,
    },
    /// Structured conditional branch.
    IfElse {
        id: String,
        req: Vec<String>,
        cond: String,
        then_body: Vec<StructuredStmt>,
        else_body: Vec<StructuredStmt>,
    },
    /// Structured while loop.
    WhileLoop {
        id: String,
        req: Vec<String>,
        cond: String,
        body: Vec<StructuredStmt>,
    },
    /// Structured condition-variable wait loop:
    /// `while (cond) { body...; condvar_wait(condvar, lock); }`.
    WhileWait {
        id: String,
        req: Vec<String>,
        cond: String,
        condvar: String,
        lock: String,
        body: Vec<StructuredStmt>,
    },
}

impl StructuredStmt {
    pub fn id(&self) -> &str {
        match self {
            StructuredStmt::Stmt { id, .. }
            | StructuredStmt::LockGuard { id, .. }
            | StructuredStmt::RwLockReadGuard { id, .. }
            | StructuredStmt::RwLockWriteGuard { id, .. }
            | StructuredStmt::IfElse { id, .. }
            | StructuredStmt::WhileLoop { id, .. }
            | StructuredStmt::WhileWait { id, .. } => id,
        }
    }

    pub fn req(&self) -> &[String] {
        match self {
            StructuredStmt::Stmt { req, .. }
            | StructuredStmt::LockGuard { req, .. }
            | StructuredStmt::RwLockReadGuard { req, .. }
            | StructuredStmt::RwLockWriteGuard { req, .. }
            | StructuredStmt::IfElse { req, .. }
            | StructuredStmt::WhileLoop { req, .. }
            | StructuredStmt::WhileWait { req, .. } => req,
        }
    }
}

/// One lowered statement provenance entry in [`TraceMap`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceMapEntry {
    pub module: String,
    pub function: String,
    pub sid: String,
    pub structured_id: Option<String>,
    pub hole_id: Option<String>,
    #[serde(default)]
    pub req: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone: Option<String>,
}

/// Traceability map emitted by SCIR lowering (`concir-trace-map/1`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceMap {
    pub schema: String,
    pub program: String,
    pub entries: BTreeMap<String, TraceMapEntry>,
}

impl TraceMap {
    pub fn empty(program: impl Into<String>) -> Self {
        TraceMap {
            schema: TRACE_MAP_SCHEMA.to_string(),
            program: program.into(),
            entries: BTreeMap::new(),
        }
    }
}

/// Lower every function's `structured_body` in `program` into flat `body`.
/// Idempotent when `structured_body` is already empty.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Program;

    #[test]
    fn rwlock_read_and_write_guards_lower_normal_exit_and_roundtrip() {
        let json = r#"{
            "program": "scir_rw_normal",
            "version": "4.0.0",
            "entry": "main::main",
            "modules": [{
                "name": "main",
                "resources": [
                    {"name": "rw", "kind": "sync", "type": "RwLock", "mode": "Sync"}
                ],
                "protection": [],
                "functions": [{
                    "name": "main",
                    "kind": "normal",
                    "structured_body": [
                        {
                            "id": "rg1",
                            "kind": "rwlock_read_guard",
                            "resource": "rw",
                            "req": ["R1"],
                            "body": [
                                {"id": "n1", "kind": "nop"}
                            ]
                        },
                        {
                            "id": "wg1",
                            "kind": "rwlock_write_guard",
                            "resource": "rw",
                            "req": ["R2"],
                            "body": [
                                {"id": "n2", "kind": "nop"}
                            ]
                        }
                    ]
                }]
            }]
        }"#;

        let mut prog: Program = serde_json::from_str(json).expect("parse");
        // Verify serialize/deserialize roundtrip preserves structured guard kinds.
        let rt_json = serde_json::to_string(&prog).expect("serialize");
        assert!(rt_json.contains("\"kind\":\"rwlock_read_guard\""));
        assert!(rt_json.contains("\"kind\":\"rwlock_write_guard\""));
        let _: Program = serde_json::from_str(&rt_json).expect("re-parse");

        let trace_map = lower_program(&mut prog).expect("lower");
        let ops: Vec<&Op> = prog.modules[0].functions[0]
            .body
            .iter()
            .map(|s| &s.op)
            .collect();
        assert!(matches!(ops[0], Op::RwLockRead { resource } if resource == "rw"));
        assert!(matches!(ops[1], Op::Nop));
        assert!(matches!(ops[2], Op::RwLockUnlock { resource } if resource == "rw"));
        assert!(matches!(ops[3], Op::RwLockWrite { resource } if resource == "rw"));
        assert!(matches!(ops[4], Op::Nop));
        assert!(matches!(ops[5], Op::RwLockUnlock { resource } if resource == "rw"));
        assert!(matches!(ops[6], Op::Return { .. }));
        assert_eq!(trace_map.entries["main::main::s1"].req, vec!["R1".to_string()]);
        assert_eq!(trace_map.entries["main::main::s4"].req, vec!["R2".to_string()]);
    }

    #[test]
    fn rwlock_guards_unwind_in_lifo_order_with_lock_guard_on_early_return() {
        let json = r#"{
            "program": "scir_rw_unwind",
            "version": "4.0.0",
            "entry": "main::main",
            "modules": [{
                "name": "main",
                "resources": [
                    {"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"},
                    {"name": "rw_r", "kind": "sync", "type": "RwLock", "mode": "Sync"},
                    {"name": "rw_w", "kind": "sync", "type": "RwLock", "mode": "Sync"}
                ],
                "protection": [],
                "functions": [{
                    "name": "main",
                    "kind": "normal",
                    "locals": [{"name": "early", "type": "Bool", "modeled": true, "init": false}],
                    "structured_body": [
                        {
                            "id": "lg_outer",
                            "kind": "lock_guard",
                            "resource": "mtx",
                            "body": [
                                {
                                    "id": "rg_mid",
                                    "kind": "rwlock_read_guard",
                                    "resource": "rw_r",
                                    "body": [
                                        {
                                            "id": "wg_inner",
                                            "kind": "rwlock_write_guard",
                                            "resource": "rw_w",
                                            "body": [
                                                {
                                                    "id": "br1",
                                                    "kind": "if_else",
                                                    "cond": "early == true",
                                                    "then_body": [
                                                        {"id": "ret_early", "kind": "return"}
                                                    ],
                                                    "else_body": []
                                                }
                                            ]
                                        }
                                    ]
                                }
                            ]
                        }
                    ]
                }]
            }]
        }"#;

        let mut prog: Program = serde_json::from_str(json).expect("parse");
        lower_program(&mut prog).expect("lower");
        let body = &prog.modules[0].functions[0].body;
        let report = crate::validate::validate(&prog);
        assert!(report.valid, "validation failed: {:?}", report.diagnostics);

        // Find the early return branch target and verify LIFO unlock order:
        // rw_w -> rw_r -> mtx -> Return.
        let ops: Vec<&Op> = body.iter().map(|s| &s.op).collect();
        let ret_idx = ops
            .iter()
            .position(|op| matches!(op, Op::Return { .. }))
            .expect("early return");
        assert!(ret_idx >= 3);
        assert!(matches!(ops[ret_idx - 3], Op::RwLockUnlock { resource } if resource == "rw_w"));
        assert!(matches!(ops[ret_idx - 2], Op::RwLockUnlock { resource } if resource == "rw_r"));
        assert!(matches!(ops[ret_idx - 1], Op::MutexUnlock { resource } if resource == "mtx"));

        // Normal fallthrough after the `if` also unlocks in LIFO order: rw_w -> rw_r -> mtx -> Return.
        let n = ops.len();
        assert!(matches!(ops[n - 4], Op::RwLockUnlock { resource } if resource == "rw_w"));
        assert!(matches!(ops[n - 3], Op::RwLockUnlock { resource } if resource == "rw_r"));
        assert!(matches!(ops[n - 2], Op::MutexUnlock { resource } if resource == "mtx"));
        assert!(matches!(ops[n - 1], Op::Return { .. }));
    }
}
