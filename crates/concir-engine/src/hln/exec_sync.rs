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


use super::state_ops::{hln_to_val, val_to_hln};
use super::UnipnEngine;

impl<'a> UnipnEngine<'a> {
    pub(super) fn deliver_to_recv(
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

    #[allow(clippy::too_many_arguments)]
    pub(super) fn step_sync_op(
        &self,
        next: &mut HlnState,
        tok: &ControlToken,
        frame: &HlnFrame,
        func: FunctionId,
        pc: usize,
        stmt: &concir_sem::program::SemStmt,
        at: &str,
        label: StepLabel,
    ) -> BackendResult<Vec<Step<HlnState>>> {
        let mut next = next.clone();
        match &stmt.op {
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
                        .at(at))
                    }
                    None => {
                        return Err(BackendError::invalid(
                            "E510",
                            format!("thread {} unlocked free Mutex {resource}", tok.tid),
                        )
                        .at(at))
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
                        .at(at))
                    }
                }
            }
            SemOp::ChannelSend { channel, value } => {
                let v = self.eval(&next, frame, value, at)?;
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
                        .at(at))
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
                        BackendError::invalid("E904", "semaphore count must be positive").at(at),
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
                        BackendError::invalid("E904", "semaphore count must be positive").at(at),
                    );
                }
                let rid = resource.0 as usize;
                let available = *next.semaphores.get(&rid).unwrap_or(&0);
                let updated = available.checked_add(*count).ok_or_else(|| {
                    BackendError::invalid(
                        "E905",
                        format!("semaphore '{resource}' permit count overflows i64"),
                    )
                    .at(at)
                })?;
                next.semaphores.insert(rid, updated);
                self.sync_sem_marking(&mut next, rid);
                self.advance_token(&mut next, tok.clone(), pc + 1)?;
            }
            _ => {}
        }
        Ok(vec![Step { label, state: next }])
    }
    pub(super) fn step_waiting_token(
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

}
