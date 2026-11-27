//! [`TransitionSystem`] implementation over UniPN [`HlnNet`] / [`HlnState`].
//!
//! All per-thread state (IDs, call stack, local slots, held locks, wait tags)
//! resides inside [`ControlToken`] values on the lowered single-subnet-per-function
//! [`LoweredHlnNet`]. Symmetry reduction is delegated directly to
//! [`unipn::hln::state_key`].

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use unipn::hln::{
    state_key as hln_state_key, ControlToken, HlnFrame, HlnRetAddr, HlnScope, HlnState, HlnVal,
    LockHoldMode, RwState, WaitTag,
};
use unipn::net::PlaceId;

use crate::fqn;
use crate::sem::eval::{eval, LExpr, ValueStore};
use crate::sem::ids::{FrameId, FunctionId, ResourceId, SlotId, SlotRef, ThreadId};
use crate::sem::monitor::MonitorConfig;
use crate::sem::outcome::{
    location_of, AnalysisBounds, BackendError, BackendResult, BoundaryEvent, BoundaryKind, Phase,
    StepLabel, TransitionOrigin,
};
use crate::sem::program::{ResKind, SemOp, SemProgram, SlotClass};
use crate::sem::system::{
    compare_values, BlockKind, BlockedRecord, DoomState, DoomThread, DoomWait, Enabled,
    InstanceState, Predicate, Step, TransitionSystem,
};
use crate::sem::value::{default_value, within_type, Value};

use super::lower::{build, LoweredHlnNet, PlaceKey};

/// Convert concir [`Value`] into lossless UniPN [`HlnVal`].
pub fn val_to_hln(v: &Value) -> HlnVal {
    match v {
        Value::Bool(b) => HlnVal::Bool(*b),
        Value::Int(i) => HlnVal::Int(*i),
        Value::Float(f) => HlnVal::Float(f.to_bits()),
        Value::Str(s) => HlnVal::Str(s.clone()),
        Value::Enum(s) => HlnVal::Enum(s.clone()),
        Value::Struct(fields) => HlnVal::Struct(
            fields
                .iter()
                .map(|(k, val)| (k.clone(), val_to_hln(val)))
                .collect(),
        ),
        Value::Array(items) => HlnVal::Array(items.iter().map(val_to_hln).collect()),
    }
}

/// Convert UniPN [`HlnVal`] back into concir [`Value`].
pub fn hln_to_val(v: &HlnVal) -> Value {
    match v {
        HlnVal::Bool(b) => Value::Bool(*b),
        HlnVal::Int(i) => Value::Int(*i),
        HlnVal::Float(bits) => Value::Float(f64::from_bits(*bits)),
        HlnVal::Str(s) => Value::Str(s.clone()),
        HlnVal::Enum(s) => Value::Enum(s.clone()),
        HlnVal::Struct(fields) => Value::Struct(
            fields
                .iter()
                .map(|(k, val)| (k.clone(), hln_to_val(val)))
                .collect(),
        ),
        HlnVal::Array(items) => Value::Array(items.iter().map(hln_to_val).collect()),
        HlnVal::Unknown => Value::Int(0),
    }
}

struct HlnValueView<'a> {
    locals: &'a BTreeMap<usize, HlnVal>,
    vars: &'a BTreeMap<usize, HlnVal>,
    cache_locals: BTreeMap<SlotId, Value>,
    cache_shared: BTreeMap<ResourceId, Value>,
}

impl<'a> HlnValueView<'a> {
    fn new(frame: &'a HlnFrame, state: &'a HlnState) -> Self {
        let cache_locals = frame
            .locals
            .iter()
            .map(|(&slot, v)| (slot, hln_to_val(v)))
            .collect();
        let cache_shared = state
            .vars
            .iter()
            .map(|(&res, v)| (ResourceId(res as u32), hln_to_val(v)))
            .collect();
        Self {
            locals: &frame.locals,
            vars: &state.vars,
            cache_locals,
            cache_shared,
        }
    }
}

impl ValueStore for HlnValueView<'_> {
    fn get_slot(&self, slot: SlotId) -> Option<&Value> {
        let _ = self.locals.get(&slot)?;
        self.cache_locals.get(&slot)
    }

    fn get_shared(&self, resource: ResourceId) -> Option<&Value> {
        let _ = self.vars.get(&(resource.0 as usize))?;
        self.cache_shared.get(&resource)
    }
}

/// UniPN HLN execution engine replacing legacy `PetriEngine`.
pub struct UnipnEngine<'a> {
    pub program: &'a SemProgram,
    pub net: LoweredHlnNet,
    pub bounds: AnalysisBounds,
    pub monitor: MonitorConfig,
}

impl<'a> UnipnEngine<'a> {
    pub fn new(program: &'a SemProgram, bounds: AnalysisBounds) -> Self {
        Self::with_monitor(program, bounds, MonitorConfig::unbounded())
    }

    pub fn with_monitor(
        program: &'a SemProgram,
        bounds: AnalysisBounds,
        monitor: MonitorConfig,
    ) -> Self {
        let net = build(program);
        Self {
            program,
            net,
            bounds,
            monitor,
        }
    }

    fn eval(
        &self,
        state: &HlnState,
        frame: &HlnFrame,
        expr: &LExpr,
        at: &str,
    ) -> BackendResult<Value> {
        let view = HlnValueView::new(frame, state);
        eval(expr, &view, at)
    }

    fn record_completion(&self, state: &mut HlnState, function: FunctionId) {
        let max = self.monitor.max_for(function);
        if max == 0 {
            return;
        }
        let e = state
            .completed_functions
            .entry(function.0 as usize)
            .or_insert(0);
        if *e < max {
            *e += 1;
        }
    }

    fn step_label(
        &self,
        func: FunctionId,
        sid: Option<usize>,
        phase: Phase,
        tid: u32,
        fid: u32,
    ) -> StepLabel {
        StepLabel::new(TransitionOrigin {
            module: self.program.function(func).module,
            function: func,
            sid,
            phase,
        })
        .with_binding(ThreadId(tid as u64), FrameId(fid as u64))
    }

    fn rname(&self, r: ResourceId) -> String {
        let res = self.program.resource(r);
        fqn::fqn(self.program.module_name(res.module), &res.name)
    }

    fn dst_type_ok(&self, func: FunctionId, dst: SlotRef, value: &Value) -> bool {
        match dst {
            SlotRef::Discard => true,
            SlotRef::Local(slot) => self
                .program
                .function(func)
                .slots
                .get(slot)
                .map_or(true, |s| within_type(value, &s.ty)),
            SlotRef::Shared(r) => self
                .program
                .resource(r)
                .ty
                .as_ref()
                .map_or(true, |ty| within_type(value, ty)),
        }
    }

    fn write_dst_raw(state: &mut HlnState, tid: u32, dst: SlotRef, value: Value) {
        let hval = val_to_hln(&value);
        match dst {
            SlotRef::Discard => {}
            SlotRef::Local(slot) => {
                for list in state.control_tokens.values_mut() {
                    if let Some(tok) = list.iter_mut().find(|t| t.tid == tid) {
                        if let Some(frame) = tok.stack.last_mut() {
                            frame.locals.insert(slot, hval);
                        }
                        break;
                    }
                }
            }
            SlotRef::Shared(r) => {
                state.vars.insert(r.0 as usize, hval);
            }
        }
    }

    fn try_write_dst(
        &self,
        state: &mut HlnState,
        tid: u32,
        func: FunctionId,
        dst: SlotRef,
        value: Value,
    ) -> bool {
        if !self.dst_type_ok(func, dst, &value) {
            return false;
        }
        Self::write_dst_raw(state, tid, dst, value);
        true
    }

    fn move_thread_token(
        &self,
        state: &mut HlnState,
        tid: u32,
        dst_pid: PlaceId,
        token: ControlToken,
    ) {
        if let Some((cur_pid, _)) = state.find_thread(tid) {
            state.take_control_token(cur_pid, tid);
        }
        state.put_control_token(dst_pid, token);
    }

    fn advance_token(
        &self,
        state: &mut HlnState,
        mut tok: ControlToken,
        next_pc: usize,
    ) -> BackendResult<()> {
        tok.wait_tag = None;
        let func = FunctionId(tok.current_func() as u32);
        if let Some(fr) = tok.current_frame_mut() {
            fr.pc = next_pc;
        }
        let pid = self.net.pc_place(func, next_pc).ok_or_else(|| {
            BackendError::invalid("E602", format!("no control place for f{}@{next_pc}", func.0))
        })?;
        self.move_thread_token(state, tok.tid, pid, tok);
        Ok(())
    }

    fn park_token(&self, state: &mut HlnState, mut tok: ControlToken, pid: PlaceId, tag: WaitTag) {
        tok.wait_tag = Some(tag);
        self.move_thread_token(state, tok.tid, pid, tok);
    }

    fn sync_mutex_marking(&self, state: &mut HlnState, res_id: usize) {
        if let Some(&pid) = self
            .net
            .place_of
            .get(&PlaceKey::Mutex(ResourceId(res_id as u32)))
        {
            let free = state
                .mutexes
                .get(&res_id)
                .map_or(true, Option::is_none);
            state.marking.set(pid, usize::from(free));
        }
    }

    fn sync_rwlock_marking(&self, state: &mut HlnState, res_id: usize) {
        if let Some(&pid) = self
            .net
            .place_of
            .get(&PlaceKey::RwLock(ResourceId(res_id as u32)))
        {
            let count = match state.rwlocks.get(&res_id) {
                Some(RwState::Free) | None => 1,
                Some(RwState::Reading(_)) | Some(RwState::Writing(_)) => 0,
            };
            state.marking.set(pid, count);
        }
    }

    fn sync_sem_marking(&self, state: &mut HlnState, res_id: usize) {
        if let Some(&pid) = self
            .net
            .place_of
            .get(&PlaceKey::Semaphore(ResourceId(res_id as u32)))
        {
            let p = state.semaphores.get(&res_id).copied().unwrap_or(0);
            state.marking.set(pid, p.max(0) as usize);
        }
    }

    fn sync_chan_marking(&self, state: &mut HlnState, res_id: usize) {
        if let Some(&pid) = self
            .net
            .place_of
            .get(&PlaceKey::Channel(ResourceId(res_id as u32)))
        {
            let len = state.channels.get(&res_id).map_or(0, VecDeque::len);
            state.marking.set(pid, len);
        }
    }

    fn alloc_frame(
        &self,
        state: &mut HlnState,
        func: FunctionId,
        ret: Option<HlnRetAddr>,
    ) -> HlnFrame {
        let frame_id = state.alloc.next_frame;
        state.alloc.next_frame += 1;
        let f = self.program.function(func);
        let mut locals = BTreeMap::new();
        for (i, slot) in f.slots.iter().enumerate() {
            let v = slot
                .init
                .clone()
                .unwrap_or_else(|| default_value(&slot.ty));
            locals.insert(i, val_to_hln(&v));
        }
        HlnFrame {
            frame_id,
            func: func.0 as usize,
            pc: 0,
            locals,
            handles: BTreeMap::new(),
            ret,
        }
    }

    fn live_thread_count(&self, state: &HlnState) -> usize {
        state
            .all_tokens()
            .filter(|(_, tok)| !state.finished.contains(&tok.tid))
            .count()
    }

    fn within_function_bound(&self, state: &HlnState, func: FunctionId) -> bool {
        let Some(bound) = self.program.function(func).bound else {
            return true;
        };
        let active = state
            .all_tokens()
            .filter(|(_, tok)| {
                !state.finished.contains(&tok.tid) && tok.entry_func == func.0 as usize
            })
            .count();
        active < bound.max(0) as usize
    }

    fn create_thread(
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

    fn finish_thread(
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

    fn apply_return(
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

    fn deliver_to_recv(
        &self,
        state: &mut HlnState,
        recv_tid: u32,
        val: Value,
    ) -> BackendResult<bool> {
        let Some((_, rtok)) = state.find_thread(recv_tid).map(|(p, t)| (p, t.clone())) else {
            return Ok(false);
        };
        let Some(rframe) = rtok.current_frame() else {
            return Ok(false);
        };
        let func = FunctionId(rframe.func as u32);
        let pc = rframe.pc;
        let dst = match self.program.function(func).body.get(pc) {
            Some(stmt) => match stmt.op {
                SemOp::ChannelRecv { dst, .. } => dst,
                _ => SlotRef::Discard,
            },
            None => SlotRef::Discard,
        };
        if !self.try_write_dst(state, recv_tid, func, dst, val) {
            return Ok(false);
        }
        let Some((_, fresh_tok)) = state.find_thread(recv_tid).map(|(p, t)| (p, t.clone())) else {
            return Ok(false);
        };
        self.advance_token(state, fresh_tok, pc + 1)?;
        Ok(true)
    }

    fn step_runnable_token(
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
            SemOp::MutexLock { resource } => {
                let rid = resource.0 as usize;
                if next.mutexes.get(&rid).copied().flatten().is_none() {
                    next.mutexes.insert(rid, Some(tok.tid));
                    self.sync_mutex_marking(&mut next, rid);
                    let mut utok = tok.clone();
                    utok.held_locks.insert(rid, LockHoldMode::Exclusive);
                    self.advance_token(&mut next, utok, pc + 1)?;
                } else {
                    let c_pid = self.net.place_of[&PlaceKey::Contend(func, pc)];
                    self.park_token(&mut next, tok.clone(), c_pid, WaitTag::Mutex(rid));
                }
            }
            SemOp::MutexUnlock { resource } => {
                let rid = resource.0 as usize;
                match next.mutexes.get(&rid).copied().flatten() {
                    Some(owner) if owner == tok.tid => {}
                    Some(_) => {
                        return Err(BackendError::invalid(
                            "E510",
                            format!(
                                "thread {} unlocked Mutex {resource} it does not hold",
                                tok.tid
                            ),
                        )
                        .at(&at))
                    }
                    None => {
                        return Err(BackendError::invalid(
                            "E510",
                            format!("thread {} unlocked free Mutex {resource}", tok.tid),
                        )
                        .at(&at))
                    }
                }
                next.mutexes.insert(rid, None);
                self.sync_mutex_marking(&mut next, rid);
                let mut utok = tok.clone();
                utok.held_locks.remove(&rid);
                self.advance_token(&mut next, utok, pc + 1)?;
            }
            SemOp::RwLockRead { resource } => {
                let rid = resource.0 as usize;
                match next.rwlocks.get(&rid).cloned() {
                    Some(RwState::Free) => {
                        let mut readers = BTreeSet::new();
                        readers.insert(tok.tid);
                        next.rwlocks.insert(rid, RwState::Reading(readers));
                        self.sync_rwlock_marking(&mut next, rid);
                        let mut utok = tok.clone();
                        utok.held_locks.insert(rid, LockHoldMode::Shared);
                        self.advance_token(&mut next, utok, pc + 1)?;
                    }
                    Some(RwState::Reading(mut readers)) => {
                        readers.insert(tok.tid);
                        next.rwlocks.insert(rid, RwState::Reading(readers));
                        self.sync_rwlock_marking(&mut next, rid);
                        let mut utok = tok.clone();
                        utok.held_locks.insert(rid, LockHoldMode::Shared);
                        self.advance_token(&mut next, utok, pc + 1)?;
                    }
                    _ => {
                        let c_pid = self.net.place_of[&PlaceKey::Contend(func, pc)];
                        self.park_token(&mut next, tok.clone(), c_pid, WaitTag::RwRead(rid));
                    }
                }
            }
            SemOp::RwLockWrite { resource } => {
                let rid = resource.0 as usize;
                if matches!(next.rwlocks.get(&rid), Some(RwState::Free)) {
                    next.rwlocks.insert(rid, RwState::Writing(tok.tid));
                    self.sync_rwlock_marking(&mut next, rid);
                    let mut utok = tok.clone();
                    utok.held_locks.insert(rid, LockHoldMode::Exclusive);
                    self.advance_token(&mut next, utok, pc + 1)?;
                } else {
                    let c_pid = self.net.place_of[&PlaceKey::Contend(func, pc)];
                    self.park_token(&mut next, tok.clone(), c_pid, WaitTag::RwWrite(rid));
                }
            }
            SemOp::RwLockUnlock { resource } => {
                let rid = resource.0 as usize;
                match next.rwlocks.get(&rid).cloned() {
                    Some(RwState::Writing(owner)) if owner == tok.tid => {
                        next.rwlocks.insert(rid, RwState::Free);
                        self.sync_rwlock_marking(&mut next, rid);
                        let mut utok = tok.clone();
                        utok.held_locks.remove(&rid);
                        self.advance_token(&mut next, utok, pc + 1)?;
                    }
                    Some(RwState::Reading(mut readers)) if readers.contains(&tok.tid) => {
                        readers.remove(&tok.tid);
                        if readers.is_empty() {
                            next.rwlocks.insert(rid, RwState::Free);
                        } else {
                            next.rwlocks.insert(rid, RwState::Reading(readers));
                        }
                        self.sync_rwlock_marking(&mut next, rid);
                        let mut utok = tok.clone();
                        utok.held_locks.remove(&rid);
                        self.advance_token(&mut next, utok, pc + 1)?;
                    }
                    _ => {
                        return Err(BackendError::invalid(
                            "E510",
                            format!(
                                "thread {} unlocked RwLock {resource} it does not hold",
                                tok.tid
                            ),
                        )
                        .at(&at))
                    }
                }
            }
            SemOp::ChannelSend { channel, value } => {
                let v = self.eval(&next, frame, value, &at)?;
                if let Some(ty) = &self.program.resource(*channel).ty
                    && !within_type(&v, ty)
                {
                    return Ok(Vec::new());
                }
                let cid = channel.0 as usize;
                let cap = self.program.resource(*channel).capacity;
                if cap == 0 {
                    let receivers: Vec<u32> = next
                        .all_tokens()
                        .filter_map(|(_, t)| match t.wait_tag {
                            Some(WaitTag::ChannelRecv(c)) if c == cid => Some(t.tid),
                            _ => None,
                        })
                        .collect();
                    if receivers.is_empty() {
                        let c_pid = self.net.place_of[&PlaceKey::Contend(func, pc)];
                        self.park_token(
                            &mut next,
                            tok.clone(),
                            c_pid,
                            WaitTag::ChannelSend {
                                channel: cid,
                                value: val_to_hln(&v),
                            },
                        );
                    } else {
                        let mut out = Vec::new();
                        for r_tid in receivers {
                            let mut s = next.clone();
                            if !self.deliver_to_recv(&mut s, r_tid, v.clone())? {
                                continue;
                            }
                            self.advance_token(&mut s, tok.clone(), pc + 1)?;
                            out.push(Step {
                                label: label.clone(),
                                state: s,
                            });
                        }
                        return Ok(out);
                    }
                } else {
                    let has_space = next
                        .channels
                        .get(&cid)
                        .map_or(false, |buf| buf.len() < cap);
                    if has_space {
                        next.channels
                            .get_mut(&cid)
                            .unwrap()
                            .push_back(val_to_hln(&v));
                        self.sync_chan_marking(&mut next, cid);
                        self.advance_token(&mut next, tok.clone(), pc + 1)?;
                    } else {
                        let c_pid = self.net.place_of[&PlaceKey::Contend(func, pc)];
                        self.park_token(
                            &mut next,
                            tok.clone(),
                            c_pid,
                            WaitTag::ChannelSend {
                                channel: cid,
                                value: val_to_hln(&v),
                            },
                        );
                    }
                }
            }
            SemOp::ChannelRecv { channel, dst } => {
                let cid = channel.0 as usize;
                let cap = self.program.resource(*channel).capacity;
                if cap == 0 {
                    let senders: Vec<(u32, Value)> = next
                        .all_tokens()
                        .filter_map(|(_, t)| match &t.wait_tag {
                            Some(WaitTag::ChannelSend { channel: c, value }) if *c == cid => {
                                Some((t.tid, hln_to_val(value)))
                            }
                            _ => None,
                        })
                        .collect();
                    if senders.is_empty() {
                        let c_pid = self.net.place_of[&PlaceKey::Contend(func, pc)];
                        self.park_token(&mut next, tok.clone(), c_pid, WaitTag::ChannelRecv(cid));
                    } else {
                        let mut out = Vec::new();
                        for (s_tid, s_val) in senders {
                            let mut s = next.clone();
                            if !self.try_write_dst(&mut s, tok.tid, func, *dst, s_val) {
                                continue;
                            }
                            if let Some((_, s_tok)) =
                                s.find_thread(s_tid).map(|(p, t)| (p, t.clone()))
                            {
                                let spc = s_tok.current_pc().unwrap_or(0) + 1;
                                self.advance_token(&mut s, s_tok, spc)?;
                            }
                            let (_, r_tok) = s.find_thread(tok.tid).unwrap();
                            let r_tok = r_tok.clone();
                            self.advance_token(&mut s, r_tok, pc + 1)?;
                            out.push(Step {
                                label: label.clone(),
                                state: s,
                            });
                        }
                        return Ok(out);
                    }
                } else {
                    let head = next
                        .channels
                        .get(&cid)
                        .and_then(VecDeque::front)
                        .map(hln_to_val);
                    match head {
                        Some(v) => {
                            if !self.try_write_dst(&mut next, tok.tid, func, *dst, v) {
                                return Ok(Vec::new());
                            }
                            next.channels.get_mut(&cid).unwrap().pop_front();
                            self.sync_chan_marking(&mut next, cid);
                            let (_, fresh) = next.find_thread(tok.tid).unwrap();
                            let fresh = fresh.clone();
                            self.advance_token(&mut next, fresh, pc + 1)?;
                        }
                        None => {
                            let c_pid = self.net.place_of[&PlaceKey::Contend(func, pc)];
                            self.park_token(
                                &mut next,
                                tok.clone(),
                                c_pid,
                                WaitTag::ChannelRecv(cid),
                            );
                        }
                    }
                }
            }
            SemOp::CondvarWait { condvar, lock } => {
                let lid = lock.0 as usize;
                match next.mutexes.get(&lid).copied().flatten() {
                    Some(owner) if owner == tok.tid => {}
                    _ => {
                        return Err(BackendError::invalid(
                            "E511",
                            format!("condvar wait without holding lock {}", self.rname(*lock)),
                        )
                        .at(&at))
                    }
                }
                next.mutexes.insert(lid, None);
                self.sync_mutex_marking(&mut next, lid);
                let mut utok = tok.clone();
                utok.held_locks.remove(&lid);
                let wait_pid = self.net.place_of[&PlaceKey::Wait(func, pc)];
                self.park_token(
                    &mut next,
                    utok,
                    wait_pid,
                    WaitTag::Condvar {
                        condvar: condvar.0 as usize,
                        lock: lid,
                    },
                );
            }
            SemOp::CondvarNotify { condvar } => {
                let cv_id = condvar.0 as usize;
                let waiters: Vec<(PlaceId, ControlToken, usize)> = next
                    .all_tokens()
                    .filter_map(|(pid, t)| match t.wait_tag {
                        Some(WaitTag::Condvar { condvar: c, lock }) if c == cv_id => {
                            Some((pid, t.clone(), lock))
                        }
                        _ => None,
                    })
                    .collect();
                if waiters.is_empty() {
                    self.advance_token(&mut next, tok.clone(), pc + 1)?;
                    return Ok(vec![Step { label, state: next }]);
                }
                let mut out = Vec::new();
                for (wait_pid, mut wtok, lock) in waiters {
                    let mut s = next.clone();
                    let reacq_pid = self.net.wait_to_reacquire[&wait_pid];
                    wtok.wait_tag = Some(WaitTag::Mutex(lock));
                    self.move_thread_token(&mut s, wtok.tid, reacq_pid, wtok);
                    self.advance_token(&mut s, tok.clone(), pc + 1)?;
                    out.push(Step {
                        label: label.clone(),
                        state: s,
                    });
                }
                return Ok(out);
            }
            SemOp::CondvarNotifyAll { condvar } => {
                let pairs = self
                    .net
                    .condvar_waits
                    .get(condvar)
                    .cloned()
                    .unwrap_or_default();
                for (wait_pid, reacq_pid) in pairs {
                    let drained = next.drain_control_tokens(wait_pid);
                    for mut wtok in drained {
                        if let Some(WaitTag::Condvar { lock, .. }) = wtok.wait_tag {
                            wtok.wait_tag = Some(WaitTag::Mutex(lock));
                        }
                        next.put_control_token(reacq_pid, wtok);
                    }
                }
                self.advance_token(&mut next, tok.clone(), pc + 1)?;
            }
            SemOp::SemaphoreAcquire { resource, count } => {
                if *count <= 0 {
                    return Err(
                        BackendError::invalid("E904", "semaphore count must be positive").at(&at),
                    );
                }
                let rid = resource.0 as usize;
                let available = *next.semaphores.get(&rid).unwrap_or(&0);
                if available >= *count {
                    next.semaphores.insert(rid, available - *count);
                    self.sync_sem_marking(&mut next, rid);
                    self.advance_token(&mut next, tok.clone(), pc + 1)?;
                } else {
                    let c_pid = self.net.place_of[&PlaceKey::Contend(func, pc)];
                    self.park_token(
                        &mut next,
                        tok.clone(),
                        c_pid,
                        WaitTag::Semaphore {
                            resource: rid,
                            count: *count,
                        },
                    );
                }
            }
            SemOp::SemaphoreRelease { resource, count } => {
                if *count <= 0 {
                    return Err(
                        BackendError::invalid("E904", "semaphore count must be positive").at(&at),
                    );
                }
                let rid = resource.0 as usize;
                let available = *next.semaphores.get(&rid).unwrap_or(&0);
                let updated = available.checked_add(*count).ok_or_else(|| {
                    BackendError::invalid(
                        "E905",
                        format!("semaphore '{resource}' permit count overflows i64"),
                    )
                    .at(&at)
                })?;
                next.semaphores.insert(rid, updated);
                self.sync_sem_marking(&mut next, rid);
                self.advance_token(&mut next, tok.clone(), pc + 1)?;
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

    fn step_waiting_token(
        &self,
        state: &HlnState,
        pid: PlaceId,
        tok: &ControlToken,
        tag: &WaitTag,
    ) -> BackendResult<Vec<Step<HlnState>>> {
        let Some(frame) = tok.current_frame() else {
            return Ok(Vec::new());
        };
        let func = FunctionId(frame.func as u32);
        let pc = frame.pc;
        let phase = match self.net.place_key(pid) {
            Some(PlaceKey::Reacquire(_, _)) => Phase::Reacquire,
            _ => Phase::Statement,
        };
        let label = self.step_label(func, Some(pc), phase, tok.tid, frame.frame_id);

        match tag {
            WaitTag::Mutex(res_id) => {
                if state.mutexes.get(res_id).copied().flatten().is_some() {
                    return Ok(Vec::new());
                }
                let mut next = state.clone();
                next.mutexes.insert(*res_id, Some(tok.tid));
                self.sync_mutex_marking(&mut next, *res_id);
                let mut utok = tok.clone();
                utok.held_locks.insert(*res_id, LockHoldMode::Exclusive);
                self.advance_token(&mut next, utok, pc + 1)?;
                Ok(vec![Step { label, state: next }])
            }
            WaitTag::RwRead(res_id) => {
                let mut next = state.clone();
                match next.rwlocks.get(res_id).cloned() {
                    Some(RwState::Free) => {
                        let mut readers = BTreeSet::new();
                        readers.insert(tok.tid);
                        next.rwlocks.insert(*res_id, RwState::Reading(readers));
                    }
                    Some(RwState::Reading(mut readers)) => {
                        readers.insert(tok.tid);
                        next.rwlocks.insert(*res_id, RwState::Reading(readers));
                    }
                    _ => return Ok(Vec::new()),
                }
                self.sync_rwlock_marking(&mut next, *res_id);
                let mut utok = tok.clone();
                utok.held_locks.insert(*res_id, LockHoldMode::Shared);
                self.advance_token(&mut next, utok, pc + 1)?;
                Ok(vec![Step { label, state: next }])
            }
            WaitTag::RwWrite(res_id) => {
                if !matches!(state.rwlocks.get(res_id), Some(RwState::Free)) {
                    return Ok(Vec::new());
                }
                let mut next = state.clone();
                next.rwlocks.insert(*res_id, RwState::Writing(tok.tid));
                self.sync_rwlock_marking(&mut next, *res_id);
                let mut utok = tok.clone();
                utok.held_locks.insert(*res_id, LockHoldMode::Exclusive);
                self.advance_token(&mut next, utok, pc + 1)?;
                Ok(vec![Step { label, state: next }])
            }
            WaitTag::Semaphore { resource, count } => {
                let available = *state.semaphores.get(resource).unwrap_or(&0);
                if available < *count {
                    return Ok(Vec::new());
                }
                let mut next = state.clone();
                next.semaphores.insert(*resource, available - *count);
                self.sync_sem_marking(&mut next, *resource);
                self.advance_token(&mut next, tok.clone(), pc + 1)?;
                Ok(vec![Step { label, state: next }])
            }
            WaitTag::ChannelSend { channel, value } => {
                let cap = self.program.resource(ResourceId(*channel as u32)).capacity;
                if cap == 0 {
                    let receivers: Vec<u32> = state
                        .all_tokens()
                        .filter_map(|(_, t)| match t.wait_tag {
                            Some(WaitTag::ChannelRecv(c)) if c == *channel => Some(t.tid),
                            _ => None,
                        })
                        .collect();
                    let mut out = Vec::new();
                    for r_tid in receivers {
                        let mut s = state.clone();
                        if !self.deliver_to_recv(&mut s, r_tid, hln_to_val(value))? {
                            continue;
                        }
                        self.advance_token(&mut s, tok.clone(), pc + 1)?;
                        out.push(Step {
                            label: label.clone(),
                            state: s,
                        });
                    }
                    Ok(out)
                } else {
                    let has_space = state
                        .channels
                        .get(channel)
                        .map_or(false, |buf| buf.len() < cap);
                    if !has_space {
                        return Ok(Vec::new());
                    }
                    let mut next = state.clone();
                    next.channels
                        .get_mut(channel)
                        .unwrap()
                        .push_back(value.clone());
                    self.sync_chan_marking(&mut next, *channel);
                    self.advance_token(&mut next, tok.clone(), pc + 1)?;
                    Ok(vec![Step { label, state: next }])
                }
            }
            WaitTag::ChannelRecv(channel) => {
                let cap = self.program.resource(ResourceId(*channel as u32)).capacity;
                let dst = match self.program.function(func).body.get(pc) {
                    Some(stmt) => match stmt.op {
                        SemOp::ChannelRecv { dst, .. } => dst,
                        _ => SlotRef::Discard,
                    },
                    None => SlotRef::Discard,
                };
                if cap == 0 {
                    let senders: Vec<(u32, Value)> = state
                        .all_tokens()
                        .filter_map(|(_, t)| match &t.wait_tag {
                            Some(WaitTag::ChannelSend { channel: c, value }) if *c == *channel => {
                                Some((t.tid, hln_to_val(value)))
                            }
                            _ => None,
                        })
                        .collect();
                    let mut out = Vec::new();
                    for (s_tid, s_val) in senders {
                        let mut s = state.clone();
                        if !self.try_write_dst(&mut s, tok.tid, func, dst, s_val) {
                            continue;
                        }
                        if let Some((_, st)) = s.find_thread(s_tid).map(|(p, t)| (p, t.clone())) {
                            let spc = st.current_pc().unwrap_or(0) + 1;
                            self.advance_token(&mut s, st, spc)?;
                        }
                        let (_, rtok) = s.find_thread(tok.tid).unwrap();
                        let rtok = rtok.clone();
                        self.advance_token(&mut s, rtok, pc + 1)?;
                        out.push(Step {
                            label: label.clone(),
                            state: s,
                        });
                    }
                    Ok(out)
                } else {
                    let head = state
                        .channels
                        .get(channel)
                        .and_then(VecDeque::front)
                        .map(hln_to_val);
                    let Some(v) = head else {
                        return Ok(Vec::new());
                    };
                    let mut next = state.clone();
                    if !self.try_write_dst(&mut next, tok.tid, func, dst, v) {
                        return Ok(Vec::new());
                    }
                    next.channels.get_mut(channel).unwrap().pop_front();
                    self.sync_chan_marking(&mut next, *channel);
                    let (_, rtok) = next.find_thread(tok.tid).unwrap();
                    let rtok = rtok.clone();
                    self.advance_token(&mut next, rtok, pc + 1)?;
                    Ok(vec![Step { label, state: next }])
                }
            }
            WaitTag::Condvar { .. } | WaitTag::Join(_) | WaitTag::Scope(_) => Ok(Vec::new()),
        }
    }

    fn thread_holds(&self, state: &HlnState, tok: &ControlToken, r: ResourceId) -> bool {
        let rid = r.0 as usize;
        match self.program.resource(r).kind {
            ResKind::Mutex => state.mutexes.get(&rid).copied().flatten() == Some(tok.tid),
            ResKind::RwLock => match state.rwlocks.get(&rid) {
                Some(RwState::Reading(readers)) => readers.contains(&tok.tid),
                Some(RwState::Writing(w)) => *w == tok.tid,
                _ => false,
            },
            ResKind::Semaphore => {
                state.semaphores.get(&rid).copied().unwrap_or(0) < self.program.resource(r).permits
            }
            _ => false,
        }
    }
}

fn ret_dst(ret: &Option<HlnRetAddr>) -> SlotRef {
    match ret {
        Some(r) => match (r.dst_local, r.dst_shared) {
            (Some(l), _) => SlotRef::Local(l),
            (None, Some(s)) => SlotRef::Shared(ResourceId(s as u32)),
            (None, None) => SlotRef::Discard,
        },
        None => SlotRef::Discard,
    }
}

impl TransitionSystem for UnipnEngine<'_> {
    type State = HlnState;

    fn program(&self) -> &SemProgram {
        self.program
    }

    fn initial(&self) -> BackendResult<HlnState> {
        let num_places = self.net.net.places.len();
        let mut state = HlnState::empty(num_places);

        for r in self.program.resources() {
            let rid = r.id.0 as usize;
            match r.kind {
                ResKind::Var | ResKind::Atomic => {
                    let v = r
                        .init
                        .clone()
                        .or_else(|| r.ty.as_ref().map(default_value))
                        .unwrap_or(Value::Int(0));
                    state.vars.insert(rid, val_to_hln(&v));
                    if let Some(&pid) = self
                        .net
                        .place_of
                        .get(&PlaceKey::Var(r.id))
                        .or_else(|| self.net.place_of.get(&PlaceKey::Atomic(r.id)))
                    {
                        state.marking.set(pid, 1);
                    }
                }
                ResKind::Mutex => {
                    state.mutexes.insert(rid, None);
                    self.sync_mutex_marking(&mut state, rid);
                }
                ResKind::RwLock => {
                    state.rwlocks.insert(rid, RwState::Free);
                    self.sync_rwlock_marking(&mut state, rid);
                }
                ResKind::Semaphore => {
                    state.semaphores.insert(rid, r.permits);
                    self.sync_sem_marking(&mut state, rid);
                }
                ResKind::Channel => {
                    state.channels.insert(rid, VecDeque::new());
                    self.sync_chan_marking(&mut state, rid);
                }
                ResKind::Condvar => {}
            }
        }

        let entry = self.program.entry();
        let mut boundary = Vec::new();
        self.create_thread(&mut state, entry, None, &mut boundary)?;
        Ok(state)
    }

    fn successors(&self, state: &HlnState) -> BackendResult<Enabled<HlnState>> {
        let mut steps = Vec::new();
        let mut boundary = Vec::new();

        let mut tokens: Vec<(PlaceId, ControlToken)> = state
            .all_tokens()
            .filter(|(_, t)| !state.finished.contains(&t.tid))
            .map(|(p, t)| (p, t.clone()))
            .collect();
        tokens.sort_by_key(|(_, t)| t.tid);

        for (pid, tok) in tokens {
            let emitted = match &tok.wait_tag {
                None => self.step_runnable_token(state, &tok, &mut boundary)?,
                Some(tag) => self.step_waiting_token(state, pid, &tok, tag)?,
            };
            steps.extend(emitted);
        }

        Ok(Enabled { steps, boundary })
    }

    fn is_finished(&self, state: &HlnState) -> bool {
        !state
            .all_tokens()
            .any(|(_, t)| !state.finished.contains(&t.tid))
    }

    fn blocked(&self, state: &HlnState) -> Vec<BlockedRecord> {
        let mut out = Vec::new();
        let mut waiters: Vec<&ControlToken> = state
            .all_tokens()
            .filter(|(_, t)| !state.finished.contains(&t.tid) && t.wait_tag.is_some())
            .map(|(_, t)| t)
            .collect();
        waiters.sort_by_key(|t| t.tid);

        for tok in waiters {
            let Some(tag) = &tok.wait_tag else { continue };
            let t_id = ThreadId(tok.tid as u64);
            match tag {
                WaitTag::Mutex(r) => {
                    let holder = state
                        .mutexes
                        .get(r)
                        .copied()
                        .flatten()
                        .map(|t| ThreadId(t as u64));
                    let rid = ResourceId(*r as u32);
                    let rname = self.rname(rid);
                    out.push(BlockedRecord {
                        thread: t_id,
                        kind: BlockKind::Lock,
                        resource: Some(rid),
                        resource_name: Some(rname.clone()),
                        holder,
                        waiting: 1,
                        detail: format!("waiting for Mutex {rname}"),
                    });
                }
                WaitTag::RwRead(r) => {
                    let holder = match state.rwlocks.get(r) {
                        Some(RwState::Writing(w)) => Some(ThreadId(*w as u64)),
                        _ => None,
                    };
                    let rid = ResourceId(*r as u32);
                    let rname = self.rname(rid);
                    out.push(BlockedRecord {
                        thread: t_id,
                        kind: BlockKind::Lock,
                        resource: Some(rid),
                        resource_name: Some(rname.clone()),
                        holder,
                        waiting: 1,
                        detail: format!("waiting for RwLock read {rname}"),
                    });
                }
                WaitTag::RwWrite(r) => {
                    let holder = match state.rwlocks.get(r) {
                        Some(RwState::Writing(w)) => Some(ThreadId(*w as u64)),
                        _ => None,
                    };
                    let rid = ResourceId(*r as u32);
                    let rname = self.rname(rid);
                    out.push(BlockedRecord {
                        thread: t_id,
                        kind: BlockKind::Lock,
                        resource: Some(rid),
                        resource_name: Some(rname.clone()),
                        holder,
                        waiting: 1,
                        detail: format!("waiting for RwLock write {rname}"),
                    });
                }
                WaitTag::Condvar { condvar, lock } => {
                    let cid = ResourceId(*condvar as u32);
                    let cname = self.rname(cid);
                    let lname = self.rname(ResourceId(*lock as u32));
                    out.push(BlockedRecord {
                        thread: t_id,
                        kind: BlockKind::Condvar,
                        resource: Some(cid),
                        resource_name: Some(cname),
                        holder: None,
                        waiting: 1,
                        detail: format!("waiting on Condvar (Mutex {lname})"),
                    });
                }
                WaitTag::Semaphore { resource, count } => {
                    let rid = ResourceId(*resource as u32);
                    let rname = self.rname(rid);
                    out.push(BlockedRecord {
                        thread: t_id,
                        kind: BlockKind::Semaphore,
                        resource: Some(rid),
                        resource_name: Some(rname.clone()),
                        holder: None,
                        waiting: 1,
                        detail: format!("waiting for {count} permit(s) on Semaphore {rname}"),
                    });
                }
                WaitTag::ChannelSend { channel, .. } => {
                    let cid = ResourceId(*channel as u32);
                    let cname = self.rname(cid);
                    out.push(BlockedRecord {
                        thread: t_id,
                        kind: BlockKind::ChannelSend,
                        resource: Some(cid),
                        resource_name: Some(cname.clone()),
                        holder: None,
                        waiting: 1,
                        detail: format!("waiting to send on Channel {cname}"),
                    });
                }
                WaitTag::ChannelRecv(channel) => {
                    let cid = ResourceId(*channel as u32);
                    let cname = self.rname(cid);
                    out.push(BlockedRecord {
                        thread: t_id,
                        kind: BlockKind::ChannelRecv,
                        resource: Some(cid),
                        resource_name: Some(cname.clone()),
                        holder: None,
                        waiting: 1,
                        detail: format!("waiting to recv from Channel {cname}"),
                    });
                }
                WaitTag::Join(_) => {
                    out.push(BlockedRecord {
                        thread: t_id,
                        kind: BlockKind::Join,
                        resource: None,
                        resource_name: None,
                        holder: None,
                        waiting: 1,
                        detail: "waiting to join child thread".into(),
                    });
                }
                WaitTag::Scope(_) => {
                    out.push(BlockedRecord {
                        thread: t_id,
                        kind: BlockKind::Scope,
                        resource: None,
                        resource_name: None,
                        holder: None,
                        waiting: 1,
                        detail: "waiting for structured scope".into(),
                    });
                }
            }
        }
        out
    }

    fn instances(&self, state: &HlnState) -> Vec<InstanceState> {
        let mut tokens: Vec<&ControlToken> = state.all_tokens().map(|(_, t)| t).collect();
        tokens.sort_by_key(|t| t.tid);
        tokens
            .into_iter()
            .map(|tok| {
                let (fid, function, sid) = match tok.current_frame() {
                    Some(fr) => (
                        Some(FrameId(fr.frame_id as u64)),
                        FunctionId(fr.func as u32),
                        Some(fr.pc),
                    ),
                    None => (None, FunctionId(tok.entry_func as u32), None),
                };
                let status = if state.finished.contains(&tok.tid) {
                    "finished".to_string()
                } else if let Some(tag) = &tok.wait_tag {
                    format!("blocked({tag:?})")
                } else {
                    "running".to_string()
                };
                InstanceState {
                    thread: ThreadId(tok.tid as u64),
                    frame: fid,
                    function,
                    sid,
                    status,
                }
            })
            .collect()
    }

    fn doom_snapshot(&self, state: &HlnState) -> DoomState {
        let mut threads_vec = Vec::new();
        let mut tokens: Vec<&ControlToken> = state
            .all_tokens()
            .filter(|(_, t)| !state.finished.contains(&t.tid))
            .map(|(_, t)| t)
            .collect();
        tokens.sort_by_key(|t| t.tid);

        for tok in tokens {
            let entry_f = self.program.function(FunctionId(tok.entry_func as u32));
            let entry_function = fqn::fqn(self.program.module_name(entry_f.module), &entry_f.name);
            let (function, at_sid) = match tok.current_frame() {
                Some(fr) => {
                    let f = self.program.function(FunctionId(fr.func as u32));
                    let fname = fqn::fqn(self.program.module_name(f.module), &f.name);
                    let sid = f.body.get(fr.pc).map(|s| s.sid.clone());
                    (fname, sid)
                }
                None => (entry_function.clone(), None),
            };
            let holds: Vec<String> = tok
                .held_locks
                .keys()
                .map(|&r| self.rname(ResourceId(r as u32)))
                .collect();
            let waiting_on = tok.wait_tag.as_ref().map(|tag| match tag {
                WaitTag::Mutex(r) => DoomWait {
                    kind: "mutex".into(),
                    resource: Some(self.rname(ResourceId(*r as u32))),
                },
                WaitTag::RwRead(r) => DoomWait {
                    kind: "rwlock_read".into(),
                    resource: Some(self.rname(ResourceId(*r as u32))),
                },
                WaitTag::RwWrite(r) => DoomWait {
                    kind: "rwlock_write".into(),
                    resource: Some(self.rname(ResourceId(*r as u32))),
                },
                WaitTag::Condvar { condvar, .. } => DoomWait {
                    kind: "condvar".into(),
                    resource: Some(self.rname(ResourceId(*condvar as u32))),
                },
                WaitTag::Semaphore { resource, .. } => DoomWait {
                    kind: "semaphore".into(),
                    resource: Some(self.rname(ResourceId(*resource as u32))),
                },
                WaitTag::ChannelSend { channel, .. } => DoomWait {
                    kind: "channel_send".into(),
                    resource: Some(self.rname(ResourceId(*channel as u32))),
                },
                WaitTag::ChannelRecv(channel) => DoomWait {
                    kind: "channel_recv".into(),
                    resource: Some(self.rname(ResourceId(*channel as u32))),
                },
                WaitTag::Join(_) => DoomWait {
                    kind: "join".into(),
                    resource: None,
                },
                WaitTag::Scope(_) => DoomWait {
                    kind: "scope".into(),
                    resource: None,
                },
            });
            threads_vec.push(DoomThread {
                thread: tok.tid,
                entry_function,
                function,
                at_sid,
                holds,
                waiting_on,
            });
        }

        let free_resources = self
            .program
            .resources()
            .iter()
            .filter(|r| {
                let rid = r.id.0 as usize;
                match r.kind {
                    ResKind::Mutex => state.mutexes.get(&rid).copied().flatten().is_none(),
                    ResKind::RwLock => matches!(state.rwlocks.get(&rid), Some(RwState::Free)),
                    _ => false,
                }
            })
            .map(|r| self.rname(r.id))
            .collect();

        DoomState {
            threads: threads_vec,
            free_resources,
        }
    }

    fn satisfied(&self, state: &HlnState, predicate: &Predicate) -> bool {
        match predicate {
            Predicate::True => true,
            Predicate::False => false,
            Predicate::VarEq { resource, value } => {
                state
                    .vars
                    .get(&(resource.0 as usize))
                    .map(hln_to_val)
                    .as_ref()
                    == Some(value)
            }
            Predicate::VarCmp {
                resource,
                op,
                value,
            } => state
                .vars
                .get(&(resource.0 as usize))
                .map(hln_to_val)
                .and_then(|v| compare_values(*op, &v, value))
                .unwrap_or(false),
            Predicate::FunctionCompleted { func } => {
                state
                    .completed_functions
                    .get(&(func.0 as usize))
                    .copied()
                    .unwrap_or(0)
                    >= 1
            }
            Predicate::FunctionCompletedAtLeast { func, n } => {
                state
                    .completed_functions
                    .get(&(func.0 as usize))
                    .copied()
                    .unwrap_or(0)
                    >= *n
            }
            Predicate::ScopeCompleted { func, sid } => {
                state.completed_scopes.contains(&(func.0 as usize, *sid))
            }
            Predicate::StatementReached { func, sid } => {
                state.reached.contains(&(func.0 as usize, *sid))
            }
            Predicate::MilestoneReached { sites, .. } => sites
                .iter()
                .any(|&(func, sid)| state.reached.contains(&(func.0 as usize, sid))),
            Predicate::MutexFree(r) => {
                let rid = r.0 as usize;
                match self.program.resource(*r).kind {
                    ResKind::Mutex => state.mutexes.get(&rid).copied().flatten().is_none(),
                    ResKind::RwLock => matches!(state.rwlocks.get(&rid), Some(RwState::Free)),
                    _ => false,
                }
            }
            Predicate::MutexHeld(r) => {
                let rid = r.0 as usize;
                match self.program.resource(*r).kind {
                    ResKind::Mutex => state.mutexes.get(&rid).copied().flatten().is_some(),
                    ResKind::RwLock => matches!(
                        state.rwlocks.get(&rid),
                        Some(RwState::Reading(_) | RwState::Writing(_))
                    ),
                    _ => false,
                }
            }
            Predicate::ChannelEmpty(r) => {
                let cid = r.0 as usize;
                let buf_empty = state.channels.get(&cid).map_or(true, VecDeque::is_empty);
                let send_empty = !state.all_tokens().any(|(_, t)| {
                    matches!(
                        t.wait_tag,
                        Some(WaitTag::ChannelSend { channel, .. }) if channel == cid
                    )
                });
                buf_empty && send_empty
            }
            Predicate::ChannelAtLeast { resource, len } => state
                .channels
                .get(&(resource.0 as usize))
                .map_or(false, |c| c.len() >= *len),
            Predicate::HoldsAll { func, resources } => state.all_tokens().any(|(_, t)| {
                let executes = t.stack.iter().any(|fr| fr.func == func.0 as usize);
                executes && resources.iter().all(|r| self.thread_holds(state, t, *r))
            }),
            Predicate::MutexExclusive(r) => match self.program.resource(*r).kind {
                ResKind::Mutex | ResKind::RwLock => true,
                ResKind::Semaphore => {
                    state.semaphores.get(&(r.0 as usize)).copied().unwrap_or(0) >= 0
                }
                _ => false,
            },
            Predicate::NeverHoldsAll { func, resources } => !state.all_tokens().any(|(_, t)| {
                let executes = t.stack.iter().any(|fr| fr.func == func.0 as usize);
                executes && resources.iter().all(|r| self.thread_holds(state, t, *r))
            }),
            Predicate::Not(p) => !self.satisfied(state, p),
            Predicate::And(ps) => ps.iter().all(|p| self.satisfied(state, p)),
            Predicate::Or(ps) => ps.iter().any(|p| self.satisfied(state, p)),
        }
    }

    fn canonical(&self, state: &HlnState) -> String {
        hln_state_key(state)
    }

    fn state_key(&self, state: &HlnState) -> String {
        hln_state_key(state)
    }
}
