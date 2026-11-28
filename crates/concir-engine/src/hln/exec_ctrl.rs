use std::collections::{BTreeMap, BTreeSet, VecDeque};

use unipn::hln::{
    state_key as hln_state_key, ControlToken, HlnFrame, HlnRetAddr, HlnScope, HlnState, HlnVal,
    LockHoldMode, RwState, WaitTag,
};
use unipn::net::PlaceId;

use concir_ir::fqn;
use concir_sem::eval::{eval, LExpr, ValueStore};
use concir_sem::ids::{FrameId, FunctionId, ResourceId, SlotId, SlotRef, ThreadId};
use concir_sem::monitor::MonitorConfig;
use concir_sem::outcome::{
    location_of, AnalysisBounds, BackendError, BackendResult, BoundaryEvent, BoundaryKind, Phase,
    StepLabel, TransitionOrigin,
};
use concir_sem::program::{ResKind, SemOp, SemProgram, SlotClass};
use concir_sem::system::{
    compare_values, BlockKind, BlockedRecord, DoomState, DoomThread, DoomWait, Enabled,
    InstanceState, Predicate, Step, TransitionSystem,
};
use concir_sem::value::{default_value, within_type, Value};

use super::lower::{build, LoweredHlnNet, PlaceKey};


use super::state_ops::{hln_to_val, ret_dst, val_to_hln};
use super::UnipnEngine;

impl<'a> UnipnEngine<'a> {
    pub(super) fn create_thread(
        &self,
        state: &mut HlnState,
        func: FunctionId,
        parent_scope: Option<u32>,
        boundary: &mut Vec<BoundaryEvent>,
    ) -> BackendResult<Option<u32>> {
        if self.live_thread_count(state) >= self.bounds.max_threads {
            boundary.push(BoundaryEvent::new(
                BoundaryKind::ThreadLimit,
                format!(
                    "thread limit {} reached while spawning",
                    self.bounds.max_threads
                ),
            ));
            return Ok(None);
        }
        let tid = state.alloc.next_thread;
        state.alloc.next_thread += 1;
        let f = self.program.function(func);
        let frame = self.alloc_frame(state, func, None);
        let tok = ControlToken {
            tid,
            entry_func: func.0 as usize,
            stack: vec![frame],
            handle_children: BTreeMap::new(),
            held_locks: BTreeMap::new(),
            parent_scope,
            wait_tag: None,
        };
        let start_pid = self
            .net
            .pc_place(func, 0)
            .unwrap_or(self.net.func_ends[func.0 as usize]);
        state.put_control_token(start_pid, tok);
        if f.is_transparent_nobody() {
            self.finish_thread(state, tid, func)?;
        }
        Ok(Some(tid))
    }

    pub(super) fn finish_thread(
        &self,
        state: &mut HlnState,
        tid: u32,
        func: FunctionId,
    ) -> BackendResult<()> {
        let Some((cur_pid, mut tok)) = state.find_thread(tid).map(|(p, t)| (p, t.clone())) else {
            return Ok(());
        };
        state.take_control_token(cur_pid, tid);
        tok.stack.clear();
        tok.wait_tag = None;
        let end_pid = self.net.func_ends[func.0 as usize];
        state.put_control_token(end_pid, tok.clone());
        state.finished.insert(tid);
        self.record_completion(state, func);

        if let Some(scope_id) = tok.parent_scope {
            let empty = match state.scopes.get_mut(&scope_id) {
                Some(sc) => {
                    sc.remaining.remove(&tid);
                    sc.remaining.is_empty()
                }
                None => false,
            };
            if empty {
                let sc = state.scopes.remove(&scope_id).unwrap();
                state
                    .completed_scopes
                    .insert((sc.owner_func, sc.owner_sid));
                if let Some((_, owner_tok)) =
                    state.find_thread(sc.owner).map(|(p, t)| (p, t.clone()))
                {
                    let next_pc = sc.owner_sid + 1;
                    self.advance_token(state, owner_tok, next_pc)?;
                }
                let members: Vec<(PlaceId, u32)> = state
                    .all_tokens()
                    .filter(|(_, t)| t.parent_scope == Some(scope_id))
                    .map(|(p, t)| (p, t.tid))
                    .collect();
                for (mp, mtid) in members {
                    state.take_control_token(mp, mtid);
                    state.finished.remove(&mtid);
                }
            }
        }

        let joiners: Vec<(u32, u32)> = state
            .all_tokens()
            .filter_map(|(_, t)| match t.wait_tag {
                Some(WaitTag::Join(hid)) if t.handle_children.get(&hid) == Some(&tid) => {
                    Some((t.tid, hid))
                }
                _ => None,
            })
            .collect();
        let had_joiner = !joiners.is_empty();
        for (jtid, _hid) in joiners {
            if let Some((_, mut jtok)) = state.find_thread(jtid).map(|(p, t)| (p, t.clone())) {
                jtok.handle_children.retain(|_, c| *c != tid);
                let pc_next = if let Some(fr) = jtok.current_frame_mut() {
                    let jf = FunctionId(fr.func as u32);
                    let jpc = fr.pc;
                    if let Some(SemOp::Join { handle }) =
                        self.program.function(jf).body.get(jpc).map(|s| &s.op)
                    {
                        fr.handles.remove(handle);
                    }
                    jpc + 1
                } else {
                    0
                };
                self.advance_token(state, jtok, pc_next)?;
            }
        }
        if had_joiner {
            if let Some((ep, _)) = state.find_thread(tid) {
                state.take_control_token(ep, tid);
            }
            state.finished.remove(&tid);
        }
        Ok(())
    }

    pub(super) fn apply_return(
        &self,
        next: &mut HlnState,
        tok: ControlToken,
        val: Option<Value>,
    ) -> BackendResult<bool> {
        let Some(cur_frame) = tok.current_frame().cloned() else {
            return Ok(false);
        };
        let func = FunctionId(cur_frame.func as u32);
        let f_def = self.program.function(func);
        if let (Some(ret), Some(v)) = (&f_def.returns, &val)
            && !within_type(v, &ret.ty)
        {
            return Ok(false);
        }

        if tok.stack.len() > 1 {
            let caller = &tok.stack[tok.stack.len() - 2];
            let caller_func = FunctionId(caller.func as u32);
            let dst = ret_dst(&cur_frame.ret);
            if let Some(v) = &val
                && !self.dst_type_ok(caller_func, dst, v)
            {
                return Ok(false);
            }
            let mut updated_tok = tok;
            let popped = updated_tok.stack.pop().unwrap();
            let ret_addr = popped.ret.unwrap();
            if let Some(v) = val {
                let hval = val_to_hln(&v);
                if let Some(local) = ret_addr.dst_local {
                    if let Some(cf) = updated_tok.current_frame_mut() {
                        cf.locals.insert(local, hval);
                    }
                } else if let Some(shared) = ret_addr.dst_shared {
                    next.vars.insert(shared, hval);
                }
            }
            let next_pc = ret_addr.pc_next;
            self.advance_token(next, updated_tok, next_pc)?;
            self.record_completion(next, func);
        } else {
            self.finish_thread(next, tok.tid, func)?;
        }
        Ok(true)
    }

    pub(super) fn step_runnable_token(
        &self,
        state: &HlnState,
        tok: &ControlToken,
        boundary: &mut Vec<BoundaryEvent>,
    ) -> BackendResult<Vec<Step<HlnState>>> {
        let Some(frame) = tok.current_frame() else {
            return Ok(Vec::new());
        };
        let func = FunctionId(frame.func as u32);
        let pc = frame.pc;
        let f_def = self.program.function(func);

        // Implicit return at function end.
        if pc >= f_def.body.len() {
            if tok.stack.len() > 1 || f_def.returns.is_none() {
                let mut next = state.clone();
                let label = self.step_label(func, None, Phase::Statement, tok.tid, frame.frame_id);
                if self.apply_return(&mut next, tok.clone(), None)? {
                    return Ok(vec![Step { label, state: next }]);
                }
            }
            return Ok(Vec::new());
        }

        let stmt = f_def.body[pc].clone();
        let at = location_of(self.program, func, Some(pc));
        let label = self.step_label(func, Some(pc), Phase::Statement, tok.tid, frame.frame_id);

        let mut next = state.clone();
        next.reached.insert((func.0 as usize, pc));

        match &stmt.op {
            SemOp::Nop => {
                self.advance_token(&mut next, tok.clone(), pc + 1)?;
            }
            SemOp::AssignLocal { target, expr } => {
                let v = self.eval(&next, frame, expr, &at)?;
                if !self.try_write_dst(&mut next, tok.tid, func, *target, v) {
                    return Ok(Vec::new());
                }
                let (_, fresh) = next.find_thread(tok.tid).unwrap();
                let fresh = fresh.clone();
                self.advance_token(&mut next, fresh, pc + 1)?;
            }
            SemOp::ReadShared { resource, dst } => {
                let v = next
                    .vars
                    .get(&(resource.0 as usize))
                    .map(hln_to_val)
                    .ok_or_else(|| {
                        BackendError::invalid("E900", format!("shared resource {resource} missing"))
                    })?;
                if let Some(d) = *dst
                    && !self.try_write_dst(&mut next, tok.tid, func, d, v)
                {
                    return Ok(Vec::new());
                }
                let (_, fresh) = next.find_thread(tok.tid).unwrap();
                let fresh = fresh.clone();
                self.advance_token(&mut next, fresh, pc + 1)?;
            }
            SemOp::WriteShared { resource, expr } => {
                let v = self.eval(&next, frame, expr, &at)?;
                let ty = self.program.resource(*resource).ty.as_ref().unwrap();
                if !within_type(&v, ty) {
                    return Ok(Vec::new());
                }
                next.vars.insert(resource.0 as usize, val_to_hln(&v));
                self.advance_token(&mut next, tok.clone(), pc + 1)?;
            }
            SemOp::AtomicLoad { resource, dst } => {
                let v = next
                    .vars
                    .get(&(resource.0 as usize))
                    .map(hln_to_val)
                    .ok_or_else(|| {
                        BackendError::invalid("E900", format!("Atomic {resource} has no value"))
                    })?;
                if !self.try_write_dst(&mut next, tok.tid, func, *dst, v) {
                    return Ok(Vec::new());
                }
                let (_, fresh) = next.find_thread(tok.tid).unwrap();
                let fresh = fresh.clone();
                self.advance_token(&mut next, fresh, pc + 1)?;
            }
            SemOp::AtomicStore { resource, value } => {
                let v = self.eval(&next, frame, value, &at)?;
                let ty = self.program.resource(*resource).ty.as_ref().unwrap();
                if !within_type(&v, ty) {
                    return Ok(Vec::new());
                }
                next.vars.insert(resource.0 as usize, val_to_hln(&v));
                self.advance_token(&mut next, tok.clone(), pc + 1)?;
            }
            SemOp::AtomicCas {
                resource,
                expected,
                desired,
                dst,
            } => {
                let exp = self.eval(&next, frame, expected, &at)?;
                let des = self.eval(&next, frame, desired, &at)?;
                let ty = self.program.resource(*resource).ty.as_ref().unwrap();
                if !within_type(&des, ty) {
                    return Ok(Vec::new());
                }
                let old = next
                    .vars
                    .get(&(resource.0 as usize))
                    .map(hln_to_val)
                    .ok_or_else(|| {
                        BackendError::invalid("E900", format!("Atomic {resource} has no value"))
                    })?;
                if !self.try_write_dst(&mut next, tok.tid, func, *dst, old.clone()) {
                    return Ok(Vec::new());
                }
                if old == exp {
                    next.vars.insert(resource.0 as usize, val_to_hln(&des));
                }
                let (_, fresh) = next.find_thread(tok.tid).unwrap();
                let fresh = fresh.clone();
                self.advance_token(&mut next, fresh, pc + 1)?;
            }
            SemOp::MutexLock { .. }
            | SemOp::MutexUnlock { .. }
            | SemOp::RwLockRead { .. }
            | SemOp::RwLockWrite { .. }
            | SemOp::RwLockUnlock { .. }
            | SemOp::ChannelSend { .. }
            | SemOp::ChannelRecv { .. }
            | SemOp::CondvarWait { .. }
            | SemOp::CondvarNotify { .. }
            | SemOp::CondvarNotifyAll { .. }
            | SemOp::SemaphoreAcquire { .. }
            | SemOp::SemaphoreRelease { .. } => {
                return self.step_sync_op(
                    &mut next,
                    tok,
                    frame,
                    func,
                    pc,
                    &stmt,
                    &at,
                    label,
                );
            }
            SemOp::Call {
                func: callee_id,
                args,
                dst,
            } => {
                let callee = self.program.function(*callee_id);
                if callee.is_transparent_nobody() {
                    self.record_completion(&mut next, *callee_id);
                    self.advance_token(&mut next, tok.clone(), pc + 1)?;
                } else {
                    if tok.stack.len() >= self.bounds.max_frames_per_thread {
                        boundary.push(BoundaryEvent::new(
                            BoundaryKind::FrameLimit,
                            format!(
                                "frame limit {} reached while calling '{}'",
                                self.bounds.max_frames_per_thread, callee.name
                            ),
                        ));
                        return Ok(Vec::new());
                    }
                    let mut values = Vec::with_capacity(args.len());
                    for a in args {
                        values.push(self.eval(&next, frame, a, &at)?);
                    }
                    let modeled: Vec<usize> = callee
                        .slots
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.class == SlotClass::Param && s.modeled)
                        .map(|(i, _)| i)
                        .collect();
                    for (slot, val) in modeled.iter().zip(values.iter()) {
                        if !within_type(val, &callee.slots[*slot].ty) {
                            return Ok(Vec::new());
                        }
                    }
                    let ret_place = self
                        .net
                        .pc_place(func, pc + 1)
                        .unwrap_or(self.net.func_ends[func.0 as usize]);
                    let (dst_local, dst_shared) = match dst.unwrap_or(SlotRef::Discard) {
                        SlotRef::Discard => (None, None),
                        SlotRef::Local(l) => (Some(l), None),
                        SlotRef::Shared(r) => (None, Some(r.0 as usize)),
                    };
                    let mut cframe = self.alloc_frame(
                        &mut next,
                        *callee_id,
                        Some(HlnRetAddr {
                            return_place: ret_place,
                            pc_next: pc + 1,
                            dst_local,
                            dst_shared,
                        }),
                    );
                    for (slot, val) in modeled.iter().zip(values) {
                        cframe.locals.insert(*slot, val_to_hln(&val));
                    }
                    let mut utok = tok.clone();
                    utok.stack.push(cframe);
                    let callee_start = self
                        .net
                        .pc_place(*callee_id, 0)
                        .unwrap_or(self.net.func_ends[callee_id.0 as usize]);
                    self.move_thread_token(&mut next, utok.tid, callee_start, utok);
                }
            }
            SemOp::Spawn {
                func: child_fn,
                handle,
            } => {
                if !self.within_function_bound(&next, *child_fn) {
                    return Ok(Vec::new());
                }
                let Some(child_tid) = self.create_thread(&mut next, *child_fn, None, boundary)?
                else {
                    return Ok(Vec::new());
                };
                let hid = next.alloc.next_handle;
                next.alloc.next_handle += 1;
                let mut utok = tok.clone();
                if let Some(fr) = utok.current_frame_mut() {
                    fr.handles.insert(handle.clone(), hid);
                }
                utok.handle_children.insert(hid, child_tid);
                self.advance_token(&mut next, utok, pc + 1)?;
            }
            SemOp::Scope { funcs } => {
                if funcs.is_empty() {
                    return Err(BackendError::invalid("E410", "scope funcs is empty").at(&at));
                }
                for cf in funcs {
                    if !self.within_function_bound(&next, *cf) {
                        return Ok(Vec::new());
                    }
                }
                let scope_id = next.alloc.next_scope;
                next.alloc.next_scope += 1;
                let mut remaining = BTreeSet::new();
                for &cf in funcs {
                    let Some(child_tid) =
                        self.create_thread(&mut next, cf, Some(scope_id), boundary)?
                    else {
                        return Ok(Vec::new());
                    };
                    if !next.finished.contains(&child_tid) {
                        remaining.insert(child_tid);
                    }
                }
                if remaining.is_empty() {
                    next.completed_scopes.insert((func.0 as usize, pc));
                    self.advance_token(&mut next, tok.clone(), pc + 1)?;
                } else {
                    next.scopes.insert(
                        scope_id,
                        HlnScope {
                            id: scope_id,
                            owner: tok.tid,
                            owner_frame: frame.frame_id,
                            owner_func: func.0 as usize,
                            owner_sid: pc,
                            remaining,
                        },
                    );
                    let c_pid = self.net.place_of[&PlaceKey::Contend(func, pc)];
                    self.park_token(&mut next, tok.clone(), c_pid, WaitTag::Scope(scope_id));
                }
            }
            SemOp::Join { handle } => {
                let hid = frame.handles.get(handle).copied().ok_or_else(|| {
                    BackendError::invalid(
                        "E402",
                        format!("join handle '{handle}' was never spawned in this activation"),
                    )
                    .at(&at)
                })?;
                let child = tok.handle_children.get(&hid).copied();
                match child {
                    Some(c) if next.finished.contains(&c) => {
                        let mut utok = tok.clone();
                        if let Some(fr) = utok.current_frame_mut() {
                            fr.handles.remove(handle);
                        }
                        utok.handle_children.remove(&hid);
                        if let Some((ep, _)) = next.find_thread(c) {
                            next.take_control_token(ep, c);
                        }
                        next.finished.remove(&c);
                        self.advance_token(&mut next, utok, pc + 1)?;
                    }
                    Some(_) => {
                        let c_pid = self.net.place_of[&PlaceKey::Contend(func, pc)];
                        self.park_token(&mut next, tok.clone(), c_pid, WaitTag::Join(hid));
                    }
                    None => {
                        return Err(BackendError::invalid(
                            "E402",
                            format!("join handle '{handle}' has no child"),
                        )
                        .at(&at))
                    }
                }
            }
            SemOp::Goto { target } => {
                self.advance_token(&mut next, tok.clone(), *target)?;
            }
            SemOp::Branch {
                cond,
                then,
                else_target,
            } => {
                let v = self.eval(&next, frame, cond, &at)?;
                let take_then = v.as_bool().ok_or_else(|| {
                    BackendError::invalid("E201", "branch condition is not Bool").at(&at)
                })?;
                let target = if take_then { *then } else { *else_target };
                self.advance_token(&mut next, tok.clone(), target)?;
            }
            SemOp::Switch {
                var,
                cases,
                default,
            } => {
                let v = self.eval(&next, frame, var, &at)?;
                let target = match &v {
                    Value::Enum(e) => cases.get(e).copied().unwrap_or(*default),
                    Value::Int(i) => cases
                        .iter()
                        .find_map(|(k, t)| (k.parse::<i64>().ok() == Some(*i)).then_some(*t))
                        .unwrap_or(*default),
                    other => {
                        return Err(BackendError::invalid(
                            "E202",
                            format!("switch on non-Int/non-Enum value {}", other.canonical()),
                        )
                        .at(&at))
                    }
                };
                self.advance_token(&mut next, tok.clone(), target)?;
            }
            SemOp::Return { value } => {
                let val = match value {
                    Some(e) => Some(self.eval(&next, frame, e, &at)?),
                    None => None,
                };
                if !self.apply_return(&mut next, tok.clone(), val)? {
                    return Ok(Vec::new());
                }
            }
            SemOp::SeqHole {
                updates_modeled, ..
            } => {
                let mut evaluated = Vec::with_capacity(updates_modeled.len());
                for u in updates_modeled {
                    let v = self.eval(&next, frame, &u.expr, &at)?;
                    if !self.dst_type_ok(func, u.target, &v) {
                        return Ok(Vec::new());
                    }
                    evaluated.push((u.target, v));
                }
                for (target, v) in evaluated {
                    Self::write_dst_raw(&mut next, tok.tid, target, v);
                }
                let (_, fresh) = next.find_thread(tok.tid).unwrap();
                let fresh = fresh.clone();
                self.advance_token(&mut next, fresh, pc + 1)?;
            }
            SemOp::Unsupported { construct } => {
                return Err(BackendError::unsupported(
                    construct,
                    format!("reached unsupported statement at {at}"),
                )
                .at(&at))
            }
        }

        Ok(vec![Step { label, state: next }])
    }

}
