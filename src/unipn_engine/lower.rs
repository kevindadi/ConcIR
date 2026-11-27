//! Lowering a resolved [`SemProgram`] into a UniPN [`HlnNet`].
//!
//! Design invariants:
//! - **Single subnet per function:** every `SemFunction` gets one `Control` chain
//!   plus a single `FunctionEnd`; runtime thread tokens carry their identity inside
//!   [`unipn::hln::ControlToken`].
//! - **Zero `Condvar` resource places:** `CondvarWait` allocates ordinary
//!   `Control(BasicBlock)` places (`Wait` and `Reacquire`), and `CondvarNotifyAll`
//!   uses first-class [`ArcDir::Broadcast`] edges directly from `Wait` to `Reacquire`.
//! - **True-concurrency metadata:** every statement transition carries sorted
//!   `reads_vars` and `writes_vars` sets for logic-driven data-race checks.

use std::collections::{BTreeSet, HashMap};

use unipn::hln::{HlnArcKind, HlnBuilder, HlnNet, HlnTransition, HlnTransitionKind, HlnVal};
use unipn::net::{ArcDir, ControlSub, PlaceId, TransitionId};

use crate::fqn;
use crate::sem::eval::LExpr;
use crate::sem::ids::{FunctionId, ResourceId, SlotRef};
use crate::sem::program::{ResKind, SemOp, SemProgram, SemStmt};
use crate::sem::value::default_value;

use super::system::val_to_hln;

/// Canonical structural key identifying each place in a [`LoweredHlnNet`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlaceKey {
    Control(FunctionId, usize),
    Contend(FunctionId, usize),
    Wait(FunctionId, usize),
    Reacquire(FunctionId, usize),
    FunctionEnd(FunctionId),
    Var(ResourceId),
    Atomic(ResourceId),
    Mutex(ResourceId),
    RwLock(ResourceId),
    Semaphore(ResourceId),
    Channel(ResourceId),
}

/// Static UniPN HLN representation plus index tables for fast [`TransitionSystem`] stepping.
#[derive(Debug, Clone)]
pub struct LoweredHlnNet {
    pub net: HlnNet,
    pub place_of: HashMap<PlaceKey, PlaceId>,
    pub place_keys: Vec<PlaceKey>,
    pub func_ends: Vec<PlaceId>,
    pub func_lens: Vec<usize>,
    /// For each condvar resource, all `(wait_place, reacquire_place)` pairs.
    pub condvar_waits: HashMap<ResourceId, Vec<(PlaceId, PlaceId)>>,
    pub wait_to_reacquire: HashMap<PlaceId, PlaceId>,
    pub stmt_transitions: HashMap<(FunctionId, usize), Vec<TransitionId>>,
}

impl LoweredHlnNet {
    pub fn pc_place(&self, func: FunctionId, pc: usize) -> Option<PlaceId> {
        let len = self.func_lens.get(func.0 as usize).copied().unwrap_or(0);
        if pc < len {
            self.place_of.get(&PlaceKey::Control(func, pc)).copied()
        } else if pc == len {
            self.func_ends.get(func.0 as usize).copied()
        } else {
            None
        }
    }

    pub fn place_key(&self, pid: PlaceId) -> Option<&PlaceKey> {
        self.place_keys.get(pid.index())
    }
}

/// Build an [`HlnNet`] (strictly one subnet per function, zero `Condvar` places)
/// from a resolved [`SemProgram`].
pub fn build(sem: &SemProgram) -> LoweredHlnNet {
    let mut builder = HlnBuilder::new();
    let mut place_of: HashMap<PlaceKey, PlaceId> = HashMap::new();
    let mut place_keys: Vec<PlaceKey> = Vec::new();
    let mut func_ends: Vec<PlaceId> = Vec::with_capacity(sem.functions().len());
    let mut func_lens: Vec<usize> = Vec::with_capacity(sem.functions().len());
    let mut condvar_waits: HashMap<ResourceId, Vec<(PlaceId, PlaceId)>> = HashMap::new();
    let mut wait_to_reacquire: HashMap<PlaceId, PlaceId> = HashMap::new();
    let mut stmt_transitions: HashMap<(FunctionId, usize), Vec<TransitionId>> = HashMap::new();

    let mut reg_place = |key: PlaceKey, pid: PlaceId| {
        place_of.insert(key.clone(), pid);
        debug_assert_eq!(pid.index(), place_keys.len());
        place_keys.push(key);
        pid
    };

    // 1. Control subnets (strictly one subnet per function; zero per-thread copies).
    for f in sem.functions() {
        let fname = fqn::fqn(sem.module_name(f.module), &f.name);
        func_lens.push(f.body.len());
        for (sid_idx, stmt) in f.body.iter().enumerate() {
            let sub = if sid_idx == 0 {
                ControlSub::FunctionStart
            } else {
                ControlSub::BasicBlock
            };
            let ctrl_pid = builder.add_control_place(format!("{fname}@{}", stmt.sid), sub);
            reg_place(PlaceKey::Control(f.id, sid_idx), ctrl_pid);

            match &stmt.op {
                SemOp::MutexLock { .. }
                | SemOp::RwLockRead { .. }
                | SemOp::RwLockWrite { .. }
                | SemOp::SemaphoreAcquire { .. }
                | SemOp::ChannelSend { .. }
                | SemOp::ChannelRecv { .. }
                | SemOp::Join { .. }
                | SemOp::Scope { .. } => {
                    let contend_pid = builder.add_control_place(
                        format!("{fname}@{}:contend", stmt.sid),
                        ControlSub::BasicBlock,
                    );
                    reg_place(PlaceKey::Contend(f.id, sid_idx), contend_pid);
                }
                SemOp::CondvarWait { condvar, .. } => {
                    let wait_pid = builder.add_control_place(
                        format!("{fname}@{}:wait", stmt.sid),
                        ControlSub::BasicBlock,
                    );
                    let reacquire_pid = builder.add_control_place(
                        format!("{fname}@{}:reacquire", stmt.sid),
                        ControlSub::BasicBlock,
                    );
                    reg_place(PlaceKey::Wait(f.id, sid_idx), wait_pid);
                    reg_place(PlaceKey::Reacquire(f.id, sid_idx), reacquire_pid);
                    condvar_waits
                        .entry(*condvar)
                        .or_default()
                        .push((wait_pid, reacquire_pid));
                    wait_to_reacquire.insert(wait_pid, reacquire_pid);
                }
                _ => {}
            }
        }
        let end_pid = builder.add_control_place(format!("{fname}@end"), ControlSub::FunctionEnd);
        reg_place(PlaceKey::FunctionEnd(f.id), end_pid);
        func_ends.push(end_pid);
    }

    // 2. Resource places (no Condvar resource places).
    for r in sem.resources() {
        let rname = fqn::fqn(sem.module_name(r.module), &r.name);
        match r.kind {
            ResKind::Mutex => {
                let pid = builder.add_mutex(r.id.0 as usize, rname);
                reg_place(PlaceKey::Mutex(r.id), pid);
            }
            ResKind::RwLock => {
                let pid = builder.add_rwlock(r.id.0 as usize, rname, None);
                reg_place(PlaceKey::RwLock(r.id), pid);
            }
            ResKind::Semaphore => {
                let pid = builder.add_semaphore(r.id.0 as usize, rname, r.permits);
                reg_place(PlaceKey::Semaphore(r.id), pid);
            }
            ResKind::Channel => {
                let pid = builder.add_channel(r.id.0 as usize, rname, r.capacity);
                reg_place(PlaceKey::Channel(r.id), pid);
            }
            ResKind::Var => {
                let init = init_hln_val(r);
                let pid = builder.add_var(r.id.0 as usize, rname, init);
                reg_place(PlaceKey::Var(r.id), pid);
            }
            ResKind::Atomic => {
                let init = init_hln_val(r);
                let pid = builder.add_atomic(r.id.0 as usize, rname, init);
                reg_place(PlaceKey::Atomic(r.id), pid);
            }
            ResKind::Condvar => {
                // Intentionally zero resource places: Condvar waits live in
                // ordinary `Wait` / `Reacquire` control places only.
            }
        }
    }

    let pc_pid = |func: FunctionId, pc: usize| -> PlaceId {
        place_of
            .get(&PlaceKey::Control(func, pc))
            .copied()
            .unwrap_or(func_ends[func.0 as usize])
    };

    // 3. Function transitions & Broadcast arcs.
    for f in sem.functions() {
        let fname = fqn::fqn(sem.module_name(f.module), &f.name);
        for (sid_idx, stmt) in f.body.iter().enumerate() {
            let src_pid = pc_pid(f.id, sid_idx);
            let next_pid = pc_pid(f.id, sid_idx + 1);
            let (reads_vars, writes_vars) = collect_stmt_rw_vars(sem, stmt);
            let tr_meta = |kind: HlnTransitionKind| {
                let mut tr = HlnTransition::new(kind);
                tr.scope = Some(fname.clone());
                tr.func_id = Some(f.id.0 as usize);
                tr.sid_idx = Some(sid_idx);
                tr.sid = Some(stmt.sid.clone());
                tr.milestone = stmt.milestone.clone();
                tr.reads_vars = reads_vars.clone();
                tr.writes_vars = writes_vars.clone();
                tr
            };

            let mut tids = Vec::new();
            match &stmt.op {
                SemOp::Nop | SemOp::Unsupported { .. } => {
                    let tid = builder.add_transition(
                        format!("{fname}@{}:nop", stmt.sid),
                        tr_meta(HlnTransitionKind::Sequential),
                    );
                    builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.push(tid);
                }
                SemOp::AssignLocal { target, .. } => {
                    let kind = match target {
                        SlotRef::Shared(_) => HlnTransitionKind::VarWrite,
                        _ => HlnTransitionKind::Sequential,
                    };
                    let tid =
                        builder.add_transition(format!("{fname}@{}", stmt.sid), tr_meta(kind));
                    builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.push(tid);
                }
                SemOp::ReadShared { resource, .. } => {
                    let mut meta = tr_meta(HlnTransitionKind::VarRead);
                    attach_resource(sem, &mut meta, *resource);
                    let tid = builder.add_transition(format!("{fname}@{}", stmt.sid), meta);
                    builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.push(tid);
                }
                SemOp::WriteShared { resource, .. } => {
                    let mut meta = tr_meta(HlnTransitionKind::VarWrite);
                    attach_resource(sem, &mut meta, *resource);
                    let tid = builder.add_transition(format!("{fname}@{}", stmt.sid), meta);
                    builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.push(tid);
                }
                SemOp::AtomicLoad { resource, .. } => {
                    let mut meta = tr_meta(HlnTransitionKind::AtomicLoad);
                    attach_resource(sem, &mut meta, *resource);
                    let tid = builder.add_transition(format!("{fname}@{}", stmt.sid), meta);
                    builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.push(tid);
                }
                SemOp::AtomicStore { resource, .. } => {
                    let mut meta = tr_meta(HlnTransitionKind::AtomicStore);
                    attach_resource(sem, &mut meta, *resource);
                    let tid = builder.add_transition(format!("{fname}@{}", stmt.sid), meta);
                    builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.push(tid);
                }
                SemOp::AtomicCas { resource, .. } => {
                    let mut meta = tr_meta(HlnTransitionKind::AtomicCas);
                    attach_resource(sem, &mut meta, *resource);
                    let tid = builder.add_transition(format!("{fname}@{}", stmt.sid), meta);
                    builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.push(tid);
                }
                SemOp::MutexLock { resource } => {
                    let m_pid = place_of[&PlaceKey::Mutex(*resource)];
                    let c_pid = place_of[&PlaceKey::Contend(f.id, sid_idx)];
                    let mut meta_fast = tr_meta(HlnTransitionKind::Lock);
                    attach_resource(sem, &mut meta_fast, *resource);
                    let t_fast = builder
                        .add_transition(format!("{fname}@{}:lock", stmt.sid), meta_fast);
                    builder.add_arc(src_pid, t_fast, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(m_pid, t_fast, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, t_fast, ArcDir::Output, 1, HlnArcKind::Plain);

                    let mut meta_wait = tr_meta(HlnTransitionKind::LockContend);
                    attach_resource(sem, &mut meta_wait, *resource);
                    let t_wait = builder
                        .add_transition(format!("{fname}@{}:contend", stmt.sid), meta_wait);
                    builder.add_arc(src_pid, t_wait, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(c_pid, t_wait, ArcDir::Output, 1, HlnArcKind::Plain);

                    let mut meta_res = tr_meta(HlnTransitionKind::Lock);
                    attach_resource(sem, &mut meta_res, *resource);
                    let t_res = builder
                        .add_transition(format!("{fname}@{}:resume", stmt.sid), meta_res);
                    builder.add_arc(c_pid, t_res, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(m_pid, t_res, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, t_res, ArcDir::Output, 1, HlnArcKind::Plain);

                    tids.extend([t_fast, t_wait, t_res]);
                }
                SemOp::MutexUnlock { resource } => {
                    let m_pid = place_of[&PlaceKey::Mutex(*resource)];
                    let mut meta = tr_meta(HlnTransitionKind::Unlock);
                    attach_resource(sem, &mut meta, *resource);
                    let tid = builder.add_transition(format!("{fname}@{}:unlock", stmt.sid), meta);
                    builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(m_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.push(tid);
                }
                SemOp::RwLockRead { resource } => {
                    let c_pid = place_of[&PlaceKey::Contend(f.id, sid_idx)];
                    let mut m1 = tr_meta(HlnTransitionKind::ReadLock);
                    attach_resource(sem, &mut m1, *resource);
                    let t_fast = builder.add_transition(format!("{fname}@{}:rlock", stmt.sid), m1);
                    builder.add_arc(src_pid, t_fast, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, t_fast, ArcDir::Output, 1, HlnArcKind::Plain);

                    let mut m2 = tr_meta(HlnTransitionKind::ReadLockContend);
                    attach_resource(sem, &mut m2, *resource);
                    let t_wait =
                        builder.add_transition(format!("{fname}@{}:rcontend", stmt.sid), m2);
                    builder.add_arc(src_pid, t_wait, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(c_pid, t_wait, ArcDir::Output, 1, HlnArcKind::Plain);

                    let mut m3 = tr_meta(HlnTransitionKind::ReadLock);
                    attach_resource(sem, &mut m3, *resource);
                    let t_res = builder.add_transition(format!("{fname}@{}:rresume", stmt.sid), m3);
                    builder.add_arc(c_pid, t_res, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, t_res, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.extend([t_fast, t_wait, t_res]);
                }
                SemOp::RwLockWrite { resource } => {
                    let c_pid = place_of[&PlaceKey::Contend(f.id, sid_idx)];
                    let mut m1 = tr_meta(HlnTransitionKind::WriteLock);
                    attach_resource(sem, &mut m1, *resource);
                    let t_fast = builder.add_transition(format!("{fname}@{}:wlock", stmt.sid), m1);
                    builder.add_arc(src_pid, t_fast, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, t_fast, ArcDir::Output, 1, HlnArcKind::Plain);

                    let mut m2 = tr_meta(HlnTransitionKind::WriteLockContend);
                    attach_resource(sem, &mut m2, *resource);
                    let t_wait =
                        builder.add_transition(format!("{fname}@{}:wcontend", stmt.sid), m2);
                    builder.add_arc(src_pid, t_wait, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(c_pid, t_wait, ArcDir::Output, 1, HlnArcKind::Plain);

                    let mut m3 = tr_meta(HlnTransitionKind::WriteLock);
                    attach_resource(sem, &mut m3, *resource);
                    let t_res = builder.add_transition(format!("{fname}@{}:wresume", stmt.sid), m3);
                    builder.add_arc(c_pid, t_res, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, t_res, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.extend([t_fast, t_wait, t_res]);
                }
                SemOp::RwLockUnlock { resource } => {
                    let mut meta = tr_meta(HlnTransitionKind::ReadUnlock);
                    attach_resource(sem, &mut meta, *resource);
                    let tid =
                        builder.add_transition(format!("{fname}@{}:rwunlock", stmt.sid), meta);
                    builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.push(tid);
                }
                SemOp::CondvarWait { condvar, lock } => {
                    let wait_pid = place_of[&PlaceKey::Wait(f.id, sid_idx)];
                    let reacq_pid = place_of[&PlaceKey::Reacquire(f.id, sid_idx)];
                    let m_pid = place_of[&PlaceKey::Mutex(*lock)];

                    let mut m_enter = tr_meta(HlnTransitionKind::CondvarWaitEnter);
                    attach_resource(sem, &mut m_enter, *lock);
                    m_enter.condvar_id = Some(condvar.0 as usize);
                    let t_enter =
                        builder.add_transition(format!("{fname}@{}:wait_enter", stmt.sid), m_enter);
                    builder.add_arc(src_pid, t_enter, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(wait_pid, t_enter, ArcDir::Output, 1, HlnArcKind::Plain);
                    builder.add_arc(m_pid, t_enter, ArcDir::Output, 1, HlnArcKind::Plain);

                    let mut m_reacq = tr_meta(HlnTransitionKind::CondvarReacquire);
                    attach_resource(sem, &mut m_reacq, *lock);
                    m_reacq.condvar_id = Some(condvar.0 as usize);
                    let t_reacq =
                        builder.add_transition(format!("{fname}@{}:reacquire", stmt.sid), m_reacq);
                    builder.add_arc(reacq_pid, t_reacq, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(m_pid, t_reacq, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, t_reacq, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.extend([t_enter, t_reacq]);
                }
                SemOp::CondvarNotify { condvar } => {
                    let mut meta = tr_meta(HlnTransitionKind::CondvarNotify);
                    meta.condvar_id = Some(condvar.0 as usize);
                    attach_resource(sem, &mut meta, *condvar);
                    let tid = builder.add_transition(format!("{fname}@{}:notify", stmt.sid), meta);
                    builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.push(tid);
                }
                SemOp::CondvarNotifyAll { condvar } => {
                    let mut meta = tr_meta(HlnTransitionKind::CondvarNotifyAll);
                    meta.condvar_id = Some(condvar.0 as usize);
                    attach_resource(sem, &mut meta, *condvar);
                    let tid =
                        builder.add_transition(format!("{fname}@{}:notify_all", stmt.sid), meta);
                    builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    // First-class Broadcast arcs move 0..N waiting threads directly
                    // from each plain Wait control place to its Reacquire control place.
                    if let Some(pairs) = condvar_waits.get(condvar) {
                        for &(wait_pid, reacq_pid) in pairs {
                            builder.add_arc(
                                wait_pid,
                                tid,
                                ArcDir::Broadcast { to: reacq_pid },
                                1,
                                HlnArcKind::Plain,
                            );
                        }
                    }
                    tids.push(tid);
                }
                SemOp::SemaphoreAcquire { resource, .. } => {
                    let c_pid = place_of[&PlaceKey::Contend(f.id, sid_idx)];
                    let mut m1 = tr_meta(HlnTransitionKind::Acquire);
                    attach_resource(sem, &mut m1, *resource);
                    let t_fast = builder.add_transition(format!("{fname}@{}:acq", stmt.sid), m1);
                    builder.add_arc(src_pid, t_fast, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, t_fast, ArcDir::Output, 1, HlnArcKind::Plain);

                    let mut m2 = tr_meta(HlnTransitionKind::AcquireContend);
                    attach_resource(sem, &mut m2, *resource);
                    let t_wait =
                        builder.add_transition(format!("{fname}@{}:acq_contend", stmt.sid), m2);
                    builder.add_arc(src_pid, t_wait, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(c_pid, t_wait, ArcDir::Output, 1, HlnArcKind::Plain);

                    let mut m3 = tr_meta(HlnTransitionKind::Acquire);
                    attach_resource(sem, &mut m3, *resource);
                    let t_res =
                        builder.add_transition(format!("{fname}@{}:acq_resume", stmt.sid), m3);
                    builder.add_arc(c_pid, t_res, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, t_res, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.extend([t_fast, t_wait, t_res]);
                }
                SemOp::SemaphoreRelease { resource, .. } => {
                    let mut meta = tr_meta(HlnTransitionKind::Release);
                    attach_resource(sem, &mut meta, *resource);
                    let tid = builder.add_transition(format!("{fname}@{}:rel", stmt.sid), meta);
                    builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.push(tid);
                }
                SemOp::ChannelSend { channel, .. } => {
                    let c_pid = place_of[&PlaceKey::Contend(f.id, sid_idx)];
                    let mut m1 = tr_meta(HlnTransitionKind::Send);
                    attach_resource(sem, &mut m1, *channel);
                    let t_fast = builder.add_transition(format!("{fname}@{}:send", stmt.sid), m1);
                    builder.add_arc(src_pid, t_fast, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, t_fast, ArcDir::Output, 1, HlnArcKind::Plain);

                    let mut m2 = tr_meta(HlnTransitionKind::SendContend);
                    attach_resource(sem, &mut m2, *channel);
                    let t_wait =
                        builder.add_transition(format!("{fname}@{}:send_wait", stmt.sid), m2);
                    builder.add_arc(src_pid, t_wait, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(c_pid, t_wait, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.extend([t_fast, t_wait]);
                }
                SemOp::ChannelRecv { channel, .. } => {
                    let c_pid = place_of[&PlaceKey::Contend(f.id, sid_idx)];
                    let mut m1 = tr_meta(HlnTransitionKind::Recv);
                    attach_resource(sem, &mut m1, *channel);
                    let t_fast = builder.add_transition(format!("{fname}@{}:recv", stmt.sid), m1);
                    builder.add_arc(src_pid, t_fast, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, t_fast, ArcDir::Output, 1, HlnArcKind::Plain);

                    let mut m2 = tr_meta(HlnTransitionKind::RecvContend);
                    attach_resource(sem, &mut m2, *channel);
                    let t_wait =
                        builder.add_transition(format!("{fname}@{}:recv_wait", stmt.sid), m2);
                    builder.add_arc(src_pid, t_wait, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(c_pid, t_wait, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.extend([t_fast, t_wait]);
                }
                SemOp::Call { func, .. } => {
                    let tid = builder.add_transition(
                        format!("{fname}@{}:call", stmt.sid),
                        tr_meta(HlnTransitionKind::Call),
                    );
                    builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                    let target_pid = pc_pid(*func, 0);
                    builder.add_arc(target_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.push(tid);
                }
                SemOp::Spawn { func, .. } => {
                    let tid = builder.add_transition(
                        format!("{fname}@{}:spawn", stmt.sid),
                        tr_meta(HlnTransitionKind::Spawn),
                    );
                    builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    let target_pid = pc_pid(*func, 0);
                    builder.add_arc(target_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.push(tid);
                }
                SemOp::Scope { funcs } => {
                    let c_pid = place_of[&PlaceKey::Contend(f.id, sid_idx)];
                    let t_start = builder.add_transition(
                        format!("{fname}@{}:scope", stmt.sid),
                        tr_meta(HlnTransitionKind::Scope),
                    );
                    builder.add_arc(src_pid, t_start, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(c_pid, t_start, ArcDir::Output, 1, HlnArcKind::Plain);
                    for &cf in funcs {
                        let c_start = pc_pid(cf, 0);
                        builder.add_arc(c_start, t_start, ArcDir::Output, 1, HlnArcKind::Plain);
                    }
                    let t_done = builder.add_transition(
                        format!("{fname}@{}:scope_done", stmt.sid),
                        tr_meta(HlnTransitionKind::Scope),
                    );
                    builder.add_arc(c_pid, t_done, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, t_done, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.extend([t_start, t_done]);
                }
                SemOp::Join { .. } => {
                    let c_pid = place_of[&PlaceKey::Contend(f.id, sid_idx)];
                    let t_fast = builder.add_transition(
                        format!("{fname}@{}:join", stmt.sid),
                        tr_meta(HlnTransitionKind::Join),
                    );
                    builder.add_arc(src_pid, t_fast, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, t_fast, ArcDir::Output, 1, HlnArcKind::Plain);

                    let t_wait = builder.add_transition(
                        format!("{fname}@{}:join_wait", stmt.sid),
                        tr_meta(HlnTransitionKind::Join),
                    );
                    builder.add_arc(src_pid, t_wait, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(c_pid, t_wait, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.extend([t_fast, t_wait]);
                }
                SemOp::Goto { target } => {
                    let dst = pc_pid(f.id, *target);
                    let tid = builder.add_transition(
                        format!("{fname}@{}:goto", stmt.sid),
                        tr_meta(HlnTransitionKind::Goto),
                    );
                    builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(dst, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.push(tid);
                }
                SemOp::Branch {
                    then, else_target, ..
                } => {
                    let t_then = builder.add_transition(
                        format!("{fname}@{}:then", stmt.sid),
                        tr_meta(HlnTransitionKind::BranchTrue),
                    );
                    builder.add_arc(src_pid, t_then, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(
                        pc_pid(f.id, *then),
                        t_then,
                        ArcDir::Output,
                        1,
                        HlnArcKind::Plain,
                    );

                    let t_else = builder.add_transition(
                        format!("{fname}@{}:else", stmt.sid),
                        tr_meta(HlnTransitionKind::BranchFalse),
                    );
                    builder.add_arc(src_pid, t_else, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(
                        pc_pid(f.id, *else_target),
                        t_else,
                        ArcDir::Output,
                        1,
                        HlnArcKind::Plain,
                    );
                    tids.extend([t_then, t_else]);
                }
                SemOp::Switch { cases, default, .. } => {
                    for (label, &target) in cases {
                        let tid = builder.add_transition(
                            format!("{fname}@{}:case_{label}", stmt.sid),
                            tr_meta(HlnTransitionKind::Switch {
                                label: label.clone(),
                            }),
                        );
                        builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                        builder.add_arc(
                            pc_pid(f.id, target),
                            tid,
                            ArcDir::Output,
                            1,
                            HlnArcKind::Plain,
                        );
                        tids.push(tid);
                    }
                    let t_def = builder.add_transition(
                        format!("{fname}@{}:default", stmt.sid),
                        tr_meta(HlnTransitionKind::Switch {
                            label: "default".into(),
                        }),
                    );
                    builder.add_arc(src_pid, t_def, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(
                        pc_pid(f.id, *default),
                        t_def,
                        ArcDir::Output,
                        1,
                        HlnArcKind::Plain,
                    );
                    tids.push(t_def);
                }
                SemOp::Return { .. } => {
                    let end_pid = func_ends[f.id.0 as usize];
                    let tid = builder.add_transition(
                        format!("{fname}@{}:return", stmt.sid),
                        tr_meta(HlnTransitionKind::Return),
                    );
                    builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(end_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.push(tid);
                }
                SemOp::SeqHole { .. } => {
                    let tid = builder.add_transition(
                        format!("{fname}@{}:seq_hole", stmt.sid),
                        tr_meta(HlnTransitionKind::SeqHole),
                    );
                    builder.add_arc(src_pid, tid, ArcDir::Input, 1, HlnArcKind::Plain);
                    builder.add_arc(next_pid, tid, ArcDir::Output, 1, HlnArcKind::Plain);
                    tids.push(tid);
                }
            }
            stmt_transitions.insert((f.id, sid_idx), tids);
        }
    }

    LoweredHlnNet {
        net: builder.build().0,
        place_of,
        place_keys,
        func_ends,
        func_lens,
        condvar_waits,
        wait_to_reacquire,
        stmt_transitions,
    }
}

fn init_hln_val(r: &crate::sem::program::SemResource) -> HlnVal {
    if let Some(v) = &r.init {
        val_to_hln(v)
    } else if let Some(ty) = &r.ty {
        val_to_hln(&default_value(ty))
    } else {
        HlnVal::Int(0)
    }
}

fn attach_resource(sem: &SemProgram, meta: &mut HlnTransition, res_id: ResourceId) {
    let r = sem.resource(res_id);
    meta.resource_id = Some(res_id.0 as usize);
    meta.resource_name = Some(fqn::fqn(sem.module_name(r.module), &r.name));
}

fn collect_expr_var_reads(sem: &SemProgram, expr: &LExpr, reads: &mut BTreeSet<usize>) {
    match expr {
        LExpr::Lit(_) | LExpr::Slot(SlotRef::Discard | SlotRef::Local(_)) => {}
        LExpr::Slot(SlotRef::Shared(r)) => {
            if sem.resource(*r).kind == ResKind::Var {
                reads.insert(r.0 as usize);
            }
        }
        LExpr::Field { base, .. } | LExpr::Neg(base) => collect_expr_var_reads(sem, base, reads),
        LExpr::Bin { lhs, rhs, .. } | LExpr::Cmp { lhs, rhs, .. } => {
            collect_expr_var_reads(sem, lhs, reads);
            collect_expr_var_reads(sem, rhs, reads);
        }
        LExpr::Struct { fields } => {
            for (_, e) in fields {
                collect_expr_var_reads(sem, e, reads);
            }
        }
    }
}

pub(crate) fn stmt_rw_vars(sem: &SemProgram, stmt: &SemStmt) -> (Vec<usize>, Vec<usize>) {
    collect_stmt_rw_vars(sem, stmt)
}

fn collect_stmt_rw_vars(sem: &SemProgram, stmt: &SemStmt) -> (Vec<usize>, Vec<usize>) {
    let mut reads = BTreeSet::new();
    let mut writes = BTreeSet::new();
    match &stmt.op {
        SemOp::AssignLocal { target, expr } => {
            if let SlotRef::Shared(r) = target
                && sem.resource(*r).kind == ResKind::Var
            {
                writes.insert(r.0 as usize);
            }
            collect_expr_var_reads(sem, expr, &mut reads);
        }
        SemOp::ReadShared { resource, dst } => {
            if sem.resource(*resource).kind == ResKind::Var {
                reads.insert(resource.0 as usize);
            }
            if let Some(SlotRef::Shared(r)) = dst
                && sem.resource(*r).kind == ResKind::Var
            {
                writes.insert(r.0 as usize);
            }
        }
        SemOp::WriteShared { resource, expr } => {
            if sem.resource(*resource).kind == ResKind::Var {
                writes.insert(resource.0 as usize);
            }
            collect_expr_var_reads(sem, expr, &mut reads);
        }
        SemOp::AtomicLoad { dst, .. } => {
            if let SlotRef::Shared(r) = dst
                && sem.resource(*r).kind == ResKind::Var
            {
                writes.insert(r.0 as usize);
            }
        }
        SemOp::AtomicStore { value, .. } => {
            collect_expr_var_reads(sem, value, &mut reads);
        }
        SemOp::AtomicCas {
            expected,
            desired,
            dst,
            ..
        } => {
            collect_expr_var_reads(sem, expected, &mut reads);
            collect_expr_var_reads(sem, desired, &mut reads);
            if let SlotRef::Shared(r) = dst
                && sem.resource(*r).kind == ResKind::Var
            {
                writes.insert(r.0 as usize);
            }
        }
        SemOp::ChannelSend { value, .. } => {
            collect_expr_var_reads(sem, value, &mut reads);
        }
        SemOp::ChannelRecv { dst, .. } => {
            if let SlotRef::Shared(r) = dst
                && sem.resource(*r).kind == ResKind::Var
            {
                writes.insert(r.0 as usize);
            }
        }
        SemOp::Call { args, .. } => {
            for a in args {
                collect_expr_var_reads(sem, a, &mut reads);
            }
        }
        SemOp::Branch { cond, .. } => collect_expr_var_reads(sem, cond, &mut reads),
        SemOp::Switch { var, .. } => collect_expr_var_reads(sem, var, &mut reads),
        SemOp::Return { value: Some(v) } => collect_expr_var_reads(sem, v, &mut reads),
        SemOp::SeqHole { updates_modeled, .. } => {
            for u in updates_modeled {
                if let SlotRef::Shared(r) = u.target
                    && sem.resource(r).kind == ResKind::Var
                {
                    writes.insert(r.0 as usize);
                }
                collect_expr_var_reads(sem, &u.expr, &mut reads);
            }
        }
        _ => {}
    }
    (reads.into_iter().collect(), writes.into_iter().collect())
}
