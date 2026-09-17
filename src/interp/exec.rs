//! Reference small-step interpreter.
//!
//! Synchronization transitions here are implemented independently from the
//! Petri-net executor. The two share the resolved program, values, expression
//! evaluation, and bounds — but not the transition code.

use crate::sem::eval::eval;
use crate::sem::ids::{FrameId, FunctionId, HandleId, ResourceId, ScopeId, SlotRef, ThreadId};
use crate::sem::outcome::{
    BackendError, BackendResult, BoundaryEvent, BoundaryKind, Phase, StepLabel, TransitionOrigin,
};
use crate::sem::program::{SemOp, SemProgram};
use crate::sem::system::{
    compare_values, BlockKind, BlockedRecord, Enabled, Predicate, Step, TransitionSystem,
};
use crate::sem::value::{within_type, Value};

use super::state::{
    BlockReason, FrameView, MachineState, MutexState, PendingSend, ScopeState, ThreadState,
    ThreadStatus,
};

pub struct Interpreter<'a> {
    pub program: &'a SemProgram,
    pub bounds: crate::sem::outcome::AnalysisBounds,
}

impl<'a> Interpreter<'a> {
    pub fn new(program: &'a SemProgram, bounds: crate::sem::outcome::AnalysisBounds) -> Self {
        Interpreter { program, bounds }
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

    fn grant_lock(&self, state: &mut MachineState, r: ResourceId) {
        if !matches!(state.store.mutexes.get(&r), Some(MutexState::Free)) {
            return;
        }
        let waiter = state
            .mutex_waiters
            .get_mut(&r)
            .and_then(|q| q.pop_front());
        if let Some(w) = waiter {
            state.store.mutexes.insert(r, MutexState::Held(w));
            state.set_runnable(w);
            state.advance(w);
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

    /// Drain a channel: move pending sends into buffered space, pair
    /// rendezvous sides, and hand buffered values to waiting receivers.
    fn drain_channel(&self, state: &mut MachineState, chan: ResourceId) {
        let cap = self.program.resource(chan).capacity;
        if cap > 0 {
            loop {
                let mut progress = false;
                // Admit blocked senders while there is space.
                loop {
                    let has_space = state
                        .store
                        .channels
                        .get(&chan)
                        .map(|c| c.buffer.len() < cap)
                        .unwrap_or(false);
                    if !has_space {
                        break;
                    }
                    let send = state
                        .store
                        .channels
                        .get_mut(&chan)
                        .and_then(|c| c.pending_send.pop_front());
                    let Some(send) = send else { break };
                    state
                        .store
                        .channels
                        .get_mut(&chan)
                        .unwrap()
                        .buffer
                        .push_back(send.value);
                    self.complete(state, send.thread);
                    progress = true;
                }
                // Deliver buffered values to waiting receivers.
                loop {
                    let value = state
                        .store
                        .channels
                        .get_mut(&chan)
                        .and_then(|c| c.buffer.pop_front());
                    let Some(value) = value else { break };
                    let recv = state
                        .store
                        .channels
                        .get_mut(&chan)
                        .and_then(|c| c.pending_recv.pop_front());
                    match recv {
                        Some(r) => {
                            if let Some(dst) = self.recv_dst_of(state, r) {
                                if let Some(fid) = state.current_frame_id(r) {
                                    let _ = state.write_dst(fid, dst, value);
                                }
                            }
                            self.complete(state, r);
                            progress = true;
                        }
                        None => {
                            state
                                .store
                                .channels
                                .get_mut(&chan)
                                .unwrap()
                                .buffer
                                .push_front(value);
                            break;
                        }
                    }
                }
                if !progress {
                    break;
                }
            }
        } else {
            loop {
                let send = state
                    .store
                    .channels
                    .get_mut(&chan)
                    .and_then(|c| c.pending_send.pop_front());
                let Some(send) = send else { break };
                let recv = state
                    .store
                    .channels
                    .get_mut(&chan)
                    .and_then(|c| c.pending_recv.pop_front());
                match recv {
                    Some(r) => {
                        if let Some(dst) = self.recv_dst_of(state, r) {
                            if let Some(fid) = state.current_frame_id(r) {
                                let _ = state.write_dst(fid, dst, send.value);
                            }
                        }
                        self.complete(state, r);
                        self.complete(state, send.thread);
                    }
                    None => {
                        state
                            .store
                            .channels
                            .get_mut(&chan)
                            .unwrap()
                            .pending_send
                            .push_front(send);
                        break;
                    }
                }
            }
        }
    }

    fn finish_thread(&self, state: &mut MachineState, tid: ThreadId, function: FunctionId) {
        if let Some(t) = state.threads.get_mut(&tid) {
            t.status = ThreadStatus::Finished;
        }
        state.finished.insert(tid);
        *state.completed_functions.entry(function).or_insert(0) += 1;

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
                    && t.stack
                        .first()
                        .map(|fid| state.frame(*fid).function)
                        == Some(func)
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
                    handles: Default::default(),
                    handle_children: Default::default(),
                    parent_scope,
                },
            );
            state.finished.insert(tid);
            *state.completed_functions.entry(func).or_insert(0) += 1;
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
                handles: Default::default(),
                handle_children: Default::default(),
                parent_scope,
            },
        );
        Ok(Some(tid))
    }

    fn step_thread(
        &self,
        state: &MachineState,
        tid: ThreadId,
        boundary: &mut Vec<BoundaryEvent>,
    ) -> BackendResult<Option<Step<MachineState>>> {
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
        let label = StepLabel::new(TransitionOrigin {
            module: function.module,
            function: function_id,
            sid: Some(pc),
            phase: Phase::Statement,
        })
        .with_binding(tid, fid);
        let op = stmt.op.clone();

        // Blocking / synchronization choices are cloned so that reads of the
        // program do not conflict with mutations of `next`.
        match op {
            SemOp::Nop => {
                next.advance(tid);
            }
            SemOp::AssignLocal { target, expr } => {
                let v = self.eval(&next, fid, &expr, &at)?;
                if let SlotRef::Local(slot) = target {
                    if !within_type(&v, &function.slots[slot].ty) {
                        return Ok(None);
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
                    return Ok(None);
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
                    return Ok(None);
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
                    return Ok(None);
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
                    next.mutex_waiters.entry(resource).or_default().push_back(tid);
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
                self.grant_lock(&mut next, resource);
                next.advance(tid);
            }
            SemOp::ChannelSend { channel, value } => {
                let v = self.eval(&next, fid, &value, &at)?;
                {
                    let ch = next.store.channels.get_mut(&channel).unwrap();
                    ch.pending_send.push_back(PendingSend { thread: tid, value: v });
                }
                self.drain_channel(&mut next, channel);
                let still = next
                    .store
                    .channels
                    .get(&channel)
                    .map(|c| c.pending_send.iter().any(|p| p.thread == tid))
                    .unwrap_or(false);
                if still {
                    next.block(tid, BlockReason::ChannelSend(channel));
                }
            }
            SemOp::ChannelRecv { channel, dst: _ } => {
                next.store
                    .channels
                    .get_mut(&channel)
                    .unwrap()
                    .pending_recv
                    .push_back(tid);
                self.drain_channel(&mut next, channel);
                let still = next
                    .store
                    .channels
                    .get(&channel)
                    .map(|c| c.pending_recv.contains(&tid))
                    .unwrap_or(false);
                if still {
                    next.block(tid, BlockReason::ChannelRecv(channel));
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
                self.grant_lock(&mut next, lock);
                {
                    let cv = next.store.condvars.get_mut(&condvar).unwrap();
                    cv.waiters.push_back(tid);
                    cv.lock = Some(lock);
                }
                next.block(tid, BlockReason::Condvar(condvar, lock));
            }
            SemOp::CondvarNotify { condvar } => {
                let lock = next.store.condvars.get(&condvar).and_then(|c| c.lock);
                let w = next
                    .store
                    .condvars
                    .get_mut(&condvar)
                    .and_then(|c| c.waiters.pop_front());
                if let (Some(w), Some(lock)) = (w, lock) {
                    next.mutex_waiters.entry(lock).or_default().push_back(w);
                    next.block(w, BlockReason::Lock(lock));
                    self.grant_lock(&mut next, lock);
                }
                next.advance(tid);
            }
            SemOp::CondvarNotifyAll { condvar } => {
                let lock = next.store.condvars.get(&condvar).and_then(|c| c.lock);
                let waiters: Vec<ThreadId> = {
                    let cv = next.store.condvars.get_mut(&condvar).unwrap();
                    cv.waiters.drain(..).collect()
                };
                if let Some(lock) = lock {
                    for w in waiters {
                        next.mutex_waiters.entry(lock).or_default().push_back(w);
                        next.block(w, BlockReason::Lock(lock));
                    }
                    self.grant_lock(&mut next, lock);
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
                loop {
                    let front = next
                        .sem_waiters
                        .get(&resource)
                        .and_then(|q| q.front().copied());
                    let Some((w, n)) = front else { break };
                    let avail = *next.store.semaphores.get(&resource).unwrap_or(&0);
                    if avail >= n {
                        next.sem_waiters.get_mut(&resource).unwrap().pop_front();
                        next.store.semaphores.insert(resource, avail - n);
                        self.complete(&mut next, w);
                    } else {
                        break;
                    }
                }
                next.advance(tid);
            }
            SemOp::Call { func, args, dst } => {
                let callee = self.program.function(func);
                if callee.is_transparent_nobody() {
                    next.advance(tid);
                } else {
                    if next.threads[&tid].stack.len() >= self.bounds.max_frames_per_thread {
                        boundary.push(BoundaryEvent::new(
                            BoundaryKind::FrameLimit,
                            format!("frame limit {} reached while calling '{}'", self.bounds.max_frames_per_thread, callee.name),
                        ));
                        return Ok(None);
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
                    frame.ret = Some(crate::interp::state::RetAddr {
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
                    return Ok(None);
                }
                let Some(child) = self.create_thread(&mut next, func, None, boundary)? else {
                    return Ok(None);
                };
                let hid = HandleId(next.alloc.next_handle);
                next.alloc.next_handle += 1;
                let t = next.threads.get_mut(&tid).unwrap();
                t.handles.insert(handle, hid);
                t.handle_children.insert(hid, child);
                next.advance(tid);
            }
            SemOp::Scope { funcs } => {
                if funcs.is_empty() {
                    return Err(BackendError::invalid("E410", "scope funcs is empty").at(&at));
                }
                for func in &funcs {
                    if !self.within_function_bound(&next, *func) {
                        return Ok(None);
                    }
                }
                let scope = ScopeId(next.alloc.next_scope);
                next.alloc.next_scope += 1;
                let mut remaining = std::collections::BTreeSet::new();
                for func in &funcs {
                    let Some(child) = self.create_thread(&mut next, *func, Some(scope), boundary)?
                    else {
                        return Ok(None);
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
                let hid = next.threads[&tid]
                    .handles
                    .get(&handle)
                    .copied()
                    .ok_or_else(|| {
                        BackendError::invalid("E402", format!("join handle '{handle}' was never spawned")).at(&at)
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
                    *next.completed_functions.entry(function_id).or_insert(0) += 1;
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

        Ok(Some(Step {
            label,
            state: next,
        }))
    }
}

impl<'a> TransitionSystem for Interpreter<'a> {
    type State = MachineState;

    fn program(&self) -> &SemProgram {
        self.program
    }

    fn initial(&self) -> BackendResult<MachineState> {
        MachineState::initial(self.program)
    }

    fn successors(&self, state: &MachineState) -> BackendResult<Enabled<MachineState>> {
        let mut enabled = Enabled::empty();
        for tid in state.threads.keys().copied().collect::<Vec<_>>() {
            if state.threads[&tid].status != ThreadStatus::Runnable {
                continue;
            }
            if let Some(step) = self.step_thread(state, tid, &mut enabled.boundary)? {
                enabled.steps.push(step);
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
                        waiting: state.mutex_waiters.get(r).map(|q| q.len()).unwrap_or(0),
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
                        detail: "waiting on condvar to re-acquire the lock".into(),
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
        out.push_str(&format!(
            "  frame {} func=f{} pc={} locals={{{}}}\n",
            fname(*f),
            frame.function.0,
            frame.pc,
            locals.join(",")
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
    for (r, q) in &state.mutex_waiters {
        out.push_str(&format!(
            "  lockq r{}=[{}]\n",
            r.0,
            q.iter().map(|t| tname(*t)).collect::<Vec<_>>().join(",")
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
