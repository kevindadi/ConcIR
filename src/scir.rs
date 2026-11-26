//! Structured Surface IR (`SCIR` v4.0) and deterministic lowering into flat CIR.
//!
//! Functions may author structured control flow (`lock_guard`, `if_else`,
//! `while_loop`, `while_wait`, and single `stmt` nodes) in `structured_body`.
//! [`lower_program`] deterministically expands every `structured_body` into
//! flat CFG `body: Vec<Stmt>` with sequential `s1, s2, ...` IDs and returns a
//! [`TraceMap`] (`concir-trace-map/1`) recording provenance (`structured_id`,
//! `hole_id`, and inherited `req` requirement tags).

use std::collections::BTreeMap;

use serde::de::{self, Deserializer};
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};

use crate::ast::{Op, Program, Stmt};

pub const TRACE_MAP_SCHEMA: &str = "concir-trace-map/1";

/// Structured statement in `Function.structured_body` (SCIR v4.0).
#[derive(Debug, Clone)]
pub enum StructuredStmt {
    /// Embeds a single flat [`Op`].
    Stmt {
        id: String,
        req: Vec<String>,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLockGuard {
    #[serde(default)]
    id: String,
    #[serde(default)]
    req: Vec<String>,
    #[allow(dead_code)]
    kind: String,
    resource: String,
    #[serde(default)]
    body: Vec<StructuredStmt>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIfElse {
    #[serde(default)]
    id: String,
    #[serde(default)]
    req: Vec<String>,
    #[allow(dead_code)]
    kind: String,
    cond: String,
    #[serde(default)]
    then_body: Vec<StructuredStmt>,
    #[serde(default)]
    else_body: Vec<StructuredStmt>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWhileLoop {
    #[serde(default)]
    id: String,
    #[serde(default)]
    req: Vec<String>,
    #[allow(dead_code)]
    kind: String,
    cond: String,
    #[serde(default)]
    body: Vec<StructuredStmt>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWhileWait {
    #[serde(default)]
    id: String,
    #[serde(default)]
    req: Vec<String>,
    #[allow(dead_code)]
    kind: String,
    cond: String,
    condvar: String,
    lock: String,
    #[serde(default)]
    body: Vec<StructuredStmt>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStmtWrapper {
    #[serde(default)]
    id: String,
    #[serde(default)]
    req: Vec<String>,
    #[allow(dead_code)]
    kind: String,
    #[serde(default)]
    op: Option<Op>,
    #[serde(default)]
    stmt: Option<Op>,
}

impl<'de> Deserialize<'de> for StructuredStmt {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let obj = value
            .as_object()
            .ok_or_else(|| de::Error::custom("structured statement must be a JSON object"))?;
        let kind = obj
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| de::Error::custom("structured statement missing string 'kind'"))?;

        match kind {
            "lock_guard" | "rwlock_read_guard" | "rwlock_write_guard" => {
                let raw: RawLockGuard =
                    serde_json::from_value(value).map_err(de::Error::custom)?;
                Ok(match raw.kind.as_str() {
                    "lock_guard" => StructuredStmt::LockGuard {
                        id: raw.id,
                        req: raw.req,
                        resource: raw.resource,
                        body: raw.body,
                    },
                    "rwlock_read_guard" => StructuredStmt::RwLockReadGuard {
                        id: raw.id,
                        req: raw.req,
                        resource: raw.resource,
                        body: raw.body,
                    },
                    _ => StructuredStmt::RwLockWriteGuard {
                        id: raw.id,
                        req: raw.req,
                        resource: raw.resource,
                        body: raw.body,
                    },
                })
            }
            "if_else" => {
                let raw: RawIfElse = serde_json::from_value(value).map_err(de::Error::custom)?;
                Ok(StructuredStmt::IfElse {
                    id: raw.id,
                    req: raw.req,
                    cond: raw.cond,
                    then_body: raw.then_body,
                    else_body: raw.else_body,
                })
            }
            "while_loop" => {
                let raw: RawWhileLoop =
                    serde_json::from_value(value).map_err(de::Error::custom)?;
                Ok(StructuredStmt::WhileLoop {
                    id: raw.id,
                    req: raw.req,
                    cond: raw.cond,
                    body: raw.body,
                })
            }
            "while_wait" => {
                let raw: RawWhileWait =
                    serde_json::from_value(value).map_err(de::Error::custom)?;
                Ok(StructuredStmt::WhileWait {
                    id: raw.id,
                    req: raw.req,
                    cond: raw.cond,
                    condvar: raw.condvar,
                    lock: raw.lock,
                    body: raw.body,
                })
            }
            "stmt" => {
                let raw: RawStmtWrapper =
                    serde_json::from_value(value).map_err(de::Error::custom)?;
                let op = match (raw.op, raw.stmt) {
                    (Some(op), None) | (None, Some(op)) => op,
                    (Some(_), Some(_)) => {
                        return Err(de::Error::custom(
                            "structured 'stmt' cannot specify both 'op' and 'stmt'",
                        ));
                    }
                    (None, None) => {
                        return Err(de::Error::custom(
                            "structured 'stmt' requires an 'op' field",
                        ));
                    }
                };
                Ok(StructuredStmt::Stmt {
                    id: raw.id,
                    req: raw.req,
                    op,
                })
            }
            // Also accept any flat `Op` directly inside `structured_body`,
            // extracting optional `id` and `req` metadata.
            _ => {
                let mut map = obj.clone();
                let id = match map.remove("id") {
                    Some(v) => {
                        // Note: `seq_hole` uses `"id"` as the hole identifier!
                        // When a flat `seq_hole` appears directly inside `structured_body`,
                        // keep `"id"` for `Op::SeqHole` and reuse it as the structured id.
                        let id_str: String =
                            serde_json::from_value(v.clone()).map_err(de::Error::custom)?;
                        if kind == "seq_hole" {
                            map.insert("id".to_string(), v);
                        }
                        id_str
                    }
                    None => String::new(),
                };
                let req: Vec<String> = match map.remove("req") {
                    Some(v) => serde_json::from_value(v).map_err(de::Error::custom)?,
                    None => Vec::new(),
                };
                let op: Op = serde_json::from_value(serde_json::Value::Object(map))
                    .map_err(de::Error::custom)?;
                Ok(StructuredStmt::Stmt { id, req, op })
            }
        }
    }
}

impl Serialize for StructuredStmt {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        match self {
            StructuredStmt::Stmt { id, req, op } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("kind", "stmt")?;
                map.serialize_entry("op", op)?;
                if !req.is_empty() {
                    map.serialize_entry("req", req)?;
                }
            }
            StructuredStmt::LockGuard {
                id,
                req,
                resource,
                body,
            } => {
                serialize_guard(&mut map, id, "lock_guard", resource, body, req)?;
            }
            StructuredStmt::RwLockReadGuard {
                id,
                req,
                resource,
                body,
            } => {
                serialize_guard(&mut map, id, "rwlock_read_guard", resource, body, req)?;
            }
            StructuredStmt::RwLockWriteGuard {
                id,
                req,
                resource,
                body,
            } => {
                serialize_guard(&mut map, id, "rwlock_write_guard", resource, body, req)?;
            }
            StructuredStmt::IfElse {
                id,
                req,
                cond,
                then_body,
                else_body,
            } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("kind", "if_else")?;
                map.serialize_entry("cond", cond)?;
                map.serialize_entry("then_body", then_body)?;
                if !else_body.is_empty() {
                    map.serialize_entry("else_body", else_body)?;
                }
                if !req.is_empty() {
                    map.serialize_entry("req", req)?;
                }
            }
            StructuredStmt::WhileLoop {
                id,
                req,
                cond,
                body,
            } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("kind", "while_loop")?;
                map.serialize_entry("cond", cond)?;
                map.serialize_entry("body", body)?;
                if !req.is_empty() {
                    map.serialize_entry("req", req)?;
                }
            }
            StructuredStmt::WhileWait {
                id,
                req,
                cond,
                condvar,
                lock,
                body,
            } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("kind", "while_wait")?;
                map.serialize_entry("cond", cond)?;
                map.serialize_entry("condvar", condvar)?;
                map.serialize_entry("lock", lock)?;
                if !body.is_empty() {
                    map.serialize_entry("body", body)?;
                }
                if !req.is_empty() {
                    map.serialize_entry("req", req)?;
                }
            }
        }
        map.end()
    }
}

fn serialize_guard<M: SerializeMap>(
    map: &mut M,
    id: &str,
    kind: &str,
    resource: &str,
    body: &[StructuredStmt],
    req: &[String],
) -> Result<(), M::Error> {
    map.serialize_entry("id", id)?;
    map.serialize_entry("kind", kind)?;
    map.serialize_entry("resource", resource)?;
    map.serialize_entry("body", body)?;
    if !req.is_empty() {
        map.serialize_entry("req", req)?;
    }
    Ok(())
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
pub fn lower_program(program: &mut Program) -> Result<TraceMap, String> {
    let mut trace_map = TraceMap::empty(program.program.clone());

    for module in &mut program.modules {
        let mod_name = module.name.clone();
        let mod_req = module.req.clone();
        for func in &mut module.functions {
            let fn_name = func.name.clone();
            let base_req = merge_req(&mod_req, &func.req);

            if !func.structured_body.is_empty() && !func.body.is_empty() {
                return Err(format!(
                    "function '{mod_name}::{fn_name}' specifies both 'body' and 'structured_body'"
                ));
            }

            if !func.structured_body.is_empty() {
                let structured = std::mem::take(&mut func.structured_body);
                let (flat_body, entries) = lower_function(&mod_name, &fn_name, &base_req, &structured)?;
                func.body = flat_body;
                for (k, v) in entries {
                    trace_map.entries.insert(k, v);
                }
            } else {
                for stmt in &func.body {
                    let key = format!("{mod_name}::{fn_name}::{}", stmt.sid);
                    let hole_id = match &stmt.op {
                        Op::SeqHole { id, .. } => Some(id.clone()),
                        _ => None,
                    };
                    let req = merge_req(&base_req, &stmt.effective_req());
                    trace_map.entries.insert(
                        key,
                        TraceMapEntry {
                            module: mod_name.clone(),
                            function: fn_name.clone(),
                            sid: stmt.sid.clone(),
                            structured_id: None,
                            hole_id,
                            req,
                        },
                    );
                }
            }
        }
    }

    Ok(trace_map)
}

#[derive(Clone, Copy)]
struct Label(usize);

enum PendingOp {
    Flat(Op),
    Branch {
        cond: String,
        then: Label,
        else_target: Label,
    },
    Goto {
        target: Label,
    },
}

struct PendingStmt {
    op: PendingOp,
    structured_id: Option<String>,
    hole_id: Option<String>,
    req: Vec<String>,
}

#[derive(Clone, Copy)]
enum GuardKind {
    Mutex,
    RwLockRead,
    RwLockWrite,
}

impl GuardKind {
    fn lock_op(self, resource: String) -> Op {
        match self {
            GuardKind::Mutex => Op::MutexLock { resource },
            GuardKind::RwLockRead => Op::RwLockRead { resource },
            GuardKind::RwLockWrite => Op::RwLockWrite { resource },
        }
    }

    fn unlock_op(self, resource: String) -> Op {
        match self {
            GuardKind::Mutex => Op::MutexUnlock { resource },
            GuardKind::RwLockRead | GuardKind::RwLockWrite => Op::RwLockUnlock { resource },
        }
    }
}

#[derive(Clone)]
struct ActiveGuard {
    kind: GuardKind,
    resource: String,
}

struct FnLowerer {
    stmts: Vec<PendingStmt>,
    labels: Vec<Option<usize>>,
    lock_stack: Vec<ActiveGuard>,
}

impl FnLowerer {
    fn new() -> Self {
        FnLowerer {
            stmts: Vec::new(),
            labels: Vec::new(),
            lock_stack: Vec::new(),
        }
    }

    fn new_label(&mut self) -> Label {
        let id = self.labels.len();
        self.labels.push(None);
        Label(id)
    }

    fn place_label(&mut self, label: Label) {
        self.labels[label.0] = Some(self.stmts.len());
    }

    fn emit(
        &mut self,
        op: PendingOp,
        structured_id: Option<String>,
        hole_id: Option<String>,
        req: Vec<String>,
    ) {
        self.stmts.push(PendingStmt {
            op,
            structured_id,
            hole_id,
            req,
        });
    }

    fn sid_opt(id: &str) -> Option<String> {
        if id.is_empty() {
            None
        } else {
            Some(id.to_string())
        }
    }

    fn lower_block(&mut self, block: &[StructuredStmt], inherited_req: &[String]) -> bool {
        let mut falls_through = true;
        for node in block {
            falls_through = self.lower_stmt(node, inherited_req);
        }
        falls_through
    }

    fn lower_guard(
        &mut self,
        kind: GuardKind,
        resource: &str,
        body: &[StructuredStmt],
        sid_tag: Option<String>,
        own_req: Vec<String>,
    ) -> bool {
        self.emit(
            PendingOp::Flat(kind.lock_op(resource.to_string())),
            sid_tag.clone(),
            None,
            own_req.clone(),
        );
        self.lock_stack.push(ActiveGuard {
            kind,
            resource: resource.to_string(),
        });
        let body_ft = self.lower_block(body, &own_req);
        self.lock_stack.pop();
        if body_ft {
            self.emit(
                PendingOp::Flat(kind.unlock_op(resource.to_string())),
                sid_tag,
                None,
                own_req,
            );
        }
        body_ft
    }

    fn lower_stmt(&mut self, node: &StructuredStmt, inherited_req: &[String]) -> bool {
        let own_req = merge_req(inherited_req, node.req());
        let sid_tag = Self::sid_opt(node.id());
        match node {
            StructuredStmt::Stmt { op, .. } => {
                let mut op_cloned = op.clone();
                let (hole_id, stmt_req) = match &mut op_cloned {
                    Op::SeqHole { id, req, .. } => {
                        let merged = merge_req(&own_req, req);
                        // Clear op.req so serialized flat Stmt has a single `"req"` key.
                        req.clear();
                        (Some(id.clone()), merged)
                    }
                    _ => (None, own_req),
                };

                match op_cloned {
                    Op::Return { value } => {
                        // Auto-unlock all active lexical lock guards in LIFO order.
                        let active_locks: Vec<ActiveGuard> =
                            self.lock_stack.iter().rev().cloned().collect();
                        for guard in active_locks {
                            self.emit(
                                PendingOp::Flat(guard.kind.unlock_op(guard.resource)),
                                sid_tag.clone(),
                                None,
                                stmt_req.clone(),
                            );
                        }
                        self.emit(
                            PendingOp::Flat(Op::Return { value }),
                            sid_tag,
                            None,
                            stmt_req,
                        );
                        false
                    }
                    Op::Goto { .. }
                    | Op::Branch { .. }
                    | Op::Switch { .. }
                    | Op::Select { .. } => {
                        self.emit(PendingOp::Flat(op_cloned), sid_tag, hole_id, stmt_req);
                        false
                    }
                    other => {
                        self.emit(PendingOp::Flat(other), sid_tag, hole_id, stmt_req);
                        true
                    }
                }
            }
            StructuredStmt::LockGuard { resource, body, .. } => {
                self.lower_guard(GuardKind::Mutex, resource, body, sid_tag, own_req)
            }
            StructuredStmt::RwLockReadGuard { resource, body, .. } => {
                self.lower_guard(GuardKind::RwLockRead, resource, body, sid_tag, own_req)
            }
            StructuredStmt::RwLockWriteGuard { resource, body, .. } => {
                self.lower_guard(GuardKind::RwLockWrite, resource, body, sid_tag, own_req)
            }
            StructuredStmt::IfElse {
                cond,
                then_body,
                else_body,
                ..
            } => {
                if then_body.is_empty() && else_body.is_empty() {
                    self.emit(PendingOp::Flat(Op::Nop), sid_tag, None, own_req);
                    return true;
                }
                if else_body.is_empty() {
                    let then_lbl = self.new_label();
                    let after_lbl = self.new_label();
                    self.emit(
                        PendingOp::Branch {
                            cond: cond.clone(),
                            then: then_lbl,
                            else_target: after_lbl,
                        },
                        sid_tag,
                        None,
                        own_req.clone(),
                    );
                    self.place_label(then_lbl);
                    let _ = self.lower_block(then_body, &own_req);
                    self.place_label(after_lbl);
                    return true;
                }
                if then_body.is_empty() {
                    let else_lbl = self.new_label();
                    let after_lbl = self.new_label();
                    self.emit(
                        PendingOp::Branch {
                            cond: cond.clone(),
                            then: after_lbl,
                            else_target: else_lbl,
                        },
                        sid_tag,
                        None,
                        own_req.clone(),
                    );
                    self.place_label(else_lbl);
                    let _ = self.lower_block(else_body, &own_req);
                    self.place_label(after_lbl);
                    return true;
                }
                let then_lbl = self.new_label();
                let else_lbl = self.new_label();
                let after_lbl = self.new_label();
                self.emit(
                    PendingOp::Branch {
                        cond: cond.clone(),
                        then: then_lbl,
                        else_target: else_lbl,
                    },
                    sid_tag.clone(),
                    None,
                    own_req.clone(),
                );
                self.place_label(then_lbl);
                let then_ft = self.lower_block(then_body, &own_req);
                if then_ft {
                    self.emit(
                        PendingOp::Goto { target: after_lbl },
                        sid_tag,
                        None,
                        own_req.clone(),
                    );
                }
                self.place_label(else_lbl);
                let else_ft = self.lower_block(else_body, &own_req);
                self.place_label(after_lbl);
                then_ft || else_ft
            }
            StructuredStmt::WhileLoop { cond, body, .. } => {
                let header_lbl = self.new_label();
                let body_lbl = self.new_label();
                let exit_lbl = self.new_label();
                self.place_label(header_lbl);
                self.emit(
                    PendingOp::Branch {
                        cond: cond.clone(),
                        then: body_lbl,
                        else_target: exit_lbl,
                    },
                    sid_tag.clone(),
                    None,
                    own_req.clone(),
                );
                self.place_label(body_lbl);
                let body_ft = self.lower_block(body, &own_req);
                if body_ft {
                    self.emit(
                        PendingOp::Goto { target: header_lbl },
                        sid_tag,
                        None,
                        own_req,
                    );
                }
                self.place_label(exit_lbl);
                true
            }
            StructuredStmt::WhileWait {
                cond,
                condvar,
                lock,
                body,
                ..
            } => {
                let header_lbl = self.new_label();
                let wait_entry = self.new_label();
                let exit_lbl = self.new_label();
                self.place_label(header_lbl);
                self.emit(
                    PendingOp::Branch {
                        cond: cond.clone(),
                        then: wait_entry,
                        else_target: exit_lbl,
                    },
                    sid_tag.clone(),
                    None,
                    own_req.clone(),
                );
                self.place_label(wait_entry);
                let body_ft = self.lower_block(body, &own_req);
                if body_ft {
                    self.emit(
                        PendingOp::Flat(Op::CondvarWait {
                            condvar: condvar.clone(),
                            lock: lock.clone(),
                        }),
                        sid_tag.clone(),
                        None,
                        own_req.clone(),
                    );
                    self.emit(
                        PendingOp::Goto { target: header_lbl },
                        sid_tag,
                        None,
                        own_req,
                    );
                }
                self.place_label(exit_lbl);
                true
            }
        }
    }
}

fn lower_function(
    mod_name: &str,
    fn_name: &str,
    base_req: &[String],
    structured: &[StructuredStmt],
) -> Result<(Vec<Stmt>, Vec<(String, TraceMapEntry)>), String> {
    let mut lowerer = FnLowerer::new();
    let falls_through = lowerer.lower_block(structured, base_req);

    let end_idx = lowerer.stmts.len();
    let label_targets_end = lowerer
        .labels
        .iter()
        .any(|pos| *pos == Some(end_idx));
    if falls_through || label_targets_end {
        lowerer.emit(
            PendingOp::Flat(Op::Return { value: None }),
            None,
            None,
            base_req.to_vec(),
        );
    }

    let resolve = |label: Label, labels: &[Option<usize>], len: usize| -> Result<String, String> {
        let idx = labels
            .get(label.0)
            .and_then(|o| *o)
            .ok_or_else(|| format!("internal SCIR lowering error: unplaced label {}", label.0))?;
        if idx >= len {
            return Err(format!(
                "internal SCIR lowering error: label {} points past end ({idx} >= {len})",
                label.0
            ));
        }
        Ok(format!("s{}", idx + 1))
    };

    let total = lowerer.stmts.len();
    let labels = lowerer.labels;
    let mut flat = Vec::with_capacity(total);
    let mut entries = Vec::with_capacity(total);

    for (idx, pending) in lowerer.stmts.into_iter().enumerate() {
        let sid = format!("s{}", idx + 1);
        let op = match pending.op {
            PendingOp::Flat(op) => op,
            PendingOp::Branch {
                cond,
                then,
                else_target,
            } => Op::Branch {
                cond,
                then: resolve(then, &labels, total)?,
                else_target: resolve(else_target, &labels, total)?,
            },
            PendingOp::Goto { target } => Op::Goto {
                target: resolve(target, &labels, total)?,
            },
        };
        let key = format!("{mod_name}::{fn_name}::{sid}");
        entries.push((
            key,
            TraceMapEntry {
                module: mod_name.to_string(),
                function: fn_name.to_string(),
                sid: sid.clone(),
                structured_id: pending.structured_id,
                hole_id: pending.hole_id,
                req: pending.req.clone(),
            },
        ));
        flat.push(Stmt {
            sid,
            req: pending.req,
            op,
        });
    }

    Ok((flat, entries))
}

fn merge_req(a: &[String], b: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    for item in a.iter().chain(b.iter()) {
        if !out.contains(item) {
            out.push(item.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
