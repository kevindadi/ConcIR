use crate::ast::{Op, Program, Stmt};

use super::{StructuredStmt, TraceMap, TraceMapEntry};

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
                            milestone: stmt.milestone.clone(),
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
    milestone: Option<String>,
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
        milestone: Option<String>,
    ) {
        self.stmts.push(PendingStmt {
            op,
            structured_id,
            hole_id,
            req,
            milestone,
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
            None,
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
                None,
            );
        }
        body_ft
    }

    fn lower_stmt(&mut self, node: &StructuredStmt, inherited_req: &[String]) -> bool {
        let own_req = merge_req(inherited_req, node.req());
        let sid_tag = Self::sid_opt(node.id());
        match node {
            StructuredStmt::Stmt { op, milestone, .. } => {
                let mut op_cloned = op.clone();
                let milestone_cloned = milestone.clone();
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
                                None,
                            );
                        }
                        self.emit(
                            PendingOp::Flat(Op::Return { value }),
                            sid_tag,
                            None,
                            stmt_req,
                            milestone_cloned,
                        );
                        false
                    }
                    Op::Goto { .. }
                    | Op::Branch { .. }
                    | Op::Switch { .. }
                    | Op::Select { .. } => {
                        self.emit(
                            PendingOp::Flat(op_cloned),
                            sid_tag,
                            hole_id,
                            stmt_req,
                            milestone_cloned,
                        );
                        false
                    }
                    other => {
                        self.emit(
                            PendingOp::Flat(other),
                            sid_tag,
                            hole_id,
                            stmt_req,
                            milestone_cloned,
                        );
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
                    self.emit(PendingOp::Flat(Op::Nop), sid_tag, None, own_req, None);
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
                        None,
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
                        None,
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
                    None,
                );
                self.place_label(then_lbl);
                let then_ft = self.lower_block(then_body, &own_req);
                if then_ft {
                    self.emit(
                        PendingOp::Goto { target: after_lbl },
                        sid_tag,
                        None,
                        own_req.clone(),
                        None,
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
                    None,
                );
                self.place_label(body_lbl);
                let body_ft = self.lower_block(body, &own_req);
                if body_ft {
                    self.emit(
                        PendingOp::Goto { target: header_lbl },
                        sid_tag,
                        None,
                        own_req,
                        None,
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
                    None,
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
                        None,
                    );
                    self.emit(
                        PendingOp::Goto { target: header_lbl },
                        sid_tag,
                        None,
                        own_req,
                        None,
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
            None,
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
                milestone: pending.milestone.clone(),
            },
        ));
        flat.push(Stmt {
            sid,
            req: pending.req,
            milestone: pending.milestone,
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

