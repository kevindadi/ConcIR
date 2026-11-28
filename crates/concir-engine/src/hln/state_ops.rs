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


use super::UnipnEngine;

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

impl<'a> UnipnEngine<'a> {
    pub(super) fn eval(
        &self,
        state: &HlnState,
        frame: &HlnFrame,
        expr: &LExpr,
        at: &str,
    ) -> BackendResult<Value> {
        let view = HlnValueView::new(frame, state);
        eval(expr, &view, at)
    }

    pub(super) fn record_completion(&self, state: &mut HlnState, function: FunctionId) {
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

    pub(super) fn step_label(
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

    pub(super) fn rname(&self, r: ResourceId) -> String {
        let res = self.program.resource(r);
        fqn::fqn(self.program.module_name(res.module), &res.name)
    }

    pub(super) fn dst_type_ok(&self, func: FunctionId, dst: SlotRef, value: &Value) -> bool {
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

    pub(super) fn write_dst_raw(state: &mut HlnState, tid: u32, dst: SlotRef, value: Value) {
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

    pub(super) fn try_write_dst(
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

    pub(super) fn move_thread_token(
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

    pub(super) fn advance_token(
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

    pub(super) fn park_token(&self, state: &mut HlnState, mut tok: ControlToken, pid: PlaceId, tag: WaitTag) {
        tok.wait_tag = Some(tag);
        self.move_thread_token(state, tok.tid, pid, tok);
    }

    pub(super) fn sync_mutex_marking(&self, state: &mut HlnState, res_id: usize) {
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

    pub(super) fn sync_rwlock_marking(&self, state: &mut HlnState, res_id: usize) {
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

    pub(super) fn sync_sem_marking(&self, state: &mut HlnState, res_id: usize) {
        if let Some(&pid) = self
            .net
            .place_of
            .get(&PlaceKey::Semaphore(ResourceId(res_id as u32)))
        {
            let p = state.semaphores.get(&res_id).copied().unwrap_or(0);
            state.marking.set(pid, p.max(0) as usize);
        }
    }

    pub(super) fn sync_chan_marking(&self, state: &mut HlnState, res_id: usize) {
        if let Some(&pid) = self
            .net
            .place_of
            .get(&PlaceKey::Channel(ResourceId(res_id as u32)))
        {
            let len = state.channels.get(&res_id).map_or(0, VecDeque::len);
            state.marking.set(pid, len);
        }
    }

    pub(super) fn alloc_frame(
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

    pub(super) fn live_thread_count(&self, state: &HlnState) -> usize {
        state
            .all_tokens()
            .filter(|(_, tok)| !state.finished.contains(&tok.tid))
            .count()
    }

    pub(super) fn within_function_bound(&self, state: &HlnState, func: FunctionId) -> bool {
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

    pub(super) fn thread_holds(&self, state: &HlnState, tok: &ControlToken, r: ResourceId) -> bool {
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

pub(super) fn ret_dst(ret: &Option<HlnRetAddr>) -> SlotRef {
    match ret {
        Some(r) => match (r.dst_local, r.dst_shared) {
            (Some(l), _) => SlotRef::Local(l),
            (None, Some(s)) => SlotRef::Shared(ResourceId(s as u32)),
            (None, None) => SlotRef::Discard,
        },
        None => SlotRef::Discard,
    }
}

