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


use super::state_ops::hln_to_val;
use super::UnipnEngine;

impl<'a> UnipnEngine<'a> {
    pub(super) fn blocked_impl(&self, state: &HlnState) -> Vec<BlockedRecord> {
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

    pub(super) fn instances_impl(&self, state: &HlnState) -> Vec<InstanceState> {
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

    pub(super) fn doom_snapshot_impl(&self, state: &HlnState) -> DoomState {
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

    pub(super) fn satisfied_impl(&self, state: &HlnState, predicate: &Predicate) -> bool {
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

}
