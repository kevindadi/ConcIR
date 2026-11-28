use std::collections::BTreeMap;

use concir_ir::ast::{BaseType, ComplexBaseType};
use concir_sem::ids::{ResourceId, SlotRef};
use concir_sem::program::{ResKind, SemFunction, SemOp, SemStmt};

use crate::{
    child_tags, default_expr, event_at_attempt, is_observable, rust_ty, sanitize, Gen, Hole,
    SidLocation, TagRecord,
};

enum StructuredNode {
    Stmt(usize),
    While {
        branch_idx: usize,
        body: Vec<StructuredNode>,
        back_goto_idx: usize,
    },
    IfElse {
        branch_idx: usize,
        then_nodes: Vec<StructuredNode>,
        then_goto_idx: Option<usize>,
        else_nodes: Vec<StructuredNode>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockMode {
    Mutex,
    RwRead,
    RwWrite,
}

fn try_structure_function(func: &concir_sem::program::SemFunction) -> Option<Vec<StructuredNode>> {
    use std::collections::BTreeSet;

    let mut total_jump_edges = 0usize;
    for (idx, stmt) in func.body.iter().enumerate() {
        match &stmt.op {
            SemOp::Switch { .. } | SemOp::Unsupported { .. } => return None,
            SemOp::Goto { .. } => total_jump_edges += 1,
            SemOp::Branch { .. } => total_jump_edges += 2,
            SemOp::MutexUnlock { .. } | SemOp::RwLockUnlock { .. } => {
                // Ensure every unlock corresponds to an earlier lock in structured scope;
                // balanced SCIR/CIR functions satisfy this, and rustc verifies lexical drop.
                let _ = idx;
            }
            _ => {}
        }
    }

    let mut consumed_edges: BTreeSet<(usize, usize)> = BTreeSet::new();
    let nodes = try_structure_range(&func.body, 0, func.body.len(), &mut consumed_edges)?;
    if consumed_edges.len() != total_jump_edges {
        return None;
    }
    // Also verify mutex/rwlock locks/unlocks do not cross branch boundaries asymmetrically
    // (e.g. locked inside `if` without unlocking inside the same `if`).
    let mut held: BTreeMap<ResourceId, LockMode> = BTreeMap::new();
    if !check_lexical_locks(&func.body, &nodes, &mut held) {
        return None;
    }
    Some(nodes)
}

/// Returns `true` if `nodes` preserves lexical `MutexGuard`/`RwLockGuard` scoping.
fn check_lexical_locks(
    body: &[concir_sem::program::SemStmt],
    nodes: &[StructuredNode],
    held: &mut BTreeMap<ResourceId, LockMode>,
) -> bool {
    check_lexical_locks_inner(body, nodes, held).is_some()
}

fn check_lexical_locks_inner(
    body: &[concir_sem::program::SemStmt],
    nodes: &[StructuredNode],
    held: &mut BTreeMap<ResourceId, LockMode>,
) -> Option<bool> {
    for node in nodes {
        match node {
            StructuredNode::Stmt(idx) => match &body[*idx].op {
                SemOp::MutexLock { resource } => {
                    if held.insert(*resource, LockMode::Mutex).is_some() {
                        return None;
                    }
                }
                SemOp::MutexUnlock { resource } => {
                    if held.remove(resource) != Some(LockMode::Mutex) {
                        return None;
                    }
                }
                SemOp::RwLockRead { resource } => {
                    if held.insert(*resource, LockMode::RwRead).is_some() {
                        return None;
                    }
                }
                SemOp::RwLockWrite { resource } => {
                    if held.insert(*resource, LockMode::RwWrite).is_some() {
                        return None;
                    }
                }
                SemOp::RwLockUnlock { resource } => {
                    if !matches!(
                        held.remove(resource),
                        Some(LockMode::RwRead | LockMode::RwWrite)
                    ) {
                        return None;
                    }
                }
                SemOp::CondvarWait { lock, .. } => {
                    if held.get(lock) != Some(&LockMode::Mutex) {
                        return None;
                    }
                }
                SemOp::Return { .. } => return Some(true),
                _ => {}
            },
            StructuredNode::While { body: inner, .. } => {
                let before = held.clone();
                let div = check_lexical_locks_inner(body, inner, held)?;
                if !div && *held != before {
                    return None;
                }
                *held = before;
            }
            StructuredNode::IfElse {
                then_nodes,
                else_nodes,
                ..
            } => {
                let before = held.clone();
                let mut then_held = before.clone();
                let mut else_held = before.clone();
                let then_div = check_lexical_locks_inner(body, then_nodes, &mut then_held)?;
                let else_div = check_lexical_locks_inner(body, else_nodes, &mut else_held)?;
                // Any lock acquired inside a branch must be unlocked inside that branch unless diverging.
                if !then_div && then_held != before {
                    return None;
                }
                if !else_div && else_held != before {
                    return None;
                }
                if then_div && else_div {
                    return Some(true);
                }
                *held = before;
            }
        }
    }
    Some(false)
}

fn try_structure_range(
    body: &[concir_sem::program::SemStmt],
    start: usize,
    end: usize,
    consumed: &mut std::collections::BTreeSet<(usize, usize)>,
) -> Option<Vec<StructuredNode>> {
    let mut out = Vec::new();
    let mut i = start;
    while i < end {
        match &body[i].op {
            SemOp::Branch {
                then, else_target, ..
            } => {
                if *then != i + 1 || *else_target <= i + 1 || *else_target > end {
                    return None;
                }
                // Check natural while loop: `stmt[else_target - 1]` is `Goto { target: i }`.
                if let SemOp::Goto { target } = &body[*else_target - 1].op
                    && *target == i
                {
                    consumed.insert((i, *then));
                    consumed.insert((i, *else_target));
                    consumed.insert((*else_target - 1, i));
                    let inner = try_structure_range(body, i + 1, *else_target - 1, consumed)?;
                    out.push(StructuredNode::While {
                        branch_idx: i,
                        body: inner,
                        back_goto_idx: *else_target - 1,
                    });
                    i = *else_target;
                    continue;
                }
                // Check if-else diamond: `stmt[else_target - 1]` is forward `Goto { target: merge }`.
                if *else_target >= i + 2
                    && let SemOp::Goto { target: merge } = &body[*else_target - 1].op
                    && *merge >= *else_target
                    && *merge <= end
                {
                    consumed.insert((i, *then));
                    consumed.insert((i, *else_target));
                    consumed.insert((*else_target - 1, *merge));
                    let then_nodes = try_structure_range(body, i + 1, *else_target - 1, consumed)?;
                    let else_nodes = try_structure_range(body, *else_target, *merge, consumed)?;
                    out.push(StructuredNode::IfElse {
                        branch_idx: i,
                        then_nodes,
                        then_goto_idx: Some(*else_target - 1),
                        else_nodes,
                    });
                    i = *merge;
                    continue;
                }
                // One-armed `if` (`then` falls through to `else_target` or returns).
                consumed.insert((i, *then));
                consumed.insert((i, *else_target));
                let then_nodes = try_structure_range(body, i + 1, *else_target, consumed)?;
                out.push(StructuredNode::IfElse {
                    branch_idx: i,
                    then_nodes,
                    then_goto_idx: None,
                    else_nodes: Vec::new(),
                });
                i = *else_target;
            }
            SemOp::Goto { .. } | SemOp::Switch { .. } | SemOp::Unsupported { .. } => {
                return None;
            }
            _ => {
                out.push(StructuredNode::Stmt(i));
                i += 1;
            }
        }
    }
    Some(out)
}

pub(crate) fn generate_function(g: &mut Gen, func: &concir_sem::program::SemFunction) -> Result<(), String> {
    if let Some(nodes) = try_structure_function(func) {
        return generate_structured_function(g, func, &nodes);
    }
    generate_pc_function(g, func)
}

fn generate_structured_function(
    g: &mut Gen,
    func: &concir_sem::program::SemFunction,
    nodes: &[StructuredNode],
) -> Result<(), String> {
    let name = format!("cf_{}", sanitize(&func.name));
    g.emit(format!(
        "    pub(crate) fn {name}(#[allow(unused_variables)] shared: std::sync::Arc<crate::Shared>, #[allow(unused_variables)] tag: &str) {{"
    ));
    for (i, slot) in func.slots.iter().enumerate() {
        let ty = g.resolve_rust_ty(func.module, Some(&slot.raw_ty), &slot.ty, false)?;
        let init = match &slot.init {
            Some(v) => g.render_value_typed(v, func.module, Some(&slot.raw_ty), &slot.ty)?,
            None => default_expr(&ty),
        };
        g.emit(format!(
            "        #[allow(unused_mut, unused_variables)] let mut _l{i}: {ty} = {init};"
        ));
    }
    let mut rw_modes: BTreeMap<ResourceId, LockMode> = BTreeMap::new();
    emit_structured_nodes(g, func, nodes, "        ", &mut rw_modes)?;
    g.emit("    }");
    g.emit("");
    Ok(())
}

fn record_sid_line(g: &mut Gen, func: &concir_sem::program::SemFunction, sid: &str, indent: &str) {
    let loc = g.line();
    g.sids.insert(
        format!("{}::{}", g.program.module_name(func.module), sid),
        SidLocation {
            file: g.current_file.clone(),
            line: loc,
        },
    );
    g.emit(format!("{indent}// @cir {sid}"));
}

fn emit_structured_nodes(
    g: &mut Gen,
    func: &concir_sem::program::SemFunction,
    nodes: &[StructuredNode],
    indent: &str,
    rw_modes: &mut BTreeMap<ResourceId, LockMode>,
) -> Result<(), String> {
    let mod_name = g.program.module_name(func.module).to_string();
    for node in nodes {
        match node {
            StructuredNode::While {
                branch_idx,
                body,
                back_goto_idx,
            } => {
                let br_stmt = &func.body[*branch_idx];
                let SemOp::Branch { cond, .. } = &br_stmt.op else {
                    unreachable!();
                };
                record_sid_line(g, func, &br_stmt.sid, indent);
                let c = g.render_expr(&mod_name, &func.name, &br_stmt.sid, cond);
                g.emit(format!("{indent}while {c} {{"));
                let inner_indent = format!("{indent}    ");
                let mut inner_modes = rw_modes.clone();
                emit_structured_nodes(g, func, body, &inner_indent, &mut inner_modes)?;
                let back_sid = &func.body[*back_goto_idx].sid;
                record_sid_line(g, func, back_sid, &inner_indent);
                g.emit(format!("{indent}}}"));
            }
            StructuredNode::IfElse {
                branch_idx,
                then_nodes,
                then_goto_idx,
                else_nodes,
            } => {
                let br_stmt = &func.body[*branch_idx];
                let SemOp::Branch { cond, .. } = &br_stmt.op else {
                    unreachable!();
                };
                record_sid_line(g, func, &br_stmt.sid, indent);
                let c = g.render_expr(&mod_name, &func.name, &br_stmt.sid, cond);
                let inner_indent = format!("{indent}    ");
                g.emit(format!("{indent}if {c} {{"));
                let mut then_modes = rw_modes.clone();
                emit_structured_nodes(g, func, then_nodes, &inner_indent, &mut then_modes)?;
                if let Some(goto_idx) = then_goto_idx {
                    let goto_sid = &func.body[*goto_idx].sid;
                    record_sid_line(g, func, goto_sid, &inner_indent);
                }
                if else_nodes.is_empty() {
                    g.emit(format!("{indent}}}"));
                } else {
                    g.emit(format!("{indent}}} else {{"));
                    let mut else_modes = rw_modes.clone();
                    emit_structured_nodes(g, func, else_nodes, &inner_indent, &mut else_modes)?;
                    g.emit(format!("{indent}}}"));
                }
            }
            StructuredNode::Stmt(idx) => {
                let stmt = &func.body[*idx];
                let sid = &stmt.sid;
                record_sid_line(g, func, sid, indent);
                emit_structured_stmt(g, func, &stmt.op, sid, indent, rw_modes)?;
            }
        }
    }
    Ok(())
}

fn emit_structured_stmt(
    g: &mut Gen,
    func: &concir_sem::program::SemFunction,
    op: &SemOp,
    sid: &str,
    indent: &str,
    rw_modes: &mut BTreeMap<ResourceId, LockMode>,
) -> Result<(), String> {
    let mod_name = g.program.module_name(func.module).to_string();
    match op {
        SemOp::Nop => {}
        SemOp::AssignLocal { target, expr } => {
            let e = g.render_expr(&mod_name, &func.name, sid, expr);
            match target {
                SlotRef::Local(i) => {
                    g.emit(format!("{indent}_l{i} = {e};"));
                }
                SlotRef::Discard => {}
                SlotRef::Shared(r) => {
                    let w = g.render_shared_write(*r, &e);
                    g.emit(format!("{indent}{w}"));
                }
            }
        }
        SemOp::ReadShared { resource, dst } => {
            let rd = g.render_shared_read(*resource);
            match dst {
                Some(SlotRef::Local(i)) => {
                    g.emit(format!("{indent}_l{i} = {rd};"));
                }
                Some(SlotRef::Shared(r)) => {
                    let w = g.render_shared_write(*r, "__v");
                    g.emit(format!("{indent}{{ let __v = {rd}; {w} }}"));
                }
                _ => {
                    g.emit(format!("{indent}let _ = {rd};"));
                }
            }
        }
        SemOp::WriteShared { resource, expr } => {
            let e = g.render_expr(&mod_name, &func.name, sid, expr);
            let w = g.render_shared_write(*resource, "__v");
            g.emit(format!("{indent}{{ let __v = {e}; {w} }}"));
        }
        SemOp::MutexLock { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            let payload = g.resource_payload_ty(*resource)?;
            g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
            g.emit(format!(
                "{indent}#[allow(unused_mut, unused_variables)] let mut _g_{f} = crate::cir_trace::lock(&shared.{f}, tag, {rq:?}, {sid:?});"
            ));
            if payload == "()" {
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut _g_{f} = crate::__res::{f}.lock().unwrap();"
                ));
            } else {
                let def_val = default_expr(&payload);
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut __spn_g_{f} = crate::__res::{f}.lock().unwrap();"
                ));
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut __payload_g_{f}: {payload} = {def_val};"
                ));
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut _g_{f}: &mut {payload} = &mut __payload_g_{f};"
                ));
            }
        }
        SemOp::MutexUnlock { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            let payload = g.resource_payload_ty(*resource)?;
            g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
            g.emit(format!(
                "{indent}crate::cir_trace::unlock(Some(_g_{f}), tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
            if payload == "()" {
                g.emit(format!("{indent}drop(_g_{f});"));
            } else {
                g.emit(format!(
                    "{indent}{{ let _ = _g_{f}; drop(__payload_g_{f}); drop(__spn_g_{f}); }}"
                ));
            }
        }
        SemOp::RwLockRead { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            let payload = g.resource_payload_ty(*resource)?;
            rw_modes.insert(*resource, LockMode::RwRead);
            g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
            g.emit(format!(
                "{indent}#[allow(unused_mut, unused_variables)] let mut _rg_{f} = crate::cir_trace::rwlock_read_lock(&shared.{f}, tag, {rq:?}, {sid:?});"
            ));
            if payload == "()" {
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut _rg_{f} = crate::__res::{f}.read().unwrap();"
                ));
            } else {
                let def_val = default_expr(&payload);
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut __spn_rg_{f} = crate::__res::{f}.read().unwrap();"
                ));
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_variables)] let __payload_rg_{f}: {payload} = {def_val};"
                ));
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_variables)] let _rg_{f}: &{payload} = &__payload_rg_{f};"
                ));
            }
        }
        SemOp::RwLockWrite { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            let payload = g.resource_payload_ty(*resource)?;
            rw_modes.insert(*resource, LockMode::RwWrite);
            g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
            g.emit(format!(
                "{indent}#[allow(unused_mut, unused_variables)] let mut _wg_{f} = crate::cir_trace::rwlock_write_lock(&shared.{f}, tag, {rq:?}, {sid:?});"
            ));
            if payload == "()" {
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut _wg_{f} = crate::__res::{f}.write().unwrap();"
                ));
            } else {
                let def_val = default_expr(&payload);
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut __spn_wg_{f} = crate::__res::{f}.write().unwrap();"
                ));
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut __payload_wg_{f}: {payload} = {def_val};"
                ));
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut _wg_{f}: &mut {payload} = &mut __payload_wg_{f};"
                ));
            }
        }
        SemOp::RwLockUnlock { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            let payload = g.resource_payload_ty(*resource)?;
            match rw_modes.remove(resource) {
                Some(LockMode::RwWrite) => {
                    g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
                    g.emit(format!(
                        "{indent}crate::cir_trace::rwlock_write_unlock(Some(_wg_{f}), tag, {rq:?}, {sid:?});"
                    ));
                    g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                    if payload == "()" {
                        g.emit(format!("{indent}drop(_wg_{f});"));
                    } else {
                        g.emit(format!(
                            "{indent}{{ let _ = _wg_{f}; drop(__payload_wg_{f}); drop(__spn_wg_{f}); }}"
                        ));
                    }
                }
                _ => {
                    g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
                    g.emit(format!(
                        "{indent}crate::cir_trace::rwlock_read_unlock(Some(_rg_{f}), tag, {rq:?}, {sid:?});"
                    ));
                    g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                    if payload == "()" {
                        g.emit(format!("{indent}drop(_rg_{f});"));
                    } else {
                        g.emit(format!(
                            "{indent}{{ let _ = _rg_{f}; drop(__payload_rg_{f}); drop(__spn_rg_{f}); }}"
                        ));
                    }
                }
            }
        }
        SemOp::CondvarWait { condvar, lock } => {
            let c = g.field(*condvar);
            let l = g.field(*lock);
            let rq = g.resource_fqn(*condvar);
            let lock_payload = g.resource_payload_ty(*lock)?;
            g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
            g.emit(format!(
                "{indent}{{ _g_{l} = crate::cir_trace::condvar_wait(&shared.{c}, _g_{l}, tag, {rq:?}, {sid:?}); }}"
            ));
            g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
            if lock_payload == "()" {
                g.emit(format!("{indent}{{ _g_{l} = crate::__res::{c}.wait(_g_{l}).unwrap(); }}"));
            } else {
                g.emit(format!(
                    "{indent}{{ __spn_g_{l} = crate::__res::{c}.wait(__spn_g_{l}).unwrap(); }}"
                ));
            }
        }
        SemOp::CondvarNotify { condvar } => {
            let c = g.field(*condvar);
            let rq = g.resource_fqn(*condvar);
            g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
            g.emit(format!(
                "{indent}crate::cir_trace::notify_one(&shared.{c}, tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
            g.emit(format!("{indent}crate::__res::{c}.notify_one();"));
        }
        SemOp::CondvarNotifyAll { condvar } => {
            let c = g.field(*condvar);
            let rq = g.resource_fqn(*condvar);
            g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
            g.emit(format!(
                "{indent}crate::cir_trace::notify_all(&shared.{c}, tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
            g.emit(format!("{indent}crate::__res::{c}.notify_all();"));
        }
        SemOp::SemaphoreAcquire { resource, count } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "{indent}shared.{f}.acquire_sid({count}, tag, {rq:?}, {sid:?});"
            ));
        }
        SemOp::SemaphoreRelease { resource, count } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "{indent}shared.{f}.release_sid({count}, tag, {rq:?}, {sid:?});"
            ));
        }
        SemOp::Return { value } => {
            if let Some(e) = value {
                let _ = g.render_expr(&mod_name, &func.name, sid, e);
            }
            g.emit(format!("{indent}return;"));
        }
        SemOp::SeqHole {
            id,
            reads,
            writes,
            updates_modeled,
            expected_type,
            req,
            ..
        } => {
            let expected_ty = crate::emit_stmt::expected_rust_ty(g, func.module, expected_type);
            let hole_line = g.line();
            g.holes.push(Hole {
                id: id.clone(),
                module: mod_name.clone(),
                function: func.name.clone(),
                sid: sid.to_string(),
                expected: expected_ty.clone(),
                file: g.current_file.clone(),
                line: hole_line,
                reads: reads.clone(),
                writes: writes.clone(),
                req: req.clone(),
            });
            g.emit(format!(
                "{indent}let _hole_val: {expected_ty} = /* HOLE({id}) expected: {expected_ty} */ Default::default();"
            ));
            if !updates_modeled.is_empty() {
                g.emit(format!("{indent}{{"));
                for (ui, u) in updates_modeled.iter().enumerate() {
                    let rhs = g.render_expr(&mod_name, &func.name, sid, &u.expr);
                    g.emit(format!("{indent}    let __u{ui} = {rhs};"));
                }
                for (ui, u) in updates_modeled.iter().enumerate() {
                    match u.target {
                        SlotRef::Local(i) => {
                            g.emit(format!("{indent}    _l{i} = __u{ui};"));
                        }
                        SlotRef::Shared(r) => {
                            let w = g.render_shared_write(r, &format!("__u{ui}"));
                            g.emit(format!("{indent}    {w}"));
                        }
                        SlotRef::Discard => {}
                    }
                }
                g.emit(format!("{indent}}}"));
            }
        }
        SemOp::Scope { funcs } => {
            let tags = child_tags(func, sid);
            g.tags.push(TagRecord {
                sid: sid.to_string(),
                function: func.name.clone(),
                tags: tags.clone(),
            });
            g.emit(format!("{indent}crate::cir_trace::scope(tag, {sid:?});"));
            g.emit(format!("{indent}{{"));
            for (i, f) in funcs.iter().enumerate() {
                let cf = g.fn_ref(*f);
                let t = tags
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("t{sid}_{}", i + 1));
                g.emit(format!(
                    "{indent}    let __h{i} = {{ let __sh = std::sync::Arc::clone(&shared); std::thread::spawn(move || {cf}(__sh, {t:?})) }};"
                ));
            }
            for i in 0..funcs.len() {
                g.emit(format!("{indent}    __h{i}.join().unwrap();"));
            }
            g.emit(format!("{indent}}}"));
        }
        SemOp::Spawn {
            func: child,
            handle,
        } => {
            let tags = child_tags(func, sid);
            g.tags.push(TagRecord {
                sid: sid.to_string(),
                function: func.name.clone(),
                tags: tags.clone(),
            });
            let cf = g.fn_ref(*child);
            let t = tags.first().cloned().unwrap_or_else(|| format!("t{sid}"));
            let h = format!("_h_{}", sanitize(handle));
            let child_fqn = g.function_fqn(*child);
            g.emit(format!(
                "{indent}crate::cir_trace::record(tag, \"spawn\", {child_fqn:?}, {sid:?});"
            ));
            g.emit(format!(
                "{indent}let {h} = {{ let __sh = std::sync::Arc::clone(&shared); std::thread::spawn(move || {cf}(__sh, {t:?})) }};"
            ));
        }
        SemOp::Join { handle } => {
            let h = format!("_h_{}", sanitize(handle));
            g.emit(format!(
                "{indent}crate::cir_trace::record(tag, \"join\", \"\", {sid:?});"
            ));
            g.emit(format!("{indent}{h}.join().unwrap();"));
        }
        SemOp::Call {
            func: callee,
            args,
            dst,
        } => {
            let cf = g.fn_ref(*callee);
            for a in args {
                let _ = g.render_expr(&mod_name, &func.name, sid, a);
            }
            if dst.is_some() {
                g.emit(format!("{indent}let _ = {cf}(shared.clone(), tag);"));
            } else {
                g.emit(format!("{indent}{cf}(shared.clone(), tag);"));
            }
        }
        SemOp::AtomicLoad { resource, dst } => {
            let rd = g.render_shared_read(*resource);
            match dst {
                SlotRef::Local(i) => {
                    g.emit(format!("{indent}_l{i} = {rd};"));
                }
                SlotRef::Shared(r) => {
                    let w = g.render_shared_write(*r, "__v");
                    g.emit(format!("{indent}{{ let __v = {rd}; {w} }}"));
                }
                SlotRef::Discard => {
                    g.emit(format!("{indent}let _ = {rd};"));
                }
            }
        }
        SemOp::AtomicStore { resource, value } => {
            let e = g.render_expr(&mod_name, &func.name, sid, value);
            let w = g.render_shared_write(*resource, "__v");
            g.emit(format!("{indent}{{ let __v = {e}; {w} }}"));
        }
        SemOp::AtomicCas {
            resource,
            expected,
            desired,
            dst,
        } => {
            let f = g.field(*resource);
            let ex = g.render_expr(&mod_name, &func.name, sid, expected);
            let de = g.render_expr(&mod_name, &func.name, sid, desired);
            let assign = match dst {
                SlotRef::Local(i) => format!("_l{i} = __old;"),
                SlotRef::Shared(r) => g.render_shared_write(*r, "__old"),
                SlotRef::Discard => "let _ = __old;".to_string(),
            };
            if g.is_std_atomic(*resource) {
                g.emit(format!(
                    "{indent}{{ let __old = match shared.{f}.compare_exchange({ex}, {de}, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst) {{ Ok(v) | Err(v) => v }}; {assign} }}"
                ));
            } else {
                g.emit(format!(
                    "{indent}{{ let mut __g = shared.{f}.lock().unwrap(); let __old = __g.clone(); if __old == {ex} {{ *__g = {de}; }} drop(__g); {assign} }}"
                ));
            }
        }
        SemOp::ChannelSend { channel, value } => {
            let f = g.field(*channel);
            let rq = g.resource_fqn(*channel);
            let e = g.render_expr(&mod_name, &func.name, sid, value);
            g.emit(format!(
                "{indent}shared.{f}.send_sid({e}, tag, {rq:?}, {sid:?});"
            ));
        }
        SemOp::ChannelRecv { channel, dst } => {
            let f = g.field(*channel);
            let rq = g.resource_fqn(*channel);
            match dst {
                SlotRef::Local(i) => {
                    g.emit(format!(
                        "{indent}_l{i} = shared.{f}.recv_sid(tag, {rq:?}, {sid:?});"
                    ));
                }
                SlotRef::Shared(r) => {
                    let w = g.render_shared_write(*r, "__v");
                    g.emit(format!(
                        "{indent}{{ let __v = shared.{f}.recv_sid(tag, {rq:?}, {sid:?}); {w} }}"
                    ));
                }
                SlotRef::Discard => {
                    g.emit(format!(
                        "{indent}let _ = shared.{f}.recv_sid(tag, {rq:?}, {sid:?});"
                    ));
                }
            }
        }
        SemOp::Goto { .. } | SemOp::Branch { .. } | SemOp::Switch { .. } => {
            return Err("unexpected unstructured control op in structured emitter".into());
        }
        SemOp::Unsupported { construct } => {
            return Err(format!("unsupported construct in codegen: {construct}"));
        }
    }
    Ok(())
}

fn generate_pc_function(g: &mut Gen, func: &concir_sem::program::SemFunction) -> Result<(), String> {
    let name = format!("cf_{}", sanitize(&func.name));
    g.emit(format!("    pub(crate) fn {name}(shared: std::sync::Arc<crate::Shared>, tag: &str) {{"));
    // slot declarations
    for (i, slot) in func.slots.iter().enumerate() {
        let ty = g.resolve_rust_ty(func.module, Some(&slot.raw_ty), &slot.ty, false)?;
        let init = match &slot.init {
            Some(v) => g.render_value_typed(v, func.module, Some(&slot.raw_ty), &slot.ty)?,
            None => default_expr(&ty),
        };
        g.emit(format!(
            "    #[allow(unused_mut)] let mut _l{i}: {ty} = {init};"
        ));
    }
    // guard declarations for every mutex / rwlock
    let mut guards = Vec::new();
    let res_list: Vec<_> = g.program.resources().to_vec();
    for r in &res_list {
        match r.kind {
            ResKind::Mutex => {
                let f = g.field(r.id);
                let payload = g.resource_payload_ty(r.id)?;
                g.emit(format!(
                    "    #[allow(unused_mut)] let mut _g_{f}: Option<std::sync::MutexGuard<'_, {payload}>> = None;"
                ));
                guards.push(f);
            }
            ResKind::RwLock => {
                let f = g.field(r.id);
                let payload = g.resource_payload_ty(r.id)?;
                g.emit(format!(
                    "    #[allow(unused_mut)] let mut _rg_{f}: Option<std::sync::RwLockReadGuard<'_, {payload}>> = None;"
                ));
                g.emit(format!(
                    "    #[allow(unused_mut)] let mut _wg_{f}: Option<std::sync::RwLockWriteGuard<'_, {payload}>> = None;"
                ));
                guards.push(f);
            }
            _ => {}
        }
    }
    g.emit("    let mut pc: usize = 0;");
    g.emit("    loop {");
    g.emit("        match pc {");

    let body_len = func.body.len();
    for (idx, stmt) in func.body.iter().enumerate() {
        let sid = stmt.sid.clone();
        let loc = g.line();
        g.sids.insert(
            format!("{}::{}", g.program.module_name(func.module), sid),
            SidLocation {
                file: g.current_file.clone(),
                line: loc,
            },
        );
        g.emit(format!("            {idx} => {{"));
        g.emit(format!("                // @cir {sid}"));
        crate::emit_stmt::render_op(g, func, &stmt.op, idx, body_len, &sid, &guards)?;
        g.emit("            }");
    }
    g.emit("            _ => return,");
    g.emit("        }");
    g.emit("    }");
    g.emit("}");
    g.emit("");
    Ok(())
}

