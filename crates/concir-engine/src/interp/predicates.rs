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
    pub(super) fn blocked_impl(&self, state: &MachineState) -> Vec<BlockedRecord> {
        let mut out = Vec::new();
        let count_lock = |r: &ResourceId| -> usize {
            state
                .threads
                .values()
                .filter(
                    |t| matches!(&t.status, ThreadStatus::Blocked(BlockReason::Lock(x)) if x == r),
                )
                .count()
        };
        for t in state.threads.values() {
            if let ThreadStatus::Blocked(reason) = &t.status {
                out.push(match reason {
                    BlockReason::Lock(r) => BlockedRecord {
                        thread: t.id,
                        kind: BlockKind::Lock,
                        resource: Some(*r),
                        resource_name: Some(self.rname(*r)),
                        holder: match state.store.mutexes.get(r) {
                            Some(MutexState::Held(h)) => Some(*h),
                            _ => None,
                        },
                        waiting: count_lock(r),
                        detail: "waiting for mutex".into(),
                    },
                    BlockReason::RwLockRead(r) => BlockedRecord {
                        thread: t.id,
                        kind: BlockKind::Lock,
                        resource: Some(*r),
                        resource_name: Some(self.rname(*r)),
                        holder: match state.store.rwlocks.get(r) {
                            Some(RwLockState::Writing(h)) => Some(*h),
                            _ => None,
                        },
                        waiting: 1,
                        detail: "waiting for rwlock read".into(),
                    },
                    BlockReason::RwLockWrite(r) => BlockedRecord {
                        thread: t.id,
                        kind: BlockKind::Lock,
                        resource: Some(*r),
                        resource_name: Some(self.rname(*r)),
                        holder: match state.store.rwlocks.get(r) {
                            Some(RwLockState::Writing(h)) => Some(*h),
                            Some(RwLockState::Reading(rs)) => rs.iter().next().copied(),
                            _ => None,
                        },
                        waiting: 1,
                        detail: "waiting for rwlock write".into(),
                    },
                    BlockReason::ChannelSend(c) => BlockedRecord {
                        thread: t.id,
                        kind: BlockKind::ChannelSend,
                        resource: Some(*c),
                        resource_name: Some(self.rname(*c)),
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
                        resource_name: Some(self.rname(*c)),
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
                        resource_name: Some(self.rname(*cv)),
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
                        resource_name: Some(self.rname(*r)),
                        holder: None,
                        waiting: state.sem_waiters.get(r).map(|q| q.len()).unwrap_or(0),
                        detail: "waiting for semaphore permits".into(),
                    },
                    BlockReason::Join(_) => BlockedRecord {
                        thread: t.id,
                        kind: BlockKind::Join,
                        resource: None,
                        resource_name: None,
                        holder: None,
                        waiting: 0,
                        detail: "waiting to join a child thread".into(),
                    },
                    BlockReason::Scope(_) => BlockedRecord {
                        thread: t.id,
                        kind: BlockKind::Scope,
                        resource: None,
                        resource_name: None,
                        holder: None,
                        waiting: 0,
                        detail: "waiting for scope members".into(),
                    },
                });
            }
        }
        out
    }

    pub(super) fn instances_impl(&self, state: &MachineState) -> Vec<concir_sem::system::InstanceState> {
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
            out.push(concir_sem::system::InstanceState {
                thread: t.id,
                frame: t.current_frame(),
                function,
                sid,
                status,
            });
        }
        out
    }

    pub(super) fn doom_snapshot_impl(&self, state: &MachineState) -> concir_sem::system::DoomState {
        use concir_sem::system::{DoomState, DoomThread, DoomWait};
        let mut free: Vec<String> = state
            .store
            .mutexes
            .iter()
            .filter(|(_, m)| matches!(m, MutexState::Free))
            .map(|(r, _)| self.rname(*r))
            .collect();
        for (r, rw) in &state.store.rwlocks {
            if matches!(rw, RwLockState::Free) {
                free.push(self.rname(*r));
            }
        }
        free.sort();
        let mut threads = Vec::new();
        for t in state.threads.values() {
            let mut holds: Vec<String> = state
                .store
                .mutexes
                .iter()
                .filter(|(_, m)| matches!(m, MutexState::Held(h) if *h == t.id))
                .map(|(r, _)| self.rname(*r))
                .collect();
            for (r, rw) in &state.store.rwlocks {
                let held = match rw {
                    RwLockState::Writing(h) => *h == t.id,
                    RwLockState::Reading(rs) => rs.contains(&t.id),
                    RwLockState::Free => false,
                };
                if held {
                    holds.push(self.rname(*r));
                }
            }
            holds.sort();
            let (function, at_sid) = match t.current_frame() {
                Some(fid) => {
                    let f = state.frame(fid);
                    let func = self.program.function(f.function);
                    (
                        concir_ir::fqn::fqn(self.program.module_name(func.module), &func.name),
                        func.body.get(f.pc).map(|s| s.sid.clone()),
                    )
                }
                None => (
                    concir_ir::fqn::fqn(
                        self.program.module_name(self.program.function(t.entry_function).module),
                        &self.program.function(t.entry_function).name,
                    ),
                    None,
                ),
            };
            let waiting_on = match &t.status {
                ThreadStatus::Blocked(reason) => Some(match reason {
                    BlockReason::Lock(r) => DoomWait { kind: "mutex".into(), resource: Some(self.rname(*r)) },
                    BlockReason::RwLockRead(r) => DoomWait { kind: "rwlock_read".into(), resource: Some(self.rname(*r)) },
                    BlockReason::RwLockWrite(r) => DoomWait { kind: "rwlock_write".into(), resource: Some(self.rname(*r)) },
                    BlockReason::Semaphore(r) => DoomWait { kind: "semaphore".into(), resource: Some(self.rname(*r)) },
                    BlockReason::Condvar(cv, _) => DoomWait { kind: "condvar".into(), resource: Some(self.rname(*cv)) },
                    BlockReason::ChannelSend(c) => DoomWait { kind: "channel_send".into(), resource: Some(self.rname(*c)) },
                    BlockReason::ChannelRecv(c) => DoomWait { kind: "channel_recv".into(), resource: Some(self.rname(*c)) },
                    BlockReason::Join(_) => DoomWait { kind: "join".into(), resource: None },
                    BlockReason::Scope(_) => DoomWait { kind: "scope".into(), resource: None },
                }),
                _ => None,
            };
            threads.push(DoomThread {
                thread: t.id.0 as u32,
                entry_function: concir_ir::fqn::fqn(
                    self.program.module_name(self.program.function(t.entry_function).module),
                    &self.program.function(t.entry_function).name,
                ),
                function,
                at_sid,
                holds,
                waiting_on,
            });
        }
        DoomState { threads, free_resources: free }
    }

    pub(super) fn satisfied_impl(&self, state: &MachineState, predicate: &Predicate) -> bool {
        match predicate {
            Predicate::True => true,
            Predicate::False => false,
            Predicate::VarEq { resource, value } => {
                state.store.read_shared(*resource) == Some(value)
            }
            Predicate::VarCmp {
                resource,
                op,
                value,
            } => state
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
            Predicate::MilestoneReached { sites, .. } => {
                sites.iter().any(|&(func, sid)| state.reached.contains(&(func, sid)))
            }
            Predicate::MutexFree(r) => {
                matches!(state.store.mutexes.get(r), Some(MutexState::Free))
                    || matches!(state.store.rwlocks.get(r), Some(RwLockState::Free))
            }
            Predicate::MutexHeld(r) => {
                matches!(state.store.mutexes.get(r), Some(MutexState::Held(_)))
                    || matches!(
                        state.store.rwlocks.get(r),
                        Some(RwLockState::Reading(_) | RwLockState::Writing(_))
                    )
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
            Predicate::HoldsAll { func, resources } => state.threads.values().any(|t| {
                let executes = t.stack.iter().any(|fid| {
                    state
                        .store
                        .frames
                        .get(fid)
                        .map(|f| f.function == *func)
                        .unwrap_or(false)
                });
                executes && resources.iter().all(|r| self.thread_holds(state, t.id, *r))
            }),
            Predicate::MutexExclusive(r) => match self.program.resource(*r).kind {
                concir_sem::program::ResKind::Mutex | concir_sem::program::ResKind::RwLock => true,
                concir_sem::program::ResKind::Semaphore => {
                    state.store.semaphores.get(r).copied().unwrap_or(0) >= 0
                }
                _ => false,
            },
            Predicate::NeverHoldsAll { func, resources } => !state.threads.values().any(|t| {
                let executes = t.stack.iter().any(|fid| {
                    state
                        .store
                        .frames
                        .get(fid)
                        .map(|f| f.function == *func)
                        .unwrap_or(false)
                });
                executes && resources.iter().all(|r| self.thread_holds(state, t.id, *r))
            }),
            Predicate::Not(p) => !self.satisfied(state, p),
            Predicate::And(ps) => ps.iter().all(|p| self.satisfied(state, p)),
            Predicate::Or(ps) => ps.iter().any(|p| self.satisfied(state, p)),
        }
    }

}
