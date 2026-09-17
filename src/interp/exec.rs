//! Reference small-step interpreter.
//!
//! Synchronization transitions here are implemented independently from the
//! Petri-net executor. The two share the resolved program, values, expression
//! evaluation, bounds, and monitor configuration — but not transition code.
//!
//! Waiting on a mutex/semaphore/channel is *enabledness*, not a forced
//! hand-off: a blocked thread can be scheduled as soon as its condition holds,
//! and every eligible waiter is an independent choice. Channel message order
//! is FIFO; waiting-thread order is not.

use crate::sem::eval::eval;
use crate::sem::ids::{FrameId, FunctionId, HandleId, ResourceId, ScopeId, SlotRef, ThreadId};
use crate::sem::monitor::MonitorConfig;
use crate::sem::outcome::{
    AnalysisBounds, BackendError, BackendResult, BoundaryEvent, BoundaryKind, Phase, StepLabel,
    TransitionOrigin,
};
use crate::sem::program::{SemOp, SemProgram};
use crate::sem::system::{
    compare_values, BlockKind, BlockedRecord, Enabled, Predicate, Step, TransitionSystem,
};
use crate::sem::value::{within_type, Value};

use super::state::{
    BlockReason, FrameView, MachineState, MutexState, PendingSend, RetAddr, ScopeState, ThreadState,
    ThreadStatus,
};

pub struct Interpreter<'a> {
    pub program: &'a SemProgram,
    pub bounds: AnalysisBounds,
    pub monitor: MonitorConfig,
}

impl<'a> Interpreter<'a> {
    pub fn new(program: &'a SemProgram, bounds: AnalysisBounds) -> Self {
        Interpreter {
            program,
            bounds,
            monitor: MonitorConfig::unbounded(),
        }
    }

    pub fn with_monitor(
        program: &'a SemProgram,
        bounds: AnalysisBounds,
        monitor: MonitorConfig,
    ) -> Self {
        Interpreter {
            program,
            bounds,
            monitor,
        }
    }

    fn eval(
        &self,
        state: &MachineState,
        frame: FrameId,
        expr: &crate::sem::eval::LExpr,
        at: &str,
    ) -> BackendResult<Value> {
        let view = FrameView {
            frame: state.frame(frame),
            store: &state.store,
        };
        eval(expr, &view, at)
    }

    fn record_completion(&self, state: &mut MachineState, function: FunctionId) {
        let max = self.monitor.max_for(function);
        if max == 0 {
            return;
        }
        let e = state.completed_functions.entry(function).or_insert(0);
        if *e < max {
            *e += 1;
        }
    }

    fn complete(&self, state: &mut MachineState, tid: ThreadId) {
        state.set_runnable(tid);
        state.advance(tid);
    }

    fn recv_dst_of(&self, state: &MachineState, tid: ThreadId) -> Option<SlotRef> {
        let fid = state.current_frame_id(tid)?;
        let pc = state.frame(fid).pc;
        let func = state.frame(fid).function;
        match &self.program.function(func).body.get(pc)?.op {
            SemOp::ChannelRecv { dst, .. } => Some(*dst),
            _ => None,
        }
    }

    fn deliver_to_recv(&self, state: &mut MachineState, recv: ThreadId, value: Value) {
        if let Some(dst) = self.recv_dst_of(state, recv) {
            if let Some(fid) = state.current_frame_id(recv) {
                let _ = state.write_dst(fid, dst, value);
            }
        }
    }

    fn finish_thread(&self, state: &mut MachineState, tid: ThreadId, function: FunctionId) {
        if let Some(t) = state.threads.get_mut(&tid) {
            t.status = ThreadStatus::Finished;
        }
        state.finished.insert(tid);
        self.record_completion(state, function);

        let scope = state.threads[&tid].parent_scope;
        if let Some(scope) = scope {
            let empty = match state.scopes.get_mut(&scope) {
                Some(sc) => {
                    sc.remaining.remove(&tid);
                    sc.remaining.is_empty()
                }
                None => false,
            };
            if empty {
                let (owner, owner_frame) = {
                    let sc = &state.scopes[&scope];
                    (sc.owner, sc.owner_frame)
                };
                let sid = state.frame(owner_frame).pc;
                let ofn = state.frame(owner_frame).function;
                state.completed_scopes.insert((ofn, sid));
                state.set_runnable(owner);
                state.advance(owner);
            }
        }

        let joiners: Vec<ThreadId> = state
            .threads
            .iter()
            .filter(|(_, t)| match &t.status {
                ThreadStatus::Blocked(BlockReason::Join(h)) => {
                    t.handle_children.get(h) == Some(&tid)
                }
                _ => false,
            })
            .map(|(id, _)| *id)
            .collect();
        for j in joiners {
            self.complete(state, j);
        }
    }

    fn within_function_bound(&self, state: &MachineState, func: FunctionId) -> bool {
        let Some(bound) = self.program.function(func).bound else {
            return true;
        };
        let active = state
            .threads
            .values()
            .filter(|t| {
                t.status != ThreadStatus::Finished
                    && t.stack.first().map(|fid| state.frame(*fid).function) == Some(func)
            })
            .count();
        active < bound.max(0) as usize
    }

    fn create_thread(
        &self,
        state: &mut MachineState,
        func: FunctionId,
        parent_scope: Option<ScopeId>,
        boundary: &mut Vec<BoundaryEvent>,
    ) -> BackendResult<Option<ThreadId>> {
        let live = state
            .threads
            .values()
            .filter(|t| t.status != ThreadStatus::Finished)
            .count();
        if live >= self.bounds.max_threads {
            boundary.push(BoundaryEvent::new(
                BoundaryKind::ThreadLimit,
                format!("thread limit {} reached while spawning", self.bounds.max_threads),
            ));
            return Ok(None);
        }
        let tid = ThreadId(state.alloc.next_thread);
        state.alloc.next_thread += 1;
        let f = self.program.function(func);
        if f.is_transparent_nobody() {
            state.threads.insert(
                tid,
                ThreadState {
                    id: tid,
                    status: ThreadStatus::Finished,
                    stack: Vec::new(),
                    entry_function: func,
                    handle_children: Default::default(),
                    parent_scope,
                },
            );
            state.finished.insert(tid);
            self.record_completion(state, func);
            return Ok(Some(tid));
        }
        let frame = state.alloc_frame(self.program, func)?;
        let fid = frame.id;
        state.store.write_frame(fid, frame);
        state.threads.insert(
            tid,
            ThreadState {
                id: tid,
                status: ThreadStatus::Runnable,
                stack: vec![fid],
                entry_function: func,
                handle_children: Default::default(),
                parent_scope,
            },
        );
        Ok(Some(tid))
    }

    fn step_label(&self, function: FunctionId, sid: usize, tid: ThreadId, fid: FrameId) -> StepLabel {
        StepLabel::new(TransitionOrigin {
            module: self.program.function(function).module,
            function,
            sid: Some(sid),
            phase: Phase::Statement,
        })
        .with_binding(tid, fid)
    }

    /// A step for a runnable thread.
    fn step_thread(
        &self,
        state: &MachineState,
        tid: ThreadId,
        boundary: &mut Vec<BoundaryEvent>,
    ) -> BackendResult<Vec<Step<MachineState>>> {
        let mut next = state.clone();
        let fid = next
            .current_frame_id(tid)
            .ok_or_else(|| BackendError::invalid("E999", format!("thread {tid} has no frame")))?;
        let function_id = next.frame(fid).function;
        let pc = next.frame(fid).pc;
        let function = self.program.function(function_id);
        if pc >= function.body.len() {
            return Err(BackendError::invalid(
                "E602",
                format!("thread {tid} fell off the end of '{}'", function.name),
            ));
        }
        next.reached.insert((function_id, pc));
        let stmt = &function.body[pc];
        let at = self.program.location(function_id, Some(pc));
        let label = self.step_label(function_id, pc, tid, fid);
        let op = stmt.op.clone();

        // notify_one has one successor per eligible waiter.
        if let SemOp::CondvarNotify { condvar } = op {
            return self.step_notify(state, tid, function_id, pc, condvar);
        }

        match op {
            SemOp::Nop => {
                next.advance(tid);
            }
            SemOp::AssignLocal { target, expr } => {
                let v = self.eval(&next, fid, &expr, &at)?;
                if let SlotRef::Local(slot) = target {
                    if !within_type(&v, &function.slots[slot].ty) {
                        return Ok(Vec::new());
                    }
                }
                next.write_dst(fid, target, v)?;
                next.advance(tid);
            }
            SemOp::ReadShared { resource, dst } => {
                let v = next.store.read_shared(resource).cloned().ok_or_else(|| {
                    BackendError::invalid("E900", format!("Var {resource} has no value"))
                })?;
                if let Some(dst) = dst {
                    next.write_dst(fid, dst, v)?;
                }
                next.advance(tid);
            }
            SemOp::WriteShared { resource, expr } => {
                let v = self.eval(&next, fid, &expr, &at)?;
                let ty = self.program.resource(resource).ty.clone().unwrap();
                if !within_type(&v, &ty) {
                    return Ok(Vec::new());
                }
                next.store.vars.insert(resource, v);
                next.advance(tid);
            }
            SemOp::AtomicLoad { resource, dst } => {
                let v = next.store.atomics.get(&resource).cloned().ok_or_else(|| {
                    BackendError::invalid("E900", format!("Atomic {resource} has no value"))
                })?;
                next.write_dst(fid, dst, v)?;
                next.advance(tid);
            }
            SemOp::AtomicStore { resource, value } => {
                let v = self.eval(&next, fid, &value, &at)?;
                let ty = self.program.resource(resource).ty.clone().unwrap();
                if !within_type(&v, &ty) {
                    return Ok(Vec::new());
                }
                next.store.atomics.insert(resource, v);
                next.advance(tid);
            }
            SemOp::AtomicCas {
                resource,
                expected,
                desired,
                dst,
            } => {
                let exp = self.eval(&next, fid, &expected, &at)?;
                let des = self.eval(&next, fid, &desired, &at)?;
                let ty = self.program.resource(resource).ty.clone().unwrap();
                if !within_type(&des, &ty) {
                    return Ok(Vec::new());
                }
                let old = next.store.atomics.get(&resource).cloned().ok_or_else(|| {
                    BackendError::invalid("E900", format!("Atomic {resource} has no value"))
                })?;
                next.write_dst(fid, dst, old.clone())?;
                if old == exp {
                    next.store.atomics.insert(resource, des);
                }
                next.advance(tid);
            }
            SemOp::MutexLock { resource } => {
                if matches!(next.store.mutexes.get(&resource), Some(MutexState::Free)) {
                    next.store.mutexes.insert(resource, MutexState::Held(tid));
                    next.advance(tid);
                } else {
                    next.block(tid, BlockReason::Lock(resource));
                }
            }
            SemOp::MutexUnlock { resource } => {
                match next.store.mutexes.get(&resource) {
                    Some(MutexState::Held(owner)) if *owner == tid => {}
                    Some(MutexState::Held(_)) => {
                        return Err(BackendError::invalid(
                            "E510",
                            format!("thread {tid} unlocked Mutex {resource} it does not hold"),
                        )
                        .at(&at))
                    }
                    _ => {
                        return Err(BackendError::invalid(
                            "E510",
                            format!("thread {tid} unlocked free Mutex {resource}"),
                        )
                        .at(&at))
                    }
                }
                next.store.mutexes.insert(resource, MutexState::Free);
                next.advance(tid);
            }
            SemOp::ChannelSend { channel, value } => {
                let v = self.eval(&next, fid, &value, &at)?;
                let cap = self.program.resource(channel).capacity;
                if cap == 0 {
                    // Rendezvous: pair with the oldest waiting receiver, if any.
                    let recv = next
                        .store
                        .channels
                        .get_mut(&channel)
                        .and_then(|c| c.pending_recv.pop_front());
                    if let Some(r) = recv {
                        self.deliver_to_recv(&mut next, r, v);
                        self.complete(&mut next, r);
                        next.advance(tid);
                    } else {
                        next.store
                            .channels
                            .get_mut(&channel)
                            .unwrap()
                            .pending_send
                            .push_back(PendingSend { thread: tid, value: v });
                        next.block(tid, BlockReason::ChannelSend(channel));
                    }
                } else {
                    let has_space = next
                        .store
                        .channels
                        .get(&channel)
                        .map(|c| c.buffer.len() < cap)
                        .unwrap_or(false);
                    if has_space {
                        next.store
                            .channels
                            .get_mut(&channel)
                            .unwrap()
                            .buffer
                            .push_back(v);
                        next.advance(tid);
                    } else {
                        next.store
                            .channels
                            .get_mut(&channel)
                            .unwrap()
                            .pending_send
                            .push_back(PendingSend { thread: tid, value: v });
                        next.block(tid, BlockReason::ChannelSend(channel));
                    }
                }
            }
            SemOp::ChannelRecv { channel, .. } => {
                let cap = self.program.resource(channel).capacity;
                if cap == 0 {
                    let send = next
                        .store
                        .channels
                        .get_mut(&channel)
                        .and_then(|c| c.pending_send.pop_front());
                    if let Some(s) = send {
                        self.deliver_to_recv(&mut next, tid, s.value);
                        self.complete(&mut next, s.thread);
                        next.advance(tid);
                    } else {
                        next.store
                            .channels
                            .get_mut(&channel)
                            .unwrap()
                            .pending_recv
                            .push_back(tid);
                        next.block(tid, BlockReason::ChannelRecv(channel));
                    }
                } else {
                    let value = next
                        .store
                        .channels
                        .get_mut(&channel)
                        .and_then(|c| c.buffer.pop_front());
                    match value {
                        Some(v) => {
                            let dst = self.recv_dst_of(&next, tid).unwrap_or(SlotRef::Discard);
                            next.write_dst(fid, dst, v)?;
                            next.advance(tid);
                        }
                        None => {
                            next.store
                                .channels
                                .get_mut(&channel)
                                .unwrap()
                                .pending_recv
                                .push_back(tid);
                            next.block(tid, BlockReason::ChannelRecv(channel));
                        }
                    }
                }
            }
            SemOp::CondvarWait { condvar, lock } => {
                match next.store.mutexes.get(&lock) {
                    Some(MutexState::Held(owner)) if *owner == tid => {}
                    _ => {
                        return Err(BackendError::invalid(
                            "E511",
                            format!("thread {tid} waited on Condvar {condvar} without holding Mutex {lock}"),
                        )
                        .at(&at))
                    }
                }
                next.store.mutexes.insert(lock, MutexState::Free);
                {
                    let cv = next.store.condvars.get_mut(&condvar).unwrap();
                    cv.waiters.push_back(tid);
                    cv.lock = Some(lock);
                }
                next.block(tid, BlockReason::Condvar(condvar, lock));
            }
            SemOp::CondvarNotify { .. } => unreachable!("handled above"),
            SemOp::CondvarNotifyAll { condvar } => {
                let lock = next.store.condvars.get(&condvar).and_then(|c| c.lock);
                let waiters: Vec<ThreadId> = {
                    let cv = next.store.condvars.get_mut(&condvar).unwrap();
                    cv.waiters.drain(..).collect()
                };
                if let Some(lock) = lock {
                    for w in waiters {
                        next.block(w, BlockReason::Lock(lock));
                    }
                }
                next.advance(tid);
            }
            SemOp::SemaphoreAcquire { resource, count } => {
                if count <= 0 {
                    return Err(BackendError::invalid("E904", "semaphore count must be positive").at(&at));
                }
                let available = *next.store.semaphores.get(&resource).unwrap_or(&0);
                if available >= count {
                    next.store.semaphores.insert(resource, available - count);
                    next.advance(tid);
                } else {
                    next.sem_waiters
                        .entry(resource)
                        .or_default()
                        .push_back((tid, count));
                    next.block(tid, BlockReason::Semaphore(resource));
                }
            }
            SemOp::SemaphoreRelease { resource, count } => {
                if count <= 0 {
                    return Err(BackendError::invalid("E904", "semaphore count must be positive").at(&at));
                }
                let available = *next.store.semaphores.get(&resource).unwrap_or(&0);
                next.store.semaphores.insert(resource, available + count);
                next.advance(tid);
            }
            SemOp::Call { func, args, dst } => {
                let callee = self.program.function(func);
                if callee.is_transparent_nobody() {
                    self.record_completion(&mut next, func);
                    next.advance(tid);
                } else {
                    if next.threads[&tid].stack.len() >= self.bounds.max_frames_per_thread {
                        boundary.push(BoundaryEvent::new(
                            BoundaryKind::FrameLimit,
                            format!("frame limit {} reached while calling '{}'", self.bounds.max_frames_per_thread, callee.name),
                        ));
                        return Ok(Vec::new());
                    }
                    let mut values = Vec::with_capacity(args.len());
                    for a in &args {
                        values.push(self.eval(&next, fid, a, &at)?);
                    }
                    let modeled: Vec<usize> = callee
                        .slots
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.class == crate::sem::program::SlotClass::Param && s.modeled)
                        .map(|(i, _)| i)
                        .collect();
                    let mut frame = next.alloc_frame(self.program, func)?;
                    for (slot, val) in modeled.iter().zip(values) {
                        frame.locals.insert(*slot, val);
                    }
                    frame.ret = Some(RetAddr {
                        pc_next: pc + 1,
                        dst: dst.unwrap_or(SlotRef::Discard),
                    });
                    let cfid = frame.id;
                    next.store.write_frame(cfid, frame);
                    next.threads.get_mut(&tid).unwrap().stack.push(cfid);
                }
            }
            SemOp::Spawn { func, handle } => {
                if !self.within_function_bound(&next, func) {
                    return Ok(Vec::new());
                }
                let Some(child) = self.create_thread(&mut next, func, None, boundary)? else {
                    return Ok(Vec::new());
                };
                let hid = HandleId(next.alloc.next_handle);
                next.alloc.next_handle += 1;
                // Bind the name in the *current frame*, not the thread.
                next.frame_mut(fid).handles.insert(handle, hid);
                next.threads
                    .get_mut(&tid)
                    .unwrap()
                    .handle_children
                    .insert(hid, child);
                next.advance(tid);
            }
            SemOp::Scope { funcs } => {
                if funcs.is_empty() {
                    return Err(BackendError::invalid("E410", "scope funcs is empty").at(&at));
                }
                for func in &funcs {
                    if !self.within_function_bound(&next, *func) {
                        return Ok(Vec::new());
                    }
                }
                let scope = ScopeId(next.alloc.next_scope);
                next.alloc.next_scope += 1;
                let mut remaining = std::collections::BTreeSet::new();
                for func in &funcs {
                    let Some(child) = self.create_thread(&mut next, *func, Some(scope), boundary)?
                    else {
                        return Ok(Vec::new());
                    };
                    if next.threads[&child].status != ThreadStatus::Finished {
                        remaining.insert(child);
                    }
                }
                if remaining.is_empty() {
                    next.completed_scopes.insert((function_id, pc));
                    next.advance(tid);
                } else {
                    next.scopes.insert(
                        scope,
                        ScopeState {
                            id: scope,
                            owner: tid,
                            owner_frame: fid,
                            remaining,
                        },
                    );
                    next.block(tid, BlockReason::Scope(scope));
                }
            }
            SemOp::Join { handle } => {
                let hid = next.frame(fid).handles.get(&handle).copied().ok_or_else(|| {
                    BackendError::invalid("E402", format!("join handle '{handle}' was never spawned in this activation")).at(&at)
                })?;
                let child = next.threads[&tid].handle_children.get(&hid).copied();
                match child {
                    Some(c) if next.finished.contains(&c) => next.advance(tid),
                    Some(_) => next.block(tid, BlockReason::Join(hid)),
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
                next.frame_mut(fid).pc = target;
            }
            SemOp::Branch {
                cond,
                then,
                else_target,
            } => {
                let v = self.eval(&next, fid, &cond, &at)?;
                let take_then = v.as_bool().ok_or_else(|| {
                    BackendError::invalid("E201", "branch condition is not Bool").at(&at)
                })?;
                next.frame_mut(fid).pc = if take_then { then } else { else_target };
            }
            SemOp::Switch {
                var,
                cases,
                default,
            } => {
                let v = self.eval(&next, fid, &var, &at)?;
                let target = match &v {
                    Value::Enum(e) => cases.get(e).copied().unwrap_or(default),
                    Value::Int(i) => cases
                        .iter()
                        .find_map(|(k, t)| {
                            if k.parse::<i64>().ok() == Some(*i) {
                                Some(*t)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(default),
                    other => {
                        return Err(BackendError::invalid(
                            "E202",
                            format!("switch on non-Int/non-Enum value {}", other.canonical()),
                        )
                        .at(&at))
                    }
                };
                next.frame_mut(fid).pc = target;
            }
            SemOp::Return { value } => {
                let val = match &value {
                    Some(e) => Some(self.eval(&next, fid, e, &at)?),
                    None => None,
                };
                if next.threads[&tid].stack.len() > 1 {
                    let callee_frame = next.frame(fid).clone();
                    next.threads.get_mut(&tid).unwrap().stack.pop();
                    next.store.frames.remove(&fid);
                    let caller = *next.threads[&tid].stack.last().unwrap();
                    if let Some(ret) = &callee_frame.ret {
                        if let Some(v) = val {
                            next.write_dst(caller, ret.dst, v)?;
                        }
                        next.frame_mut(caller).pc = ret.pc_next;
                    }
                    self.record_completion(&mut next, function_id);
                } else {
                    next.threads.get_mut(&tid).unwrap().stack.pop();
                    next.store.frames.remove(&fid);
                    self.finish_thread(&mut next, tid, function_id);
                }
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

    /// notify_one: one successor per current waiter (plus one when there is
    /// none). The enumeration order is deterministic; the *choice* is not
    /// removed.
    fn step_notify(
        &self,
        state: &MachineState,
        tid: ThreadId,
        function_id: FunctionId,
        pc: usize,
        condvar: ResourceId,
    ) -> BackendResult<Vec<Step<MachineState>>> {
        let fid = state.current_frame_id(tid).unwrap();
        let label = self.step_label(function_id, pc, tid, fid);
        let waiters: Vec<ThreadId> = state
            .store
            .condvars
            .get(&condvar)
            .map(|c| c.waiters.iter().copied().collect())
            .unwrap_or_default();
        let lock = state.store.condvars.get(&condvar).and_then(|c| c.lock);

        if waiters.is_empty() {
            let mut next = state.clone();
            next.reached.insert((function_id, pc));
            next.advance(tid);
            return Ok(vec![Step { label, state: next }]);
        }
        let mut out = Vec::new();
        for w in waiters {
            let mut next = state.clone();
            next.reached.insert((function_id, pc));
            if let Some(cv) = next.store.condvars.get_mut(&condvar) {
                cv.waiters.retain(|x| *x != w);
            }
            if let Some(lock) = lock {
                next.block(w, BlockReason::Lock(lock));
            }
            next.advance(tid);
            out.push(Step {
                label: label.clone(),
                state: next,
            });
        }
        Ok(out)
    }

    /// Resume a thread blocked on a resource whose condition now holds.
    fn resume_thread(
        &self,
        state: &MachineState,
        tid: ThreadId,
        reason: &BlockReason,
    ) -> BackendResult<Vec<Step<MachineState>>> {
        let mut next = state.clone();
        let fid = next
            .current_frame_id(tid)
            .ok_or_else(|| BackendError::invalid("E999", format!("thread {tid} has no frame")))?;
        let function_id = next.frame(fid).function;
        let pc = next.frame(fid).pc;
        let label = self.step_label(function_id, pc, tid, fid);

        match reason {
            BlockReason::Lock(r) => {
                if !matches!(next.store.mutexes.get(r), Some(MutexState::Free)) {
                    return Ok(Vec::new());
                }
                next.store.mutexes.insert(*r, MutexState::Held(tid));
                next.set_runnable(tid);
                next.advance(tid);
            }
            BlockReason::Semaphore(r) => {
                let need = next
                    .sem_waiters
                    .get(r)
                    .and_then(|q| q.iter().find(|(t, _)| *t == tid).map(|(_, n)| *n));
                let Some(need) = need else { return Ok(Vec::new()) };
                let available = *next.store.semaphores.get(r).unwrap_or(&0);
                if available < need {
                    return Ok(Vec::new());
                }
                next.store.semaphores.insert(*r, available - need);
                if let Some(q) = next.sem_waiters.get_mut(r) {
                    q.retain(|(t, _)| *t != tid);
                }
                next.set_runnable(tid);
                next.advance(tid);
            }
            BlockReason::ChannelSend(c) => {
                let cap = self.program.resource(*c).capacity;
                if cap == 0 {
                    // Should not normally occur (the second arrival pairs),
                    // but handle it: pair with the oldest receiver if any.
                    let recv = next
                        .store
                        .channels
                        .get_mut(c)
                        .and_then(|ch| ch.pending_recv.pop_front());
                    let Some(recv) = recv else { return Ok(Vec::new()) };
                    let value = match next
                        .store
                        .channels
                        .get_mut(c)
                        .and_then(|ch| ch.pending_send.iter().position(|p| p.thread == tid))
                    {
                        Some(pos) => next
                            .store
                            .channels
                            .get_mut(c)
                            .unwrap()
                            .pending_send
                            .remove(pos)
                            .map(|p| p.value)
                            .unwrap(),
                        None => return Ok(Vec::new()),
                    };
                    self.deliver_to_recv(&mut next, recv, value);
                    self.complete(&mut next, recv);
                    next.set_runnable(tid);
                next.advance(tid);
                } else {
                    let has_space = next
                        .store
                        .channels
                        .get(c)
                        .map(|ch| ch.buffer.len() < cap)
                        .unwrap_or(false);
                    if !has_space {
                        return Ok(Vec::new());
                    }
                    let pos = next
                        .store
                        .channels
                        .get(c)
                        .and_then(|ch| ch.pending_send.iter().position(|p| p.thread == tid));
                    let Some(pos) = pos else { return Ok(Vec::new()) };
                    let send = next
                        .store
                        .channels
                        .get_mut(c)
                        .unwrap()
                        .pending_send
                        .remove(pos)
                        .unwrap();
                    next.store
                        .channels
                        .get_mut(c)
                        .unwrap()
                        .buffer
                        .push_back(send.value);
                    next.set_runnable(tid);
                next.advance(tid);
                }
            }
            BlockReason::ChannelRecv(c) => {
                let cap = self.program.resource(*c).capacity;
                if cap == 0 {
                    let send = next
                        .store
                        .channels
                        .get_mut(c)
                        .and_then(|ch| ch.pending_send.pop_front());
                    let Some(send) = send else { return Ok(Vec::new()) };
                    self.deliver_to_recv(&mut next, tid, send.value);
                    self.complete(&mut next, send.thread);
                    next.set_runnable(tid);
                next.advance(tid);
                } else {
                    let value = next
                        .store
                        .channels
                        .get_mut(c)
                        .and_then(|ch| ch.buffer.pop_front());
                    let Some(v) = value else { return Ok(Vec::new()) };
                    let dst = self.recv_dst_of(&next, tid).unwrap_or(SlotRef::Discard);
                    next.write_dst(fid, dst, v)?;
                    if let Some(ch) = next.store.channels.get_mut(c) {
                        ch.pending_recv.retain(|t| *t != tid);
                    }
                    next.set_runnable(tid);
                next.advance(tid);
                }
            }
            // Condvar waiters are resumed by `notify`; join/scope by completion.
            BlockReason::Condvar(_, _) | BlockReason::Join(_) | BlockReason::Scope(_) => {
                return Ok(Vec::new());
            }
        }

        Ok(vec![Step { label, state: next }])
    }
}

impl<'a> TransitionSystem for Interpreter<'a> {
    type State = MachineState;

    fn program(&self) -> &SemProgram {
        self.program
    }

    fn initial(&self) -> BackendResult<MachineState> {
        let mut state = MachineState::initial(self.program)?;
        // Drop the transparent-entry completion if the contract does not
        // observe it.
        let entry = self.program.entry();
        if self.monitor.max_for(entry) == 0 {
            state.completed_functions.remove(&entry);
        }
        Ok(state)
    }

    fn successors(&self, state: &MachineState) -> BackendResult<Enabled<MachineState>> {
        let mut enabled = Enabled::empty();
        for tid in state.threads.keys().copied().collect::<Vec<_>>() {
            match &state.threads[&tid].status {
                ThreadStatus::Runnable => {
                    for step in self.step_thread(state, tid, &mut enabled.boundary)? {
                        enabled.steps.push(step);
                    }
                }
                ThreadStatus::Blocked(reason) => {
                    for step in self.resume_thread(state, tid, reason)? {
                        enabled.steps.push(step);
                    }
                }
                ThreadStatus::Finished => {}
            }
        }
        Ok(enabled)
    }

    fn is_finished(&self, state: &MachineState) -> bool {
        state
            .threads
            .values()
            .all(|t| t.status == ThreadStatus::Finished)
    }

    fn blocked(&self, state: &MachineState) -> Vec<BlockedRecord> {
        let mut out = Vec::new();
        let count_lock = |r: &ResourceId| -> usize {
            state
                .threads
                .values()
                .filter(|t| matches!(&t.status, ThreadStatus::Blocked(BlockReason::Lock(x)) if x == r))
                .count()
        };
        for t in state.threads.values() {
            if let ThreadStatus::Blocked(reason) = &t.status {
                out.push(match reason {
                    BlockReason::Lock(r) => BlockedRecord {
                        thread: t.id,
                        kind: BlockKind::Lock,
                        resource: Some(*r),
                        holder: match state.store.mutexes.get(r) {
                            Some(MutexState::Held(h)) => Some(*h),
                            _ => None,
                        },
                        waiting: count_lock(r),
                        detail: "waiting for mutex".into(),
                    },
                    BlockReason::ChannelSend(c) => BlockedRecord {
                        thread: t.id,
                        kind: BlockKind::ChannelSend,
                        resource: Some(*c),
                        holder: None,
                        waiting: state
                            .store
                            .channels
                            .get(c)
                            .map(|ch| ch.pending_send.len())
                            .unwrap_or(0),
                        detail: "channel send blocked (buffer full / no receiver)".into(),
                    },
                    BlockReason::ChannelRecv(c) => BlockedRecord {
                        thread: t.id,
                        kind: BlockKind::ChannelRecv,
                        resource: Some(*c),
                        holder: None,
                        waiting: state
                            .store
                            .channels
                            .get(c)
                            .map(|ch| ch.pending_recv.len())
                            .unwrap_or(0),
                        detail: "channel receive blocked (empty)".into(),
                    },
                    BlockReason::Condvar(cv, lk) => BlockedRecord {
                        thread: t.id,
                        kind: BlockKind::Condvar,
                        resource: Some(*cv),
                        holder: match state.store.mutexes.get(lk) {
                            Some(MutexState::Held(h)) => Some(*h),
                            _ => None,
                        },
                        waiting: state
                            .store
                            .condvars
                            .get(cv)
                            .map(|c| c.waiters.len())
                            .unwrap_or(0),
                        detail: "waiting on condvar to be notified".into(),
                    },
                    BlockReason::Semaphore(r) => BlockedRecord {
                        thread: t.id,
                        kind: BlockKind::Semaphore,
                        resource: Some(*r),
                        holder: None,
                        waiting: state.sem_waiters.get(r).map(|q| q.len()).unwrap_or(0),
                        detail: "waiting for semaphore permits".into(),
                    },
                    BlockReason::Join(_) => BlockedRecord {
                        thread: t.id,
                        kind: BlockKind::Join,
                        resource: None,
                        holder: None,
                        waiting: 0,
                        detail: "waiting to join a child thread".into(),
                    },
                    BlockReason::Scope(_) => BlockedRecord {
                        thread: t.id,
                        kind: BlockKind::Scope,
                        resource: None,
                        holder: None,
                        waiting: 0,
                        detail: "waiting for scope members".into(),
                    },
                });
            }
        }
        out
    }

    fn instances(&self, state: &MachineState) -> Vec<crate::sem::system::InstanceState> {
        let mut out = Vec::new();
        for t in state.threads.values() {
            let (function, sid, status) = match t.status {
                ThreadStatus::Finished => (t.entry_function, None, "finished".to_string()),
                _ => match t.current_frame() {
                    Some(fid) => {
                        let f = state.frame(fid);
                        (f.function, Some(f.pc), "running".to_string())
                    }
                    None => (t.entry_function, None, "running".to_string()),
                },
            };
            out.push(crate::sem::system::InstanceState {
                thread: t.id,
                frame: t.current_frame(),
                function,
                sid,
                status,
            });
        }
        out
    }

    fn satisfied(&self, state: &MachineState, predicate: &Predicate) -> bool {
        match predicate {
            Predicate::True => true,
            Predicate::False => false,
            Predicate::VarEq { resource, value } => {
                state.store.read_shared(*resource) == Some(value)
            }
            Predicate::VarCmp { resource, op, value } => state
                .store
                .read_shared(*resource)
                .and_then(|v| compare_values(*op, v, value))
                .unwrap_or(false),
            Predicate::FunctionCompleted { func } => {
                state.completed_functions.get(func).copied().unwrap_or(0) >= 1
            }
            Predicate::FunctionCompletedAtLeast { func, n } => {
                state.completed_functions.get(func).copied().unwrap_or(0) >= *n
            }
            Predicate::ScopeCompleted { func, sid } => {
                state.completed_scopes.contains(&(*func, *sid))
            }
            Predicate::StatementReached { func, sid } => state.reached.contains(&(*func, *sid)),
            Predicate::MutexFree(r) => {
                matches!(state.store.mutexes.get(r), Some(MutexState::Free))
            }
            Predicate::MutexHeld(r) => {
                matches!(state.store.mutexes.get(r), Some(MutexState::Held(_)))
            }
            Predicate::ChannelEmpty(r) => state
                .store
                .channels
                .get(r)
                .map(|c| c.buffer.is_empty() && c.pending_send.is_empty())
                .unwrap_or(true),
            Predicate::ChannelAtLeast { resource, len } => state
                .store
                .channels
                .get(resource)
                .map(|c| c.buffer.len() >= *len)
                .unwrap_or(false),
            Predicate::Not(p) => !self.satisfied(state, p),
            Predicate::And(ps) => ps.iter().all(|p| self.satisfied(state, p)),
            Predicate::Or(ps) => ps.iter().any(|p| self.satisfied(state, p)),
        }
    }

    fn canonical(&self, state: &MachineState) -> String {
        render_state(self.program, state)
    }
}

// ───────────────────────── Canonical rendering ─────────────────────────

fn render_state(program: &SemProgram, state: &MachineState) -> String {
    let thread_order: Vec<ThreadId> = state.threads.keys().copied().collect();
    let frame_order: Vec<FrameId> = state.store.frames.keys().copied().collect();
    let tname = |t: ThreadId| -> String {
        thread_order
            .iter()
            .position(|x| *x == t)
            .map(|i| format!("T{i}"))
            .unwrap_or_else(|| format!("T?{}", t.0))
    };
    let fname = |f: FrameId| -> String {
        frame_order
            .iter()
            .position(|x| *x == f)
            .map(|i| format!("F{i}"))
            .unwrap_or_else(|| format!("F?{}", f.0))
    };

    let mut out = String::new();
    out.push_str("STORE\n");
    for (r, v) in &state.store.vars {
        out.push_str(&format!("  var r{}={}\n", r.0, v.canonical()));
    }
    for (r, v) in &state.store.atomics {
        out.push_str(&format!("  atomic r{}={}\n", r.0, v.canonical()));
    }
    for (r, m) in &state.store.mutexes {
        let s = match m {
            MutexState::Free => "free".to_string(),
            MutexState::Held(t) => format!("held({})", tname(*t)),
        };
        out.push_str(&format!("  mutex r{}={}\n", r.0, s));
    }
    for (r, n) in &state.store.semaphores {
        out.push_str(&format!("  sem r{}={}\n", r.0, n));
    }
    for (r, c) in &state.store.channels {
        out.push_str(&format!(
            "  chan r{} buf=[{}] send=[{}] recv=[{}]\n",
            r.0,
            c.buffer.iter().map(Value::canonical).collect::<Vec<_>>().join(","),
            c.pending_send
                .iter()
                .map(|p| format!("{}:{}", tname(p.thread), p.value.canonical()))
                .collect::<Vec<_>>()
                .join(","),
            c.pending_recv.iter().map(|t| tname(*t)).collect::<Vec<_>>().join(","),
        ));
    }
    for (r, c) in &state.store.condvars {
        out.push_str(&format!(
            "  condvar r{} waiters=[{}] lock={:?}\n",
            r.0,
            c.waiters.iter().map(|t| tname(*t)).collect::<Vec<_>>().join(","),
            c.lock.map(|l| l.0)
        ));
    }
    for f in &frame_order {
        let frame = &state.store.frames[f];
        let locals: Vec<String> = frame
            .locals
            .iter()
            .map(|(k, v)| format!("{k}={}", v.canonical()))
            .collect();
        let handles: Vec<String> = frame
            .handles
            .iter()
            .map(|(k, h)| format!("{k}->h{}", h.0))
            .collect();
        out.push_str(&format!(
            "  frame {} func=f{} pc={} locals={{{}}} handles={{{}}}\n",
            fname(*f),
            frame.function.0,
            frame.pc,
            locals.join(","),
            handles.join(",")
        ));
    }
    out.push_str("THREADS\n");
    for t in &thread_order {
        let th = &state.threads[t];
        let stack: Vec<String> = th.stack.iter().map(|f| fname(*f)).collect();
        let status = match &th.status {
            ThreadStatus::Runnable => "run".to_string(),
            ThreadStatus::Finished => "done".to_string(),
            ThreadStatus::Blocked(b) => format!("{b:?}"),
        };
        out.push_str(&format!(
            "  {} status={} stack=[{}] scope={:?}\n",
            tname(*t),
            status,
            stack.join(","),
            th.parent_scope.map(|s| s.0)
        ));
    }
    for (r, q) in &state.sem_waiters {
        out.push_str(&format!(
            "  semq r{}=[{}]\n",
            r.0,
            q.iter().map(|(t, n)| format!("{}:{}", tname(*t), n)).collect::<Vec<_>>().join(",")
        ));
    }
    for s in state.scopes.values() {
        out.push_str(&format!(
            "  scope {} owner={} remaining=[{}]\n",
            s.id.0,
            tname(s.owner),
            s.remaining.iter().map(|t| tname(*t)).collect::<Vec<_>>().join(",")
        ));
    }
    out.push_str(&format!(
        "COMPLETED funcs={:?} scopes={:?}\n",
        state.completed_functions, state.completed_scopes
    ));
    out.push_str(&format!("REACHED {:?}\n", state.reached));
    let _ = program;
    out
}
