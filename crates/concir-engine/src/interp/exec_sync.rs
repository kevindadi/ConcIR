use concir_sem::eval::eval;
use concir_sem::ids::{FrameId, FunctionId, HandleId, ResourceId, ScopeId, SlotRef, ThreadId};
use concir_sem::monitor::MonitorConfig;
use concir_sem::outcome::{
    AnalysisBounds, BackendError, BackendResult, BoundaryEvent, BoundaryKind, Phase, StepLabel,
    TransitionOrigin,
};
use concir_sem::program::{SemOp, SemProgram};
use concir_sem::system::{
    compare_values, BlockKind, BlockedRecord, Enabled, Predicate, Step, TransitionSystem,
};
use concir_sem::value::{within_type, Value};

use super::state::{
    BlockReason, FrameView, MachineState, MutexState, PendingSend, RetAddr, RwLockState,
    ScopeState, ThreadState, ThreadStatus,
};


use super::Interpreter;

impl<'a> Interpreter<'a> {
    pub(super) fn deliver_to_recv(
        &self,
        state: &mut MachineState,
        recv: ThreadId,
        value: Value,
    ) -> BackendResult<bool> {
        if let Some(dst) = self.recv_dst_of(state, recv) {
            if let Some(fid) = state.current_frame_id(recv) {
                return self.try_write_dst(state, fid, dst, value);
            }
        }
        Ok(true)
    }

    /// Whether `tid` holds resource `r` (mutex owned by `tid`; a semaphore is
    /// "held" while permits are below its initial count, since permit ownership
    /// is not tracked per thread).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn step_sync_op(
        &self,
        next: &mut MachineState,
        tid: ThreadId,
        fid: FrameId,
        _func: FunctionId,
        _pc: usize,
        op: SemOp,
        at: &str,
        label: StepLabel,
    ) -> BackendResult<Vec<Step<MachineState>>> {
        let mut next = next.clone();
        match op {
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
                        .at(at))
                    }
                    _ => {
                        return Err(BackendError::invalid(
                            "E510",
                            format!("thread {tid} unlocked free Mutex {resource}"),
                        )
                        .at(at))
                    }
                }
                next.store.mutexes.insert(resource, MutexState::Free);
                next.advance(tid);
            }
            SemOp::RwLockRead { resource } => match next.store.rwlocks.get(&resource).cloned() {
                Some(RwLockState::Free) => {
                    let mut readers = std::collections::BTreeSet::new();
                    readers.insert(tid);
                    next.store
                        .rwlocks
                        .insert(resource, RwLockState::Reading(readers));
                    next.advance(tid);
                }
                Some(RwLockState::Reading(mut readers)) => {
                    readers.insert(tid);
                    next.store
                        .rwlocks
                        .insert(resource, RwLockState::Reading(readers));
                    next.advance(tid);
                }
                _ => {
                    next.block(tid, BlockReason::RwLockRead(resource));
                }
            },
            SemOp::RwLockWrite { resource } => {
                if matches!(next.store.rwlocks.get(&resource), Some(RwLockState::Free)) {
                    next.store
                        .rwlocks
                        .insert(resource, RwLockState::Writing(tid));
                    next.advance(tid);
                } else {
                    next.block(tid, BlockReason::RwLockWrite(resource));
                }
            }
            SemOp::RwLockUnlock { resource } => match next.store.rwlocks.get(&resource).cloned() {
                Some(RwLockState::Writing(owner)) if owner == tid => {
                    next.store.rwlocks.insert(resource, RwLockState::Free);
                    next.advance(tid);
                }
                Some(RwLockState::Reading(mut readers)) if readers.contains(&tid) => {
                    readers.remove(&tid);
                    if readers.is_empty() {
                        next.store.rwlocks.insert(resource, RwLockState::Free);
                    } else {
                        next.store
                            .rwlocks
                            .insert(resource, RwLockState::Reading(readers));
                    }
                    next.advance(tid);
                }
                _ => {
                    return Err(BackendError::invalid(
                        "E510",
                        format!("thread {tid} unlocked RwLock {resource} it does not hold"),
                    )
                    .at(at))
                }
            },
            SemOp::ChannelSend { channel, value } => {
                let v = self.eval(&next, fid, &value, at)?;
                let base_ty = self.program.resource(channel).ty.clone();
                if let Some(ty) = &base_ty {
                    if !within_type(&v, ty) {
                        return Ok(Vec::new());
                    }
                }
                let cap = self.program.resource(channel).capacity;
                if cap == 0 {
                    // Rendezvous: every waiting receiver is a legal match
                    // (message order does not impose waiting-thread FIFO).
                    let receivers: Vec<ThreadId> = next
                        .store
                        .channels
                        .get(&channel)
                        .map(|c| c.pending_recv.iter().copied().collect())
                        .unwrap_or_default();
                    if receivers.is_empty() {
                        next.store
                            .channels
                            .get_mut(&channel)
                            .unwrap()
                            .pending_send
                            .push_back(PendingSend {
                                thread: tid,
                                value: v,
                            });
                        next.block(tid, BlockReason::ChannelSend(channel));
                    } else {
                        let mut out = Vec::new();
                        for r in receivers {
                            let mut s = next.clone();
                            if !self.deliver_to_recv(&mut s, r, v.clone())? {
                                continue;
                            }
                            s.store
                                .channels
                                .get_mut(&channel)
                                .unwrap()
                                .pending_recv
                                .retain(|t| *t != r);
                            self.complete(&mut s, r);
                            s.advance(tid);
                            out.push(Step {
                                label: label.clone(),
                                state: s,
                            });
                        }
                        return Ok(out);
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
                            .push_back(PendingSend {
                                thread: tid,
                                value: v,
                            });
                        next.block(tid, BlockReason::ChannelSend(channel));
                    }
                }
            }
            SemOp::ChannelRecv { channel, .. } => {
                let cap = self.program.resource(channel).capacity;
                if cap == 0 {
                    // Rendezvous: every waiting sender is a legal match.
                    let senders: Vec<(ThreadId, Value)> = next
                        .store
                        .channels
                        .get(&channel)
                        .map(|c| {
                            c.pending_send
                                .iter()
                                .map(|p| (p.thread, p.value.clone()))
                                .collect()
                        })
                        .unwrap_or_default();
                    if senders.is_empty() {
                        next.store
                            .channels
                            .get_mut(&channel)
                            .unwrap()
                            .pending_recv
                            .push_back(tid);
                        next.block(tid, BlockReason::ChannelRecv(channel));
                    } else {
                        let dst = self.recv_dst_of(&next, tid).unwrap_or(SlotRef::Discard);
                        let mut out = Vec::new();
                        for (s_tid, s_val) in senders {
                            let mut s = next.clone();
                            if !self.try_write_dst(&mut s, fid, dst, s_val)? {
                                continue;
                            }
                            s.store
                                .channels
                                .get_mut(&channel)
                                .unwrap()
                                .pending_send
                                .retain(|p| p.thread != s_tid);
                            self.complete(&mut s, s_tid);
                            s.advance(tid);
                            out.push(Step {
                                label: label.clone(),
                                state: s,
                            });
                        }
                        return Ok(out);
                    }
                } else {
                    let head = next
                        .store
                        .channels
                        .get(&channel)
                        .and_then(|c| c.buffer.front().cloned());
                    match head {
                        Some(v) => {
                            let dst = self.recv_dst_of(&next, tid).unwrap_or(SlotRef::Discard);
                            if !self.try_write_dst(&mut next, fid, dst, v)? {
                                return Ok(Vec::new());
                            }
                            next.store
                                .channels
                                .get_mut(&channel)
                                .unwrap()
                                .buffer
                                .pop_front();
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
                    _ => return Err(BackendError::invalid(
                        "E511",
                        format!(
                            "thread {tid} waited on Condvar {condvar} without holding Mutex {lock}"
                        ),
                    )
                    .at(at)),
                }
                next.store.mutexes.insert(lock, MutexState::Free);
                {
                    let cv = next.store.condvars.get_mut(&condvar).unwrap();
                    cv.waiters.push_back((tid, lock));
                }
                next.block(tid, BlockReason::Condvar(condvar, lock));
            }
            SemOp::CondvarNotify { .. } => unreachable!("handled above"),
            SemOp::CondvarNotifyAll { condvar } => {
                // Each waiter re-acquires *its own* lock.
                let waiters: Vec<(ThreadId, ResourceId)> = {
                    let cv = next.store.condvars.get_mut(&condvar).unwrap();
                    cv.waiters.drain(..).collect()
                };
                for (w, lock) in waiters {
                    next.block(w, BlockReason::Lock(lock));
                }
                next.advance(tid);
            }
            SemOp::SemaphoreAcquire { resource, count } => {
                if count <= 0 {
                    return Err(
                        BackendError::invalid("E904", "semaphore count must be positive").at(at),
                    );
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
                    return Err(
                        BackendError::invalid("E904", "semaphore count must be positive").at(at),
                    );
                }
                let available = *next.store.semaphores.get(&resource).unwrap_or(&0);
                let updated = available.checked_add(count).ok_or_else(|| {
                    BackendError::invalid(
                        "E905",
                        format!("semaphore '{resource}' permit count overflows i64"),
                    )
                    .at(at)
                })?;
                next.store.semaphores.insert(resource, updated);
                next.advance(tid);
            }
            _ => return Ok(Vec::new()),
        }
        Ok(vec![Step { label, state: next }])
    }
    pub(super) fn step_notify(
        &self,
        state: &MachineState,
        tid: ThreadId,
        function_id: FunctionId,
        pc: usize,
        condvar: ResourceId,
    ) -> BackendResult<Vec<Step<MachineState>>> {
        let fid = state.current_frame_id(tid).unwrap();
        let label = self.step_label(function_id, pc, tid, fid);
        let waiters: Vec<(ThreadId, ResourceId)> = state
            .store
            .condvars
            .get(&condvar)
            .map(|c| c.waiters.iter().cloned().collect())
            .unwrap_or_default();

        if waiters.is_empty() {
            let mut next = state.clone();
            next.reached.insert((function_id, pc));
            next.advance(tid);
            return Ok(vec![Step { label, state: next }]);
        }
        let mut out = Vec::new();
        for (w, lock) in waiters {
            let mut next = state.clone();
            next.reached.insert((function_id, pc));
            if let Some(cv) = next.store.condvars.get_mut(&condvar) {
                cv.waiters.retain(|x| *x != (w, lock));
            }
            next.block(w, BlockReason::Lock(lock));
            next.advance(tid);
            out.push(Step {
                label: label.clone(),
                state: next,
            });
        }
        Ok(out)
    }

    /// Resume a thread blocked on a resource whose condition now holds.
    pub(super) fn resume_thread(
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
            BlockReason::RwLockRead(r) => match next.store.rwlocks.get(r).cloned() {
                Some(RwLockState::Free) => {
                    let mut readers = std::collections::BTreeSet::new();
                    readers.insert(tid);
                    next.store.rwlocks.insert(*r, RwLockState::Reading(readers));
                    next.set_runnable(tid);
                    next.advance(tid);
                }
                Some(RwLockState::Reading(mut readers)) => {
                    readers.insert(tid);
                    next.store.rwlocks.insert(*r, RwLockState::Reading(readers));
                    next.set_runnable(tid);
                    next.advance(tid);
                }
                _ => return Ok(Vec::new()),
            },
            BlockReason::RwLockWrite(r) => {
                if !matches!(next.store.rwlocks.get(r), Some(RwLockState::Free)) {
                    return Ok(Vec::new());
                }
                next.store.rwlocks.insert(*r, RwLockState::Writing(tid));
                next.set_runnable(tid);
                next.advance(tid);
            }
            BlockReason::Semaphore(r) => {
                let need = next
                    .sem_waiters
                    .get(r)
                    .and_then(|q| q.iter().find(|(t, _)| *t == tid).map(|(_, n)| *n));
                let Some(need) = need else {
                    return Ok(Vec::new());
                };
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
                    // Cap-0 never has both sides waiting; pair defensively.
                    let recv = next
                        .store
                        .channels
                        .get(c)
                        .and_then(|ch| ch.pending_recv.front().copied());
                    let Some(recv) = recv else {
                        return Ok(Vec::new());
                    };
                    let pos = next
                        .store
                        .channels
                        .get(c)
                        .and_then(|ch| ch.pending_send.iter().position(|p| p.thread == tid));
                    let Some(pos) = pos else {
                        return Ok(Vec::new());
                    };
                    let value = next
                        .store
                        .channels
                        .get(c)
                        .unwrap()
                        .pending_send
                        .get(pos)
                        .unwrap()
                        .value
                        .clone();
                    if !self.deliver_to_recv(&mut next, recv, value)? {
                        return Ok(Vec::new());
                    }
                    next.store
                        .channels
                        .get_mut(c)
                        .unwrap()
                        .pending_send
                        .remove(pos);
                    next.store
                        .channels
                        .get_mut(c)
                        .unwrap()
                        .pending_recv
                        .retain(|t| *t != recv);
                    self.complete(&mut next, recv);
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
                    let Some(pos) = pos else {
                        return Ok(Vec::new());
                    };
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
                }
                next.set_runnable(tid);
                next.advance(tid);
            }
            BlockReason::ChannelRecv(c) => {
                let cap = self.program.resource(*c).capacity;
                if cap == 0 {
                    let send = next
                        .store
                        .channels
                        .get(c)
                        .and_then(|ch| ch.pending_send.front().cloned());
                    let Some(send) = send else {
                        return Ok(Vec::new());
                    };
                    let dst = self.recv_dst_of(&next, tid).unwrap_or(SlotRef::Discard);
                    if !self.try_write_dst(&mut next, fid, dst, send.value.clone())? {
                        return Ok(Vec::new());
                    }
                    next.store
                        .channels
                        .get_mut(c)
                        .unwrap()
                        .pending_send
                        .retain(|p| p.thread != send.thread);
                    self.complete(&mut next, send.thread);
                } else {
                    let head = next
                        .store
                        .channels
                        .get(c)
                        .and_then(|ch| ch.buffer.front().cloned());
                    let Some(v) = head else { return Ok(Vec::new()) };
                    let dst = self.recv_dst_of(&next, tid).unwrap_or(SlotRef::Discard);
                    if !self.try_write_dst(&mut next, fid, dst, v)? {
                        return Ok(Vec::new());
                    }
                    next.store.channels.get_mut(c).unwrap().buffer.pop_front();
                    if let Some(ch) = next.store.channels.get_mut(c) {
                        ch.pending_recv.retain(|t| *t != tid);
                    }
                }
                next.set_runnable(tid);
                next.advance(tid);
            }
            // Condvar waiters are resumed by `notify`; join/scope by completion.
            BlockReason::Condvar(_, _) | BlockReason::Join(_) | BlockReason::Scope(_) => {
                return Ok(Vec::new());
            }
        }

        Ok(vec![Step { label, state: next }])
    }
}
