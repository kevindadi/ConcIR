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
    pub(super) fn eval(
        &self,
        state: &MachineState,
        frame: FrameId,
        expr: &concir_sem::eval::LExpr,
        at: &str,
    ) -> BackendResult<Value> {
        let view = FrameView {
            frame: state.frame(frame),
            store: &state.store,
        };
        eval(expr, &view, at)
    }

    pub(super) fn record_completion(&self, state: &mut MachineState, function: FunctionId) {
        let max = self.monitor.max_for(function);
        if max == 0 {
            return;
        }
        let e = state.completed_functions.entry(function).or_insert(0);
        if *e < max {
            *e += 1;
        }
    }

    /// Whether writing `value` to `dst` respects the destination's declared
    /// domain (bounded `Int`). This is checked for *every* value-entry path,
    /// not only explicit assignments.
    /// Stable `module::entity` name of a resource.
    pub(super) fn rname(&self, r: ResourceId) -> String {
        let res = self.program.resource(r);
        concir_ir::fqn::fqn(self.program.module_name(res.module), &res.name)
    }

    pub(super) fn dst_type_ok(
        &self,
        state: &MachineState,
        frame: FrameId,
        dst: SlotRef,
        value: &Value,
    ) -> bool {
        match dst {
            SlotRef::Discard => true,
            SlotRef::Local(slot) => {
                let f = self.program.function(state.frame(frame).function);
                match f.slots.get(slot) {
                    Some(s) => within_type(value, &s.ty),
                    None => true,
                }
            }
            SlotRef::Shared(r) => match self.program.resource(r).ty.as_ref() {
                Some(ty) => within_type(value, ty),
                None => true,
            },
        }
    }

    /// Checked write: returns `false` (and writes nothing) if the value is
    /// outside the destination's domain, so the enclosing step is disabled.
    pub(super) fn try_write_dst(
        &self,
        state: &mut MachineState,
        frame: FrameId,
        dst: SlotRef,
        value: Value,
    ) -> BackendResult<bool> {
        if !self.dst_type_ok(state, frame, dst, &value) {
            return Ok(false);
        }
        state.write_dst(frame, dst, value)?;
        Ok(true)
    }

    pub(super) fn complete(&self, state: &mut MachineState, tid: ThreadId) {
        state.set_runnable(tid);
        state.advance(tid);
    }

    pub(super) fn recv_dst_of(&self, state: &MachineState, tid: ThreadId) -> Option<SlotRef> {
        let fid = state.current_frame_id(tid)?;
        let pc = state.frame(fid).pc;
        let func = state.frame(fid).function;
        match &self.program.function(func).body.get(pc)?.op {
            SemOp::ChannelRecv { dst, .. } => Some(*dst),
            _ => None,
        }
    }

    /// Deliver a value to a blocked receiver's `dst`. Returns `Ok(false)` if
    /// the destination's domain rejects the value, so the whole step is
    /// disabled (no message is consumed).
    pub(super) fn thread_holds(
        &self,
        state: &MachineState,
        tid: ThreadId,
        r: concir_sem::ids::ResourceId,
    ) -> bool {
        use concir_sem::program::ResKind;
        match self.program.resource(r).kind {
            ResKind::Mutex => {
                matches!(state.store.mutexes.get(&r), Some(MutexState::Held(h)) if *h == tid)
            }
            ResKind::RwLock => match state.store.rwlocks.get(&r) {
                Some(RwLockState::Reading(readers)) => readers.contains(&tid),
                Some(RwLockState::Writing(w)) => *w == tid,
                _ => false,
            },
            ResKind::Semaphore => {
                state.store.semaphores.get(&r).copied().unwrap_or(0)
                    < self.program.resource(r).permits
            }
            _ => false,
        }
    }

    /// Apply a `return` (explicit or the implicit return at a function's end).
    /// Returns `Ok(false)` when the step is disabled (return type or caller
    /// destination domain violation), so the caller emits no successor.
    pub(super) fn apply_return(
        &self,
        next: &mut MachineState,
        tid: ThreadId,
        fid: FrameId,
        function_id: FunctionId,
        function: &concir_sem::program::SemFunction,
        val: Option<Value>,
    ) -> BackendResult<bool> {
        // First check the callee's own declared return type, then the caller's
        // destination; both must hold before any unwinding, write, completion,
        // or wake-up.
        if let (Some(ret), Some(v)) = (&function.returns, &val) {
            if !within_type(v, &ret.ty) {
                return Ok(false);
            }
        }
        if next.threads[&tid].stack.len() > 1 {
            let stack_len = next.threads[&tid].stack.len();
            let caller = next.threads[&tid].stack[stack_len - 2];
            let callee_frame = next.frame(fid).clone();
            if let (Some(ret), Some(v)) = (&callee_frame.ret, &val) {
                if !self.dst_type_ok(next, caller, ret.dst, v) {
                    return Ok(false);
                }
            }
            next.threads.get_mut(&tid).unwrap().stack.pop();
            next.store.frames.remove(&fid);
            if let Some(ret) = &callee_frame.ret {
                if let Some(v) = val {
                    next.write_dst(caller, ret.dst, v)?;
                }
                next.frame_mut(caller).pc = ret.pc_next;
            }
            self.record_completion(next, function_id);
        } else {
            next.threads.get_mut(&tid).unwrap().stack.pop();
            next.store.frames.remove(&fid);
            self.finish_thread(next, tid, function_id);
        }
        Ok(true)
    }

    pub(super) fn finish_thread(&self, state: &mut MachineState, tid: ThreadId, function: FunctionId) {
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
                // Reclaim scope members and the scope record: once the scope
                // completed they cannot be joined or observed again.
                let members: Vec<ThreadId> = state
                    .threads
                    .iter()
                    .filter(|(_, t)| t.parent_scope == Some(scope))
                    .map(|(id, _)| *id)
                    .collect();
                for m in members {
                    state.threads.remove(&m);
                    state.finished.remove(&m);
                }
                state.scopes.remove(&scope);
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
        let had_joiner = !joiners.is_empty();
        for j in joiners {
            // Consume the joiner's handle binding and child mapping.
            if let Some(t) = state.threads.get_mut(&j) {
                t.handle_children.retain(|_, c| *c != tid);
            }
            if let Some(jf) = state.current_frame_id(j) {
                let stmt = state.frame(jf).pc;
                let func = state.frame(jf).function;
                if let Some(SemOp::Join { handle }) =
                    self.program.function(func).body.get(stmt).map(|s| &s.op)
                {
                    let handle = handle.clone();
                    state.frame_mut(jf).handles.remove(&handle);
                }
            }
            self.complete(state, j);
        }
        if had_joiner {
            // The joiner consumed the child; reclaim its identity.
            state.threads.remove(&tid);
            state.finished.remove(&tid);
        }
    }

    pub(super) fn within_function_bound(&self, state: &MachineState, func: FunctionId) -> bool {
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

    pub(super) fn create_thread(
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
                format!(
                    "thread limit {} reached while spawning",
                    self.bounds.max_threads
                ),
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

    pub(super) fn step_label(
        &self,
        function: FunctionId,
        sid: usize,
        tid: ThreadId,
        fid: FrameId,
    ) -> StepLabel {
        StepLabel::new(TransitionOrigin {
            module: self.program.function(function).module,
            function,
            sid: Some(sid),
            phase: Phase::Statement,
        })
        .with_binding(tid, fid)
    }

    /// A step for a runnable thread.
    pub(super) fn step_thread(
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
            // Falling off the end is an implicit `return` (design §3).
            if !self.apply_return(&mut next, tid, fid, function_id, function, None)? {
                return Ok(Vec::new());
            }
            let label = self.step_label(function_id, pc, tid, fid);
            return Ok(vec![Step { label, state: next }]);
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
                    if !self.try_write_dst(&mut next, fid, dst, v)? {
                        return Ok(Vec::new());
                    }
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
                if !self.try_write_dst(&mut next, fid, dst, v)? {
                    return Ok(Vec::new());
                }
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
                if !self.try_write_dst(&mut next, fid, dst, old.clone())? {
                    return Ok(Vec::new());
                }
                if old == exp {
                    next.store.atomics.insert(resource, des);
                }
                next.advance(tid);
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
                    tid,
                    fid,
                    function_id,
                    pc,
                    op,
                    &at,
                    label,
                );
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
                            format!(
                                "frame limit {} reached while calling '{}'",
                                self.bounds.max_frames_per_thread, callee.name
                            ),
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
                        .filter(|(_, s)| {
                            s.class == concir_sem::program::SlotClass::Param && s.modeled
                        })
                        .map(|(i, _)| i)
                        .collect();
                    // Every argument binding must respect the parameter's
                    // declared domain before the frame is created.
                    for (slot, val) in modeled.iter().zip(values.iter()) {
                        if !within_type(val, &callee.slots[*slot].ty) {
                            return Ok(Vec::new());
                        }
                    }
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
                    let Some(child) =
                        self.create_thread(&mut next, *func, Some(scope), boundary)?
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
                let hid = next
                    .frame(fid)
                    .handles
                    .get(&handle)
                    .copied()
                    .ok_or_else(|| {
                        BackendError::invalid(
                            "E402",
                            format!("join handle '{handle}' was never spawned in this activation"),
                        )
                        .at(&at)
                    })?;
                let child = next.threads[&tid].handle_children.get(&hid).copied();
                match child {
                    Some(c) if next.finished.contains(&c) => {
                        // Consume the handle binding and reclaim the child, so
                        // no stale identity remains in the state.
                        next.frame_mut(fid).handles.remove(&handle);
                        next.threads
                            .get_mut(&tid)
                            .unwrap()
                            .handle_children
                            .remove(&hid);
                        next.threads.remove(&c);
                        next.finished.remove(&c);
                        next.advance(tid);
                    }
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
                if !self.apply_return(&mut next, tid, fid, function_id, function, val)? {
                    return Ok(Vec::new());
                }
            }
            SemOp::SeqHole {
                updates_modeled, ..
            } => {
                // Evaluate all modeled updates in the pre-state, validate domains,
                // then apply simultaneously as one atomic silent step.
                let mut evaluated = Vec::with_capacity(updates_modeled.len());
                for u in &updates_modeled {
                    let v = self.eval(&next, fid, &u.expr, &at)?;
                    if !self.dst_type_ok(&next, fid, u.target, &v) {
                        return Ok(Vec::new());
                    }
                    evaluated.push((u.target, v));
                }
                for (target, v) in evaluated {
                    next.write_dst(fid, target, v)?;
                }
                next.advance(tid);
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
