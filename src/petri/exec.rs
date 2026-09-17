//! Deterministic Petri-net executor.
//!
//! This engine is written against [`NetState`] only. It does not call the
//! reference interpreter; the two independently implement synchronization
//! transitions and share only the resolved program, values, expression
//! evaluation, and bounds.

use std::collections::BTreeSet;

use crate::sem::eval::eval;
use crate::sem::ids::{FrameId, FunctionId, HandleId, ResourceId, ScopeId, SlotRef, ThreadId};
use crate::sem::monitor::MonitorConfig;
use crate::sem::outcome::{
    AnalysisBounds, BackendError, BackendResult, BoundaryEvent, BoundaryKind, StepLabel,
};
use crate::sem::program::{SemOp, SemProgram};
use crate::sem::system::{
    compare_values, BlockKind, BlockedRecord, Enabled, InstanceState, Predicate, Step,
    TransitionSystem,
};
use crate::sem::value::{within_type, Value};

use super::net::{
    build, default_locals, Binding, MutexToken, NetAlloc, NetFrame, NetFrameView, NetOp, NetRetAddr,
    NetScope, NetState, NetStore, NetThread, NetToken, PetriNet, PlaceId, PlaceKey, Transition,
};

pub struct PetriEngine<'a> {
    pub program: &'a SemProgram,
    pub net: PetriNet,
    pub bounds: AnalysisBounds,
    pub monitor: MonitorConfig,
}

enum FireBind {
    Control {
        thread: ThreadId,
        frame: FrameId,
    },
    ControlWait {
        thread: ThreadId,
        frame: FrameId,
        wait: NetToken,
    },
    ControlChooseWait {
        thread: ThreadId,
        frame: FrameId,
        wait: NetToken,
    },
    Wait(NetToken),
    Pair(NetToken, NetToken),
}

impl<'a> PetriEngine<'a> {
    pub fn new(program: &'a SemProgram, bounds: AnalysisBounds) -> Self {
        Self::with_monitor(program, bounds, MonitorConfig::unbounded())
    }

    pub fn with_monitor(
        program: &'a SemProgram,
        bounds: AnalysisBounds,
        monitor: MonitorConfig,
    ) -> Self {
        let net = build(program);
        PetriEngine {
            program,
            net,
            bounds,
            monitor,
        }
    }

    fn record_completion(&self, state: &mut NetState, function: FunctionId) {
        let max = self.monitor.max_for(function);
        if max == 0 {
            return;
        }
        let e = state.store.completed_functions.entry(function).or_insert(0);
        if *e < max {
            *e += 1;
        }
    }

    // ── place lookups ──────────────────────────────────────────────
    fn ctrl(&self, function: FunctionId, sid: usize) -> Option<PlaceId> {
        self.net
            .place_of
            .get(&PlaceKey::Control { function, sid })
            .copied()
    }

    fn place(&self, key: &PlaceKey) -> Option<PlaceId> {
        self.net.place_of.get(key).copied()
    }

    fn eval(
        &self,
        state: &NetState,
        frame: FrameId,
        expr: &crate::sem::eval::LExpr,
        at: &str,
    ) -> BackendResult<Value> {
        let view = NetFrameView {
            frame: state.store.frame(frame),
            state,
            net: &self.net,
        };
        eval(expr, &view, at)
    }

    fn put_control_next(&self, state: &mut NetState, token: &NetToken) -> BackendResult<()> {
        let thread = token.thread().unwrap();
        let frame = token.frame().unwrap();
        let f = state.store.frame(frame);
        let function = f.function;
        let pc = f.pc;
        let pid = self.ctrl(function, pc + 1).ok_or_else(|| {
            BackendError::invalid("E602", format!("no control place after pc {pc}"))
        })?;
        self.place_control(state, pid, thread, frame);
        if let Some(t) = state.store.threads.get_mut(&thread) {
            t.blocked_at = None;
        }
        Ok(())
    }

    fn set_blocked(&self, state: &mut NetState, thread: ThreadId, key: PlaceKey) {
        if let Some(t) = state.store.threads.get_mut(&thread) {
            t.blocked_at = Some(key);
        }
    }

    /// Place a control token, keeping `frame.pc` in sync with the control place.
    fn place_control(&self, state: &mut NetState, pid: PlaceId, thread: ThreadId, frame: FrameId) {
        if let Some(PlaceKey::Control { sid, .. }) = self.net.places.get(pid as usize).map(|p| &p.key)
        {
            if let Some(f) = state.store.frames.get_mut(&frame) {
                f.pc = *sid;
            }
        }
        state.put(pid, NetToken::control(thread, frame));
    }

    fn wake_control_next(&self, state: &mut NetState, token: &NetToken) {
        if let (Some(thread), Some(frame)) = (token.thread(), token.frame()) {
            let f = state.store.frame(frame);
            let (function, pc) = (f.function, f.pc);
            if let Some(dest) = self.ctrl(function, pc + 1) {
                self.place_control(state, dest, thread, frame);
            }
            if let Some(t) = state.store.threads.get_mut(&thread) {
                t.blocked_at = None;
            }
        }
    }

    fn create_thread(
        &self,
        state: &mut NetState,
        func: FunctionId,
        parent_scope: Option<ScopeId>,
        boundary: &mut Vec<BoundaryEvent>,
    ) -> BackendResult<Option<ThreadId>> {
        let live = state
            .store
            .threads
            .keys()
            .filter(|t| !state.store.finished.contains(t))
            .count();
        if live >= self.bounds.max_threads {
            boundary.push(BoundaryEvent::new(
                BoundaryKind::ThreadLimit,
                format!("thread limit {} reached", self.bounds.max_threads),
            ));
            return Ok(None);
        }
        let tid = ThreadId(state.store.alloc.next_thread);
        state.store.alloc.next_thread += 1;
        let f = self.program.function(func);
        let thread = NetThread {
            entry_function: func,
            stack: Vec::new(),
            handle_children: Default::default(),
            parent_scope,
            blocked_at: None,
        };
        state.store.threads.insert(tid, thread);
        if f.is_transparent_nobody() {
            state.store.finished.insert(tid);
            self.record_completion(state, func);
            return Ok(Some(tid));
        }
        let frame = NetFrame {
            id: FrameId(state.store.alloc.next_frame),
            function: func,
            pc: 0,
            locals: default_locals(self.program, func),
            handles: Default::default(),
            ret: None,
        };
        state.store.alloc.next_frame += 1;
        let fid = frame.id;
        state.store.frames.insert(fid, frame);
        state.store.threads.get_mut(&tid).unwrap().stack.push(fid);
        let start = self.ctrl(func, 0).ok_or_else(|| {
            BackendError::invalid("E602", format!("no entry control place for f{}", func.0))
        })?;
        self.place_control(state, start, tid, fid);
        Ok(Some(tid))
    }

    fn within_bound(&self, state: &NetState, func: FunctionId) -> bool {
        let Some(bound) = self.program.function(func).bound else {
            return true;
        };
        let active = state
            .store
            .threads
            .iter()
            .filter(|(id, t)| !state.store.finished.contains(id) && t.entry_function == func)
            .count();
        active < bound.max(0) as usize
    }

    fn finish_thread(&self, state: &mut NetState, thread: ThreadId, function: FunctionId) {
        state.store.finished.insert(thread);
        self.record_completion(state, function);

        let scope = state.store.threads.get(&thread).and_then(|t| t.parent_scope);
        if let Some(scope) = scope {
            let empty = match state.store.scopes.get_mut(&scope) {
                Some(sc) => {
                    sc.remaining.remove(&thread);
                    sc.remaining.is_empty()
                }
                None => false,
            };
            if empty {
                let (owner, owner_frame, owner_sid, owner_function) = {
                    let sc = &state.store.scopes[&scope];
                    let f = state.store.frame(sc.owner_frame);
                    (sc.owner, sc.owner_frame, sc.owner_sid, f.function)
                };
                state
                    .store
                    .completed_scopes
                    .insert((owner_function, owner_sid));
                let key = PlaceKey::ScopeWait {
                    function: owner_function,
                    sid: owner_sid,
                };
                if let Some(pid) = self.place(&key) {
                    if state.take_token(pid, &NetToken::ScopeWait { thread: owner, frame: owner_frame })
                    {
                        if let Some(dest) = self.ctrl(owner_function, owner_sid + 1) {
                            self.place_control(state, dest, owner, owner_frame);
                        }
                        if let Some(t) = state.store.threads.get_mut(&owner) {
                            t.blocked_at = None;
                        }
                    }
                }
                // Reclaim scope members and the scope record.
                let members: Vec<ThreadId> = state
                    .store
                    .threads
                    .iter()
                    .filter(|(_, t)| t.parent_scope == Some(scope))
                    .map(|(id, _)| *id)
                    .collect();
                for m in members {
                    state.store.threads.remove(&m);
                    state.store.finished.remove(&m);
                }
                state.store.scopes.remove(&scope);
            }
        }

        // Wake joiners whose child is this thread.
        let join_keys: Vec<PlaceKey> = self
            .net
            .places
            .iter()
            .filter(|p| matches!(p.key, PlaceKey::JoinWait { .. }))
            .map(|p| p.key.clone())
            .collect();
        for key in join_keys {
            let sid = match &key {
                PlaceKey::JoinWait { sid, .. } => *sid,
                _ => continue,
            };
            let function = match &key {
                PlaceKey::JoinWait { function, .. } => *function,
                _ => continue,
            };
            let Some(pid) = self.place(&key) else { continue };
            let tokens: Vec<NetToken> = state.place_tokens(pid).to_vec();
            for token in tokens {
                let (jt, jf) = match &token {
                    NetToken::JoinWait { thread, frame } => (*thread, *frame),
                    _ => continue,
                };
                let stmt = &self.program.function(function).body[sid];
                let handle = match &stmt.op {
                    SemOp::Join { handle } => handle,
                    _ => continue,
                };
                let child = state
                    .store
                    .frames
                    .get(&jf)
                    .and_then(|fr| fr.handles.get(handle))
                    .and_then(|h| state.store.threads.get(&jt).and_then(|t| t.handle_children.get(h)))
                    .copied();
                if child == Some(thread) {
                    state.take_token(pid, &token);
                    if let Some(dest) = self.ctrl(function, sid + 1) {
                        self.place_control(state, dest, jt, jf);
                    }
                    if let Some(t) = state.store.threads.get_mut(&jt) {
                        t.blocked_at = None;
                    }
                    // Consume the handle binding and reclaim the finished child.
                    let hid = state
                        .store
                        .frames
                        .get(&jf)
                        .and_then(|fr| fr.handles.get(handle))
                        .copied();
                    if let Some(hid) = hid {
                        if let Some(t) = state.store.threads.get_mut(&jt) {
                            t.handle_children.remove(&hid);
                        }
                        if let Some(fr) = state.store.frames.get_mut(&jf) {
                            fr.handles.remove(handle);
                        }
                    }
                    state.store.threads.remove(&thread);
                    state.store.finished.remove(&thread);
                }
            }
        }
    }

    fn fire(
        &self,
        state: &NetState,
        t: &Transition,
        bind: &FireBind,
        boundary: &mut Vec<BoundaryEvent>,
    ) -> BackendResult<Option<NetState>> {
        let mut next = state.clone();
        let control = match bind {
            FireBind::Control { thread, frame }
            | FireBind::ControlWait { thread, frame, .. }
            | FireBind::ControlChooseWait { thread, frame, .. } => Some((*thread, *frame)),
            _ => None,
        };
        // Remove the input control token for control-bound transitions.
        if let Some((thread, frame)) = control {
            let pid = match bind {
                FireBind::Control { .. } => match t.binding {
                    Binding::Control(pid) => Some(pid),
                    _ => None,
                },
                FireBind::ControlWait { .. } => match t.binding {
                    Binding::ControlWait(pid, _) => Some(pid),
                    _ => None,
                },
                FireBind::ControlChooseWait { .. } => match t.binding {
                    Binding::ControlChooseWait(pid, _) => Some(pid),
                    _ => None,
                },
                _ => None,
            };
            if let Some(pid) = pid {
                next.take_token(pid, &NetToken::control(thread, frame));
            }
            if let Some(f) = next.store.frames.get(&frame) {
                next.store.reached.insert((f.function, f.pc));
            }
        }

        let at = match control {
            Some((_, frame)) => {
                let f = next.store.frame(frame);
                self.program.location(f.function, Some(f.pc))
            }
            None => "<net>".to_string(),
        };

        macro_rules! disabled {
            () => {
                return Ok(None)
            };
        }

        match &t.op {
            NetOp::Nop => {
                let out = t.next.expect("nop has next");
                self.place_control(&mut next, out, control.unwrap().0, control.unwrap().1);
            }
            NetOp::AssignLocal { target, expr } => {
                let (_, frame) = control.unwrap();
                let v = self.eval(&next, frame, expr, &at)?;
                if let SlotRef::Local(slot) = target {
                    let f = self.program.function(next.store.frame(frame).function);
                    if !within_type(&v, &f.slots[*slot].ty) {
                        disabled!();
                    }
                }
                next.store.frame_mut(frame).locals.insert(match target {
                    SlotRef::Local(s) => *s,
                    _ => 0,
                }, v);
                let out = t.next.unwrap();
                self.place_control(&mut next, out, control.unwrap().0, control.unwrap().1);
            }
            NetOp::ReadVar { resource, dst } => {
                let (_, frame) = control.unwrap();
                let pid = self.place(&PlaceKey::Var(*resource)).unwrap();
                let v = next.read_data(pid).cloned().ok_or_else(|| {
                    BackendError::invalid("E900", format!("Var {resource} has no value"))
                })?;
                if let Some(dst) = dst {
                    if !self.write_dst(&mut next, frame, *dst, v)? { disabled!(); }
                }
                let out = t.next.unwrap();
                self.place_control(&mut next, out, control.unwrap().0, control.unwrap().1);
            }
            NetOp::WriteVar { resource, expr } => {
                let (_, frame) = control.unwrap();
                let v = self.eval(&next, frame, expr, &at)?;
                let ty = self.program.resource(*resource).ty.clone().unwrap();
                if !within_type(&v, &ty) {
                    disabled!();
                }
                let pid = self.place(&PlaceKey::Var(*resource)).unwrap();
                next.set_data(pid, v);
                let out = t.next.unwrap();
                self.place_control(&mut next, out, control.unwrap().0, control.unwrap().1);
            }
            NetOp::AtomicLoad { resource, dst } => {
                let (_, frame) = control.unwrap();
                let pid = self.place(&PlaceKey::Atomic(*resource)).unwrap();
                let v = next.read_data(pid).cloned().ok_or_else(|| {
                    BackendError::invalid("E900", format!("Atomic {resource} has no value"))
                })?;
                if !self.write_dst(&mut next, frame, *dst, v)? { disabled!(); }
                let out = t.next.unwrap();
                self.place_control(&mut next, out, control.unwrap().0, control.unwrap().1);
            }
            NetOp::AtomicStore { resource, expr } => {
                let (_, frame) = control.unwrap();
                let v = self.eval(&next, frame, expr, &at)?;
                let ty = self.program.resource(*resource).ty.clone().unwrap();
                if !within_type(&v, &ty) {
                    disabled!();
                }
                let pid = self.place(&PlaceKey::Atomic(*resource)).unwrap();
                next.set_data(pid, v);
                let out = t.next.unwrap();
                self.place_control(&mut next, out, control.unwrap().0, control.unwrap().1);
            }
            NetOp::AtomicCas {
                resource,
                expected,
                desired,
                dst,
            } => {
                let (_, frame) = control.unwrap();
                let exp = self.eval(&next, frame, expected, &at)?;
                let des = self.eval(&next, frame, desired, &at)?;
                let ty = self.program.resource(*resource).ty.clone().unwrap();
                if !within_type(&des, &ty) {
                    disabled!();
                }
                let pid = self.place(&PlaceKey::Atomic(*resource)).unwrap();
                let old = next.read_data(pid).cloned().ok_or_else(|| {
                    BackendError::invalid("E900", format!("Atomic {resource} has no value"))
                })?;
                if !self.write_dst(&mut next, frame, *dst, old.clone())? { disabled!(); }
                if old == exp {
                    next.set_data(pid, des);
                }
                let out = t.next.unwrap();
                self.place_control(&mut next, out, control.unwrap().0, control.unwrap().1);
            }
            NetOp::MutexLockGrant { resource } => {
                let (thread, frame) = control.unwrap();
                let mtx = self.place(&PlaceKey::Mutex(*resource)).unwrap();
                if !matches!(next.read_mutex(mtx), Some(MutexToken::Free)) {
                    disabled!();
                }
                next.set_mutex(mtx, MutexToken::Held(thread));
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
            }
            NetOp::MutexLockBlock { resource } => {
                let (thread, frame) = control.unwrap();
                let mtx = self.place(&PlaceKey::Mutex(*resource)).unwrap();
                if !matches!(next.read_mutex(mtx), Some(MutexToken::Held(_))) {
                    disabled!();
                }
                let out = t.next.unwrap();
                next.put(out, NetToken::LockWait { thread, frame });
                self.set_blocked(&mut next, thread, PlaceKey::LockWait(*resource));
            }
            NetOp::MutexUnlock { resource } => {
                let (thread, frame) = control.unwrap();
                let mtx = self.place(&PlaceKey::Mutex(*resource)).unwrap();
                match next.read_mutex(mtx) {
                    Some(MutexToken::Held(owner)) if owner == thread => {
                        next.set_mutex(mtx, MutexToken::Free);
                    }
                    Some(MutexToken::Held(_)) => {
                        return Err(BackendError::invalid(
                            "E510",
                            format!("thread {thread} unlocked Mutex {resource} it does not hold"),
                        )
                        .at(&at))
                    }
                    _ => {
                        return Err(BackendError::invalid(
                            "E510",
                            format!("thread {thread} unlocked free Mutex {resource}"),
                        )
                        .at(&at))
                    }
                }
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
            }
            NetOp::LockWaitAcquire { resource } => {
                let token = match bind {
                    FireBind::Wait(token) => token.clone(),
                    _ => disabled!(),
                };
                let (thread, frame) = (
                    token.thread().unwrap(),
                    token.frame().unwrap(),
                );
                let mtx = self.place(&PlaceKey::Mutex(*resource)).unwrap();
                if !matches!(next.read_mutex(mtx), Some(MutexToken::Free)) {
                    disabled!();
                }
                let lw = self.place(&PlaceKey::LockWait(*resource)).unwrap();
                if !next.take_token(lw, &token) {
                    disabled!();
                }
                next.set_mutex(mtx, MutexToken::Held(thread));
                self.put_control_next(&mut next, &token)?;
                let _ = frame;
            }
            NetOp::CondvarWait { condvar, lock } => {
                let (thread, frame) = control.unwrap();
                let mtx = self.place(&PlaceKey::Mutex(*lock)).unwrap();
                match next.read_mutex(mtx) {
                    Some(MutexToken::Held(owner)) if owner == thread => {}
                    _ => {
                        return Err(BackendError::invalid(
                            "E511",
                            format!(
                                "thread {thread} waited on Condvar {condvar} without holding Mutex {lock}"
                            ),
                        )
                        .at(&at))
                    }
                }
                next.set_mutex(mtx, MutexToken::Free);
                let cv = self.place(&PlaceKey::Condvar(*condvar)).unwrap();
                next.put(
                    cv,
                    NetToken::CondvarWait {
                        thread,
                        frame,
                        lock: *lock,
                    },
                );
                self.set_blocked(&mut next, thread, PlaceKey::Condvar(*condvar));
            }
            NetOp::CondvarNotifyHit { condvar } => {
                let (thread, frame) = control.unwrap();
                let wait = match bind {
                    FireBind::ControlChooseWait { wait, .. } => wait.clone(),
                    _ => disabled!(),
                };
                let (wt, wf, lock) = match &wait {
                    NetToken::CondvarWait { thread, frame, lock } => (*thread, *frame, *lock),
                    _ => disabled!(),
                };
                let cv = self.place(&PlaceKey::Condvar(*condvar)).unwrap();
                if !next.take_token(cv, &wait) {
                    disabled!();
                }
                let lw = self.place(&PlaceKey::LockWait(lock)).unwrap();
                next.put(lw, NetToken::LockWait { thread: wt, frame: wf });
                self.set_blocked(&mut next, wt, PlaceKey::LockWait(lock));
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
            }
            NetOp::CondvarNotifyMiss { condvar } => {
                let (thread, frame) = control.unwrap();
                let cv = self.place(&PlaceKey::Condvar(*condvar)).unwrap();
                if !next.place_tokens(cv).iter().all(|t| !matches!(t, NetToken::CondvarWait { .. })) {
                    disabled!();
                }
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
            }
            NetOp::CondvarNotifyAll { condvar } => {
                let (thread, frame) = control.unwrap();
                let cv = self.place(&PlaceKey::Condvar(*condvar)).unwrap();
                let waiters: Vec<NetToken> = next
                    .place_tokens(cv)
                    .iter()
                    .filter(|t| matches!(t, NetToken::CondvarWait { .. }))
                    .cloned()
                    .collect();
                for token in waiters {
                    if let NetToken::CondvarWait { thread: wt, frame: wf, lock } = &token {
                        next.take_token(cv, &token);
                        let lw = self.place(&PlaceKey::LockWait(*lock)).unwrap();
                        next.put(lw, NetToken::LockWait { thread: *wt, frame: *wf });
                        self.set_blocked(&mut next, *wt, PlaceKey::LockWait(*lock));
                    }
                }
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
            }
            NetOp::SemAcquire { resource, count } => {
                if *count <= 0 {
                    return Err(BackendError::invalid("E904", "semaphore count must be positive").at(&at));
                }
                let (thread, frame) = control.unwrap();
                let sem = self.place(&PlaceKey::Semaphore(*resource)).unwrap();
                let available = next.read_data(sem).and_then(Value::as_int).ok_or_else(|| {
                    BackendError::invalid("E900", format!("Semaphore {resource} has no count"))
                })?;
                if available < *count {
                    disabled!();
                }
                next.set_data(sem, Value::Int(available - *count));
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
            }
            NetOp::SemAcquireBlock { resource, count } => {
                let (thread, frame) = control.unwrap();
                let sem = self.place(&PlaceKey::Semaphore(*resource)).unwrap();
                let available = next.read_data(sem).and_then(Value::as_int).unwrap_or(0);
                if available >= *count {
                    disabled!();
                }
                let sw = self.place(&PlaceKey::SemWait(*resource)).unwrap();
                next.put(
                    sw,
                    NetToken::SemWait {
                        thread,
                        frame,
                        count: *count,
                    },
                );
                self.set_blocked(&mut next, thread, PlaceKey::SemWait(*resource));
            }
            NetOp::SemRelease { resource, count } => {
                if *count <= 0 {
                    return Err(BackendError::invalid("E904", "semaphore count must be positive").at(&at));
                }
                let (thread, frame) = control.unwrap();
                let sem = self.place(&PlaceKey::Semaphore(*resource)).unwrap();
                let available = next.read_data(sem).and_then(Value::as_int).unwrap_or(0);
                let updated = available.checked_add(*count).ok_or_else(|| {
                    BackendError::invalid(
                        "E905",
                        format!("semaphore '{resource}' permit count overflows i64"),
                    )
                    .at(&at)
                })?;
                next.set_data(sem, Value::Int(updated));
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
            }
            NetOp::SemGrant { resource } => {
                let token = match bind {
                    FireBind::Wait(token) => token.clone(),
                    _ => disabled!(),
                };
                let (thread, frame, need) = match &token {
                    NetToken::SemWait { thread, frame, count } => (*thread, *frame, *count),
                    _ => disabled!(),
                };
                let sem = self.place(&PlaceKey::Semaphore(*resource)).unwrap();
                let available = next.read_data(sem).and_then(Value::as_int).unwrap_or(0);
                if available < need {
                    disabled!();
                }
                let sw = self.place(&PlaceKey::SemWait(*resource)).unwrap();
                if !next.take_token(sw, &token) {
                    disabled!();
                }
                next.set_data(sem, Value::Int(available - need));
                self.put_control_next(&mut next, &token)?;
                let _ = (thread, frame);
            }
            NetOp::SendRegister { channel } => {
                let (thread, frame) = control.unwrap();
                let recv = self.place(&PlaceKey::ChannelRecv(*channel)).unwrap();
                if !next.place_tokens(recv).is_empty() {
                    disabled!();
                }
                let stmt = current_stmt(self.program, &next, frame);
                let value_expr = match &stmt {
                    SemOp::ChannelSend { value, .. } => value.clone(),
                    _ => {
                        return Err(BackendError::invalid("E999", "SendRegister on non-send"))
                    }
                };
                let v = self.eval(&next, frame, &value_expr, &at)?;
                if !self.payload_ok(*channel, &v) {
                    disabled!();
                }
                let out = t.next.unwrap();
                next.put(
                    out,
                    NetToken::SendWait {
                        thread,
                        frame,
                        value: v,
                    },
                );
                self.set_blocked(&mut next, thread, PlaceKey::ChannelSend(*channel));
            }
            NetOp::RecvRegister { channel } => {
                let (thread, frame) = control.unwrap();
                let send = self.place(&PlaceKey::ChannelSend(*channel)).unwrap();
                if !next.place_tokens(send).is_empty() {
                    disabled!();
                }
                let out = t.next.unwrap();
                next.put(out, NetToken::RecvWait { thread, frame });
                self.set_blocked(&mut next, thread, PlaceKey::ChannelRecv(*channel));
            }
            NetOp::SendPair { channel } => {
                let (thread, frame) = control.unwrap();
                let wait = match bound_wait(bind) {
                    Some(w) => w.clone(),
                    None => disabled!(),
                };
                let (rt, rf) = match &wait {
                    NetToken::RecvWait { thread, frame } => (*thread, *frame),
                    _ => disabled!(),
                };
                let stmt = current_stmt(self.program, &next, frame);
                let value_expr = match &stmt {
                    SemOp::ChannelSend { value, .. } => value.clone(),
                    _ => return Err(BackendError::invalid("E999", "SendPair on non-send")),
                };
                let v = self.eval(&next, frame, &value_expr, &at)?;
                if !self.payload_ok(*channel, &v) {
                    disabled!();
                }
                let recv_place = self.place(&PlaceKey::ChannelRecv(*channel)).unwrap();
                if !next.take_token(recv_place, &wait) {
                    disabled!();
                }
                if let Some(dst) = recv_dst(self.program, &next, rf) {
                    if dst != SlotRef::Discard {
                        if !self.write_dst(&mut next, rf, dst, v)? { disabled!(); }
                    }
                }
                self.wake_control_next(&mut next, &wait);
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
                let _ = rt;
            }
            NetOp::RecvPair { channel } => {
                let (thread, frame) = control.unwrap();
                let wait = match bound_wait(bind) {
                    Some(w) => w.clone(),
                    None => disabled!(),
                };
                let (st, sf, value) = match &wait {
                    NetToken::SendWait { thread, frame, value } => (*thread, *frame, value.clone()),
                    _ => disabled!(),
                };
                if !self.payload_ok(*channel, &value) {
                    disabled!();
                }
                let send_place = self.place(&PlaceKey::ChannelSend(*channel)).unwrap();
                if !next.take_token(send_place, &wait) {
                    disabled!();
                }
                if let Some(dst) = recv_dst(self.program, &next, frame) {
                    if dst != SlotRef::Discard {
                        if !self.write_dst(&mut next, frame, dst, value)? { disabled!(); }
                    }
                }
                self.wake_control_next(&mut next, &wait);
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
                let _ = (st, sf);
            }
            NetOp::Rendezvous { channel } => {
                let (send_token, recv_token) = match bind {
                    FireBind::Pair(a, b) => (a.clone(), b.clone()),
                    _ => disabled!(),
                };
                let (st, sf, value) = match &send_token {
                    NetToken::SendWait { thread, frame, value } => (*thread, *frame, value.clone()),
                    _ => disabled!(),
                };
                let (rt, rf) = match &recv_token {
                    NetToken::RecvWait { thread, frame } => (*thread, *frame),
                    _ => disabled!(),
                };
                if !self.payload_ok(*channel, &value) {
                    disabled!();
                }
                let send_place = self.place(&PlaceKey::ChannelSend(*channel)).unwrap();
                let recv_place = self.place(&PlaceKey::ChannelRecv(*channel)).unwrap();
                if !next.take_token(send_place, &send_token) || !next.take_token(recv_place, &recv_token) {
                    disabled!();
                }
                let dst = recv_dst(self.program, &next, rf);
                if let Some(dst) = dst {
                    if dst != SlotRef::Discard {
                        if !self.write_dst(&mut next, rf, dst, value)? { disabled!(); }
                    }
                }
                // Both sides resume after their wait statements.
                self.put_control_next(&mut next, &send_token)?;
                let recv_token2 = NetToken::RecvWait { thread: rt, frame: rf };
                self.put_control_next(&mut next, &recv_token2)?;
                let _ = (st, sf, rt);
            }
            NetOp::SendBuf { channel, expr } => {
                let (thread, frame) = control.unwrap();
                let cap = self.program.resource(*channel).capacity;
                let ch = self.place(&PlaceKey::Channel(*channel)).unwrap();
                let available = next
                    .read_data(ch)
                    .and_then(|v| if let Value::Array(a) = v { Some(a.len()) } else { None })
                    .unwrap_or(0);
                if available >= cap {
                    disabled!();
                }
                let v = self.eval(&next, frame, expr, &at)?;
                if !self.payload_ok(*channel, &v) {
                    disabled!();
                }
                if let Some(Value::Array(a)) = next.read_data(ch).cloned() {
                    let mut a = a;
                    a.push(v);
                    next.set_data(ch, Value::Array(a));
                }
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
            }
            NetOp::SendBufBlock { channel, expr } => {
                let (thread, frame) = control.unwrap();
                let cap = self.program.resource(*channel).capacity;
                let ch = self.place(&PlaceKey::Channel(*channel)).unwrap();
                let available = next
                    .read_data(ch)
                    .and_then(|v| if let Value::Array(a) = v { Some(a.len()) } else { None })
                    .unwrap_or(0);
                if available < cap {
                    disabled!();
                }
                let v = self.eval(&next, frame, expr, &at)?;
                if !self.payload_ok(*channel, &v) {
                    disabled!();
                }
                let out = t.next.unwrap();
                next.put(
                    out,
                    NetToken::SendWait {
                        thread,
                        frame,
                        value: v,
                    },
                );
                self.set_blocked(&mut next, thread, PlaceKey::ChannelSend(*channel));
            }
            NetOp::RecvBuf { channel, dst } => {
                let (thread, frame) = control.unwrap();
                let ch = self.place(&PlaceKey::Channel(*channel)).unwrap();
                let popped = match next.read_data(ch).cloned() {
                    Some(Value::Array(mut a)) if !a.is_empty() => {
                        let v = a.remove(0);
                        next.set_data(ch, Value::Array(a));
                        Some(v)
                    }
                    _ => None,
                };
                let Some(v) = popped else { disabled!() };
                if !self.write_dst(&mut next, frame, *dst, v)? { disabled!(); }
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
            }
            NetOp::RecvBufBlock { channel } => {
                let (thread, frame) = control.unwrap();
                let ch = self.place(&PlaceKey::Channel(*channel)).unwrap();
                let empty = next
                    .read_data(ch)
                    .and_then(|v| if let Value::Array(a) = v { Some(a.is_empty()) } else { None })
                    .unwrap_or(true);
                if !empty {
                    disabled!();
                }
                let out = t.next.unwrap();
                next.put(out, NetToken::RecvWait { thread, frame });
                self.set_blocked(&mut next, thread, PlaceKey::ChannelRecv(*channel));
            }
            NetOp::BufferDeliver { channel } => {
                let token = match bind {
                    FireBind::Wait(token) => token.clone(),
                    _ => disabled!(),
                };
                let (thread, frame, value) = match &token {
                    NetToken::SendWait { thread, frame, value } => (*thread, *frame, value.clone()),
                    _ => disabled!(),
                };
                let cap = self.program.resource(*channel).capacity;
                let ch = self.place(&PlaceKey::Channel(*channel)).unwrap();
                let available = next
                    .read_data(ch)
                    .and_then(|v| if let Value::Array(a) = v { Some(a.len()) } else { None })
                    .unwrap_or(0);
                if available >= cap {
                    disabled!();
                }
                if !self.payload_ok(*channel, &value) {
                    disabled!();
                }
                let send_place = self.place(&PlaceKey::ChannelSend(*channel)).unwrap();
                if !next.take_token(send_place, &token) {
                    disabled!();
                }
                if let Some(Value::Array(mut a)) = next.read_data(ch).cloned() {
                    a.push(value);
                    next.set_data(ch, Value::Array(a));
                }
                self.put_control_next(&mut next, &token)?;
                let _ = (thread, frame);
            }
            NetOp::BufferRecvWait { channel } => {
                let token = match bind {
                    FireBind::Wait(token) => token.clone(),
                    _ => disabled!(),
                };
                let (thread, frame) = match &token {
                    NetToken::RecvWait { thread, frame } => (*thread, *frame),
                    _ => disabled!(),
                };
                let ch = self.place(&PlaceKey::Channel(*channel)).unwrap();
                let popped = match next.read_data(ch).cloned() {
                    Some(Value::Array(mut a)) if !a.is_empty() => {
                        let v = a.remove(0);
                        next.set_data(ch, Value::Array(a));
                        Some(v)
                    }
                    _ => None,
                };
                let Some(v) = popped else { disabled!() };
                let recv_place = self.place(&PlaceKey::ChannelRecv(*channel)).unwrap();
                if !next.take_token(recv_place, &token) {
                    disabled!();
                }
                let dst = recv_dst(self.program, &next, frame);
                if let Some(dst) = dst {
                    if dst != SlotRef::Discard {
                        if !self.write_dst(&mut next, frame, dst, v)? { disabled!(); }
                    }
                }
                self.put_control_next(&mut next, &token)?;
                let _ = thread;
            }
            NetOp::Goto { target: _ } => {
                let (thread, frame) = control.unwrap();
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
            }
            NetOp::BranchThen { cond, target: _ } => {
                let (thread, frame) = control.unwrap();
                let v = self.eval(&next, frame, cond, &at)?;
                let take = v.as_bool().ok_or_else(|| {
                    BackendError::invalid("E201", "branch condition is not Bool").at(&at)
                })?;
                if !take {
                    disabled!();
                }
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
            }
            NetOp::BranchElse { cond, target: _ } => {
                let (thread, frame) = control.unwrap();
                let v = self.eval(&next, frame, cond, &at)?;
                let take = v.as_bool().ok_or_else(|| {
                    BackendError::invalid("E201", "branch condition is not Bool").at(&at)
                })?;
                if take {
                    disabled!();
                }
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
            }
            NetOp::SwitchCase { var, label, target: _ } => {
                let (thread, frame) = control.unwrap();
                let v = self.eval(&next, frame, var, &at)?;
                if !value_matches_label(&v, label) {
                    disabled!();
                }
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
            }
            NetOp::SwitchDefault {
                var,
                labels,
                target: _,
            } => {
                let (thread, frame) = control.unwrap();
                let v = self.eval(&next, frame, var, &at)?;
                if labels.iter().any(|l| value_matches_label(&v, l)) {
                    disabled!();
                }
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
            }
            NetOp::Call { func, args, dst } => {
                let (thread, frame) = control.unwrap();
                let depth = next.store.threads[&thread].stack.len();
                if depth >= self.bounds.max_frames_per_thread {
                    boundary.push(BoundaryEvent::new(
                        BoundaryKind::FrameLimit,
                        format!(
                            "frame limit {} reached while calling",
                            self.bounds.max_frames_per_thread
                        ),
                    ));
                    disabled!();
                }
                let mut values = Vec::with_capacity(args.len());
                for a in args {
                    values.push(self.eval(&next, frame, a, &at)?);
                }
                let callee = self.program.function(*func);
                let modeled = super::net::modeled_params(callee);
                // Validate every argument against its parameter's domain
                // before the frame is created.
                for (slot, val) in modeled.iter().zip(values.iter()) {
                    if !within_type(val, &callee.slots[*slot].ty) {
                        disabled!();
                    }
                }
                let fid = FrameId(next.store.alloc.next_frame);
                next.store.alloc.next_frame += 1;
                let mut locals = default_locals(self.program, *func);
                for (slot, val) in modeled.iter().zip(values) {
                    locals.insert(*slot, val);
                }
                let caller_pc = next.store.frame(frame).pc;
                next.store.frames.insert(
                    fid,
                    NetFrame {
                        id: fid,
                        function: *func,
                        pc: 0,
                        locals,
                        handles: Default::default(),
                        ret: Some(NetRetAddr {
                            pc_next: caller_pc + 1,
                            dst: dst.unwrap_or(SlotRef::Discard),
                        }),
                    },
                );
                next.store.threads.get_mut(&thread).unwrap().stack.push(fid);
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, fid);
            }
            NetOp::Spawn { func, handle } => {
                let (thread, frame) = control.unwrap();
                if !self.within_bound(&next, *func) {
                    disabled!();
                }
                let Some(child) = self.create_thread(&mut next, *func, None, boundary)? else {
                    disabled!();
                };
                let hid = HandleId(next.store.alloc.next_handle);
                next.store.alloc.next_handle += 1;
                next.store
                    .frame_mut(frame)
                    .handles
                    .insert(handle.clone(), hid);
                next.store
                    .threads
                    .get_mut(&thread)
                    .unwrap()
                    .handle_children
                    .insert(hid, child);
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
            }
            NetOp::Scope { funcs } => {
                let (thread, frame) = control.unwrap();
                if funcs.is_empty() {
                    return Err(BackendError::invalid("E410", "scope funcs is empty").at(&at));
                }
                for func in funcs {
                    if !self.within_bound(&next, *func) {
                        disabled!();
                    }
                }
                let scope = ScopeId(next.store.alloc.next_scope);
                next.store.alloc.next_scope += 1;
                let mut remaining = BTreeSet::new();
                for func in funcs {
                    let Some(child) = self.create_thread(&mut next, *func, Some(scope), boundary)?
                    else {
                        disabled!();
                    };
                    if !next.store.finished.contains(&child) {
                        remaining.insert(child);
                    }
                }
                let owner_sid = next.store.frame(frame).pc;
                let owner_function = next.store.frame(frame).function;
                if remaining.is_empty() {
                    next.store
                        .completed_scopes
                        .insert((owner_function, owner_sid));
                    let out = t.next.unwrap();
                    self.place_control(&mut next, out, thread, frame);
                } else {
                    next.store.scopes.insert(
                        scope,
                        NetScope {
                            id: scope,
                            owner: thread,
                            owner_frame: frame,
                            owner_sid,
                            remaining,
                        },
                    );
                    let sw = self
                        .place(&PlaceKey::ScopeWait {
                            function: owner_function,
                            sid: owner_sid,
                        })
                        .unwrap();
                    next.put(sw, NetToken::ScopeWait { thread, frame });
                    self.set_blocked(&mut next, thread, PlaceKey::ScopeWait {
                        function: owner_function,
                        sid: owner_sid,
                    });
                }
            }
            NetOp::JoinReady { handle } => {
                let (thread, frame) = control.unwrap();
                let hid = next.store.frame(frame)
                    .handles
                    .get(handle)
                    .copied()
                    .ok_or_else(|| {
                        BackendError::invalid("E402", format!("join handle '{handle}' never spawned"))
                            .at(&at)
                    })?;
                let child = next.store.threads[&thread].handle_children.get(&hid).copied();
                let Some(c) = child else { disabled!() };
                if !next.store.finished.contains(&c) {
                    disabled!();
                }
                // Consume the handle binding and reclaim the finished child.
                next.store.frame_mut(frame).handles.remove(handle);
                next.store
                    .threads
                    .get_mut(&thread)
                    .unwrap()
                    .handle_children
                    .remove(&hid);
                next.store.threads.remove(&c);
                next.store.finished.remove(&c);
                let out = t.next.unwrap();
                self.place_control(&mut next, out, thread, frame);
            }
            NetOp::JoinBlock { handle } => {
                let (thread, frame) = control.unwrap();
                let hid = next.store.frame(frame)
                    .handles
                    .get(handle)
                    .copied()
                    .ok_or_else(|| {
                        BackendError::invalid("E402", format!("join handle '{handle}' never spawned"))
                            .at(&at)
                    })?;
                let child = next.store.threads[&thread].handle_children.get(&hid).copied();
                match child {
                    Some(c) if next.store.finished.contains(&c) => disabled!(),
                    Some(_) => {}
                    None => disabled!(),
                }
                let f = next.store.frame(frame);
                let key = PlaceKey::JoinWait {
                    function: f.function,
                    sid: f.pc,
                };
                let out = t.next.unwrap();
                next.put(out, NetToken::JoinWait { thread, frame });
                self.set_blocked(&mut next, thread, key);
            }
            NetOp::ReturnInner { value } => {
                let (thread, frame) = control.unwrap();
                if next.store.threads[&thread].stack.len() <= 1 {
                    disabled!();
                }
                let val = match value {
                    Some(e) => Some(self.eval(&next, frame, e, &at)?),
                    None => None,
                };
                // Check the callee's declared return type before unwinding.
                let returned = self.program.function(next.store.frame(frame).function).returns.clone();
                if let (Some(ret), Some(v)) = (&returned, &val) {
                    if !within_type(v, &ret.ty) {
                        disabled!();
                    }
                }
                let callee = next.store.frame(frame).clone();
                next.store.frames.remove(&frame);
                next.store.threads.get_mut(&thread).unwrap().stack.pop();
                let caller = *next.store.threads[&thread].stack.last().unwrap();
                if let Some(ret) = &callee.ret {
                    if let Some(v) = val {
                        if !self.write_dst(&mut next, caller, ret.dst, v)? { disabled!(); }
                    }
                    let caller_pc = ret.pc_next;
                    let caller_fn = next.store.frame(caller).function;
                    if let Some(dest) = self.ctrl(caller_fn, caller_pc) {
                        self.place_control(&mut next, dest, thread, caller);
                    }
                }
                self.record_completion(&mut next, callee.function);
            }
            NetOp::ReturnFinal { value } => {
                let (thread, frame) = control.unwrap();
                if next.store.threads[&thread].stack.len() != 1 {
                    disabled!();
                }
                let val = match value {
                    Some(e) => Some(self.eval(&next, frame, e, &at)?),
                    None => None,
                };
                let function = next.store.frame(frame).function;
                // Check the declared return type before finishing the thread.
                if let (Some(ret), Some(v)) = (&self.program.function(function).returns, &val) {
                    if !within_type(v, &ret.ty) {
                        disabled!();
                    }
                }
                next.store.frames.remove(&frame);
                next.store.threads.get_mut(&thread).unwrap().stack.pop();
                self.finish_thread(&mut next, thread, function);
            }
        }

        Ok(Some(next))
    }

    /// Stable `module::entity` name of a resource.
    fn rname(&self, r: ResourceId) -> String {
        let res = self.program.resource(r);
        crate::fqn::fqn(self.program.module_name(res.module), &res.name)
    }

    /// Check a channel payload against the channel's own declared base type.
    fn payload_ok(&self, channel: ResourceId, value: &Value) -> bool {
        match self.program.resource(channel).ty.as_ref() {
            Some(ty) => within_type(value, ty),
            None => true,
        }
    }

    fn dst_type_ok(&self, state: &NetState, frame: FrameId, dst: SlotRef, value: &Value) -> bool {
        match dst {
            SlotRef::Discard => true,
            SlotRef::Local(slot) => match state.store.frames.get(&frame) {
                Some(fr) => match self.program.function(fr.function).slots.get(slot) {
                    Some(s) => within_type(value, &s.ty),
                    None => true,
                },
                None => true,
            },
            SlotRef::Shared(r) => match self.program.resource(r).ty.as_ref() {
                Some(ty) => within_type(value, ty),
                None => true,
            },
        }
    }

    /// Returns `Ok(false)` if the value is outside the destination's domain,
    /// in which case nothing is written and the step must be disabled.
    fn write_dst(
        &self,
        state: &mut NetState,
        frame: FrameId,
        dst: SlotRef,
        value: Value,
    ) -> BackendResult<bool> {
        if !self.dst_type_ok(state, frame, dst, &value) {
            return Ok(false);
        }
        match dst {
            SlotRef::Discard => Ok(true),
            SlotRef::Local(slot) => {
                state.store.frame_mut(frame).locals.insert(slot, value);
                Ok(true)
            }
            SlotRef::Shared(r) => {
                if let Some(pid) = self.place(&PlaceKey::Var(r)) {
                    state.set_data(pid, value);
                } else if let Some(pid) = self.place(&PlaceKey::Atomic(r)) {
                    state.set_data(pid, value);
                } else {
                    return Err(BackendError::invalid(
                        "E900",
                        format!("no place for shared resource {r}"),
                    ));
                }
                Ok(true)
            }
        }
    }
}

fn current_stmt(program: &SemProgram, state: &NetState, frame: FrameId) -> SemOp {
    let f = state.store.frame(frame);
    program.function(f.function).body[f.pc].op.clone()
}

fn recv_dst(program: &SemProgram, state: &NetState, frame: FrameId) -> Option<SlotRef> {
    match current_stmt(program, state, frame) {
        SemOp::ChannelRecv { dst, .. } => Some(dst),
        _ => None,
    }
}

fn value_matches_label(v: &Value, label: &str) -> bool {
    match v {
        Value::Enum(e) => e == label,
        Value::Int(i) => label.parse::<i64>().ok() == Some(*i),
        _ => false,
    }
}

// Small compatibility helpers for thread identity in NetThread.

impl<'a> TransitionSystem for PetriEngine<'a> {
    type State = NetState;

    fn program(&self) -> &SemProgram {
        self.program
    }

    fn initial(&self) -> BackendResult<NetState> {
        let mut state = NetState {
            marking: Default::default(),
            store: NetStore {
                frames: Default::default(),
                threads: Default::default(),
                scopes: Default::default(),
                finished: Default::default(),
                completed_functions: Default::default(),
                completed_scopes: Default::default(),
                reached: Default::default(),
                alloc: NetAlloc::default(),
            },
        };
        // Initialize resource places that exist in the net.
        for place in &self.net.places {
            match &place.key {
                PlaceKey::Var(r) => {
                    if let Some(v) = self.program.resource(*r).init.clone() {
                        state.set_data(place.id, v);
                    }
                }
                PlaceKey::Atomic(r) => {
                    if let Some(v) = self.program.resource(*r).init.clone() {
                        state.set_data(place.id, v);
                    }
                }
                PlaceKey::Mutex(_) => state.set_mutex(place.id, MutexToken::Free),
                PlaceKey::Semaphore(r) => {
                    let n = self.program.resource(*r).permits;
                    state.set_data(place.id, Value::Int(n));
                }
                PlaceKey::Channel(_) => state.set_data(place.id, Value::Array(Vec::new())),
                _ => {}
            }
        }
        // Entry thread.
        let entry = self.program.entry();
        let tid = ThreadId(0);
        state.store.alloc.next_thread = 1;
        state.store.threads.insert(
            tid,
            NetThread {
                entry_function: entry,
                stack: Vec::new(),
                    handle_children: Default::default(),
                parent_scope: None,
                blocked_at: None,
            },
        );
        if self.program.function(entry).is_transparent_nobody() {
            state.store.finished.insert(tid);
            self.record_completion(&mut state, entry);
            return Ok(state);
        }
        let fid = FrameId(0);
        state.store.alloc.next_frame = 1;
        state.store.frames.insert(
            fid,
            NetFrame {
                id: fid,
                function: entry,
                pc: 0,
                locals: default_locals(self.program, entry),
                handles: Default::default(),
                ret: None,
            },
        );
        state.store.threads.get_mut(&tid).unwrap().stack.push(fid);
        let start = self.ctrl(entry, 0).ok_or_else(|| {
            BackendError::invalid("E602", "entry function has no first statement")
        })?;
        self.place_control(&mut state, start, tid, fid);
        Ok(state)
    }

    fn successors(&self, state: &NetState) -> BackendResult<Enabled<NetState>> {
        let mut enabled = Enabled::empty();
        for t in &self.net.transitions {
            match &t.binding {
                Binding::Control(pid) => {
                    let tokens: Vec<NetToken> = state
                        .place_tokens(*pid)
                        .iter()
                        .filter(|t| matches!(t, NetToken::Control { .. }))
                        .cloned()
                        .collect();
                    for token in tokens {
                        let (thread, frame) = match &token {
                            NetToken::Control { thread, frame } => (*thread, *frame),
                            _ => continue,
                        };
                        let bind = FireBind::Control { thread, frame };
                        if let Some(s) =
                            self.fire(state, t, &bind, &mut enabled.boundary)?
                        {
                            let label = StepLabel::new(t.origin.clone())
                                .with_binding(thread, frame);
                            enabled.steps.push(Step { label, state: s });
                        }
                    }
                }
                Binding::Wait(pid) => {
                    // Every waiting token is an independent choice; blocking
                    // order is not part of the CIR semantics.
                    let tokens: Vec<NetToken> = state.place_tokens(*pid).to_vec();
                    for token in tokens {
                        let bind = FireBind::Wait(token);
                        if let Some(s) =
                            self.fire(state, t, &bind, &mut enabled.boundary)?
                        {
                            let (thread, frame) = binding_ids(&bind);
                            let label = StepLabel::new(t.origin.clone())
                                .with_binding(thread, frame);
                            enabled.steps.push(Step { label, state: s });
                        }
                    }
                }
                Binding::Pair(a, b) => {
                    let front_a = state.place_tokens(*a).first().cloned();
                    let front_b = state.place_tokens(*b).first().cloned();
                    if let (Some(ta), Some(tb)) = (front_a, front_b) {
                        let bind = FireBind::Pair(ta, tb);
                        if let Some(s) =
                            self.fire(state, t, &bind, &mut enabled.boundary)?
                        {
                            let (thread, frame) = binding_ids(&bind);
                            let label = StepLabel::new(t.origin.clone())
                                .with_binding(thread, frame);
                            enabled.steps.push(Step { label, state: s });
                        }
                    }
                }
                Binding::ControlWait(c, w) => {
                    let tokens: Vec<NetToken> = state
                        .place_tokens(*c)
                        .iter()
                        .filter(|t| matches!(t, NetToken::Control { .. }))
                        .cloned()
                        .collect();
                    // Channel message order is FIFO: pair with the front waiter.
                    let wait = state.place_tokens(*w).first().cloned();
                    if let Some(wait) = wait {
                        for token in tokens {
                            let (thread, frame) = match &token {
                                NetToken::Control { thread, frame } => (*thread, *frame),
                                _ => continue,
                            };
                            let bind = FireBind::ControlWait {
                                thread,
                                frame,
                                wait: wait.clone(),
                            };
                            if let Some(s) =
                                self.fire(state, t, &bind, &mut enabled.boundary)?
                            {
                                let label = StepLabel::new(t.origin.clone())
                                    .with_binding(thread, frame);
                                enabled.steps.push(Step { label, state: s });
                            }
                        }
                    }
                }
                Binding::ControlChooseWait(c, w) => {
                    let tokens: Vec<NetToken> = state
                        .place_tokens(*c)
                        .iter()
                        .filter(|t| matches!(t, NetToken::Control { .. }))
                        .cloned()
                        .collect();
                    // notify_one: every current waiter is a legal choice.
                    let waits: Vec<NetToken> = state.place_tokens(*w).to_vec();
                    for wait in waits {
                        for token in &tokens {
                            let (thread, frame) = match token {
                                NetToken::Control { thread, frame } => (*thread, *frame),
                                _ => continue,
                            };
                            let bind = FireBind::ControlChooseWait {
                                thread,
                                frame,
                                wait: wait.clone(),
                            };
                            if let Some(s) =
                                self.fire(state, t, &bind, &mut enabled.boundary)?
                            {
                                let label = StepLabel::new(t.origin.clone())
                                    .with_binding(thread, frame);
                                enabled.steps.push(Step { label, state: s });
                            }
                        }
                    }
                }
            }
        }
        Ok(enabled)
    }

    fn is_finished(&self, state: &NetState) -> bool {
        state
            .store
            .threads
            .keys()
            .all(|t| state.store.finished.contains(t))
    }

    fn blocked(&self, state: &NetState) -> Vec<BlockedRecord> {
        let mut out = Vec::new();
        for (tid, t) in &state.store.threads {
            if state.store.finished.contains(tid) {
                continue;
            }
            let Some(key) = &t.blocked_at else { continue };
            let (kind, resource, holder, waiting, detail) = match key {
                PlaceKey::LockWait(r) | PlaceKey::Condvar(r) => {
                    let pid = self.place(key).unwrap();
                    let mtx = self.place(&PlaceKey::Mutex(*r));
                    let holder = mtx.and_then(|m| match state.read_mutex(m) {
                        Some(MutexToken::Held(h)) => Some(h),
                        _ => None,
                    });
                    (
                        if matches!(key, PlaceKey::Condvar(_)) {
                            BlockKind::Condvar
                        } else {
                            BlockKind::Lock
                        },
                        Some(*r),
                        holder,
                        state.place_tokens(pid).len(),
                        "waiting for a lock".to_string(),
                    )
                }
                PlaceKey::ChannelSend(c) => (
                    BlockKind::ChannelSend,
                    Some(*c),
                    None,
                    self.place(key).map(|p| state.place_tokens(p).len()).unwrap_or(0),
                    "channel send blocked".to_string(),
                ),
                PlaceKey::ChannelRecv(c) => (
                    BlockKind::ChannelRecv,
                    Some(*c),
                    None,
                    self.place(key).map(|p| state.place_tokens(p).len()).unwrap_or(0),
                    "channel receive blocked".to_string(),
                ),
                PlaceKey::SemWait(r) => (
                    BlockKind::Semaphore,
                    Some(*r),
                    None,
                    self.place(key).map(|p| state.place_tokens(p).len()).unwrap_or(0),
                    "waiting for semaphore permits".to_string(),
                ),
                PlaceKey::JoinWait { .. } => (
                    BlockKind::Join,
                    None,
                    None,
                    0,
                    "waiting to join a child thread".to_string(),
                ),
                PlaceKey::ScopeWait { .. } => (
                    BlockKind::Scope,
                    None,
                    None,
                    0,
                    "waiting for scope members".to_string(),
                ),
                _ => continue,
            };
            out.push(BlockedRecord {
                thread: *tid,
                kind,
                resource,
                resource_name: resource.map(|r| self.rname(r)),
                holder,
                waiting,
                detail,
            });
        }
        out
    }

    fn instances(&self, state: &NetState) -> Vec<InstanceState> {
        let mut out = Vec::new();
        for (tid, t) in &state.store.threads {
            let (function, sid, status) = match state.store.current_frame(*tid) {
                Some(fid) => {
                    let f = state.store.frame(fid);
                    (f.function, Some(f.pc), "running".to_string())
                }
                None => (t.entry_function, None, "finished".to_string()),
            };
            out.push(InstanceState {
                thread: *tid,
                frame: state.store.current_frame(*tid),
                function,
                sid,
                status,
            });
        }
        out
    }

    fn satisfied(&self, state: &NetState, predicate: &Predicate) -> bool {
        let read_var = |r: ResourceId| -> Option<&Value> {
            if let Some(pid) = self.place(&PlaceKey::Var(r)) {
                state.read_data(pid)
            } else if let Some(pid) = self.place(&PlaceKey::Atomic(r)) {
                state.read_data(pid)
            } else {
                None
            }
        };
        match predicate {
            Predicate::True => true,
            Predicate::False => false,
            Predicate::VarEq { resource, value } => read_var(*resource) == Some(value),
            Predicate::VarCmp { resource, op, value } => read_var(*resource)
                .and_then(|v| compare_values(*op, v, value))
                .unwrap_or(false),
            Predicate::FunctionCompleted { func } => {
                state.store.completed_functions.get(func).copied().unwrap_or(0) >= 1
            }
            Predicate::FunctionCompletedAtLeast { func, n } => {
                state.store.completed_functions.get(func).copied().unwrap_or(0) >= *n
            }
            Predicate::ScopeCompleted { func, sid } => {
                state.store.completed_scopes.contains(&(*func, *sid))
            }
            Predicate::StatementReached { func, sid } => state.store.reached.contains(&(*func, *sid)),
            Predicate::MutexFree(r) => self
                .place(&PlaceKey::Mutex(*r))
                .and_then(|p| state.read_mutex(p))
                .map(|m| matches!(m, MutexToken::Free))
                .unwrap_or(false),
            Predicate::MutexHeld(r) => self
                .place(&PlaceKey::Mutex(*r))
                .and_then(|p| state.read_mutex(p))
                .map(|m| matches!(m, MutexToken::Held(_)))
                .unwrap_or(false),
            Predicate::ChannelEmpty(r) => self
                .place(&PlaceKey::Channel(*r))
                .and_then(|p| state.read_data(p))
                .map(|v| matches!(v, Value::Array(a) if a.is_empty()))
                .unwrap_or(true),
            Predicate::ChannelAtLeast { resource, len } => self
                .place(&PlaceKey::Channel(*resource))
                .and_then(|p| state.read_data(p))
                .map(|v| matches!(v, Value::Array(a) if a.len() >= *len))
                .unwrap_or(false),
            Predicate::Not(p) => !self.satisfied(state, p),
            Predicate::And(ps) => ps.iter().all(|p| self.satisfied(state, p)),
            Predicate::Or(ps) => ps.iter().any(|p| self.satisfied(state, p)),
        }
    }

    fn canonical(&self, state: &NetState) -> String {
        render_net(&self.net, state, &|v| v.canonical())
    }

    fn state_key(&self, state: &NetState) -> String {
        render_net(&self.net, state, &|v| v.key())
    }}

fn render_net(net: &PetriNet, state: &NetState, enc: &dyn Fn(&Value) -> String) -> String {
    let mut out = String::new();
    let thread_order: Vec<ThreadId> = state.store.threads.keys().copied().collect();
    let frame_order: Vec<FrameId> = state.store.frames.keys().copied().collect();
    let scope_order: Vec<ScopeId> = state.store.scopes.keys().copied().collect();
    let mut handle_ids: Vec<HandleId> = state
        .store
        .frames
        .values()
        .flat_map(|f| f.handles.values().copied())
        .chain(
            state
                .store
                .threads
                .values()
                .flat_map(|t| t.handle_children.keys().copied()),
        )
        .collect();
    handle_ids.sort();
    handle_ids.dedup();
    let tname = |t: ThreadId| {
        thread_order
            .iter()
            .position(|x| *x == t)
            .map(|i| format!("T{i}"))
            .unwrap_or_else(|| format!("T?{}", t.0))
    };
    let fname = |f: FrameId| {
        frame_order
            .iter()
            .position(|x| *x == f)
            .map(|i| format!("F{i}"))
            .unwrap_or_else(|| format!("F?{}", f.0))
    };
    let sname = |s: ScopeId| {
        scope_order
            .iter()
            .position(|x| *x == s)
            .map(|i| format!("S{i}"))
            .unwrap_or_else(|| format!("S?{}", s.0))
    };
    let hname = |h: HandleId| {
        handle_ids
            .iter()
            .position(|x| *x == h)
            .map(|i| format!("h{i}"))
            .unwrap_or_else(|| format!("h?{}", h.0))
    };
    for place in &net.places {
        let tokens = state.place_tokens(place.id);
        if tokens.is_empty() {
            continue;
        }
        let rendered: Vec<String> = tokens
            .iter()
            .map(|t| render_token(t, &tname, &fname, enc))
            .collect();
        out.push_str(&format!("P{:?}=[{}]\n", place.key, rendered.join(",")));
    }
    for f in &frame_order {
        let fr = &state.store.frames[f];
        let mut locals: Vec<String> = fr
            .locals
            .iter()
            .map(|(k, v)| format!("{k}={}", enc(v)))
            .collect();
        locals.sort();
        let mut handles: Vec<String> = fr
            .handles
            .iter()
            .map(|(k, h)| format!("{k}->{}", hname(*h)))
            .collect();
        handles.sort();
        let ret = match &fr.ret {
            Some(r) => format!("ret(pc={},dst={:?})", r.pc_next, r.dst),
            None => "ret(none)".to_string(),
        };
        out.push_str(&format!(
            "frame {} fn={} pc={} locals={{{}}} handles={{{}}} {}\n",
            fname(*f),
            fr.function.0,
            fr.pc,
            locals.join(","),
            handles.join(","),
            ret
        ));
    }
    for (tid, t) in &state.store.threads {
        let mut kids: Vec<String> = t
            .handle_children
            .iter()
            .map(|(h, c)| format!("{}->{}", hname(*h), tname(*c)))
            .collect();
        kids.sort();
        out.push_str(&format!(
            "thread {} entry=f{} stack=[{}] finished={} children=[{}] scope={}\n",
            tname(*tid),
            t.entry_function.0,
            t.stack.iter().map(|f| fname(*f)).collect::<Vec<_>>().join(","),
            state.store.finished.contains(tid),
            kids.join(","),
            t.parent_scope.map(|s| sname(s)).unwrap_or_else(|| "-".into())
        ));
    }
    for sc in state.store.scopes.values() {
        let mut rem: Vec<String> = sc.remaining.iter().map(|t| tname(*t)).collect();
        rem.sort();
        out.push_str(&format!(
            "scope {} owner={} frame={} sid={} remaining=[{}]\n",
            sname(sc.id),
            tname(sc.owner),
            fname(sc.owner_frame),
            sc.owner_sid,
            rem.join(",")
        ));
    }
    out.push_str(&format!(
        "completed fn={:?} scopes={:?} reached={:?}\n",
        state.store.completed_functions,
        state.store.completed_scopes,
        state.store.reached
    ));
    out
}

fn bound_wait(bind: &FireBind) -> Option<&NetToken> {
    match bind {
        FireBind::ControlWait { wait, .. } | FireBind::ControlChooseWait { wait, .. } => Some(wait),
        _ => None,
    }
}

fn binding_ids(bind: &FireBind) -> (ThreadId, FrameId) {
    match bind {
        FireBind::Control { thread, frame }
        | FireBind::ControlWait { thread, frame, .. }
        | FireBind::ControlChooseWait { thread, frame, .. } => (*thread, *frame),
        FireBind::Wait(t) => (
            t.thread().unwrap_or(ThreadId(0)),
            t.frame().unwrap_or(FrameId(0)),
        ),
        FireBind::Pair(a, _) => (
            a.thread().unwrap_or(ThreadId(0)),
            a.frame().unwrap_or(FrameId(0)),
        ),
    }
}

fn render_token(
    t: &NetToken,
    tname: &impl Fn(ThreadId) -> String,
    fname: &impl Fn(FrameId) -> String,
    enc: &dyn Fn(&Value) -> String,
) -> String {
    match t {
        NetToken::Control { thread, frame } => format!("C({},{})", tname(*thread), fname(*frame)),
        NetToken::Data(v) => format!("D({})", enc(v)),
        NetToken::Mutex(MutexToken::Free) => "M(free)".into(),
        NetToken::Mutex(MutexToken::Held(t)) => format!("M(held:{})", tname(*t)),
        NetToken::LockWait { thread, .. } => format!("L({})", tname(*thread)),
        NetToken::CondvarWait { thread, lock, .. } => {
            format!("CV({},r{})", tname(*thread), lock.0)
        }
        NetToken::SendWait { thread, value, .. } => {
            format!("S({},{})", tname(*thread), enc(value))
        }
        NetToken::RecvWait { thread, .. } => format!("R({})", tname(*thread)),
        NetToken::SemWait { thread, count, .. } => format!("Q({},{count})", tname(*thread)),
        NetToken::JoinWait { thread, .. } => format!("J({})", tname(*thread)),
        NetToken::ScopeWait { thread, .. } => format!("W({})", tname(*thread)),
    }
}
