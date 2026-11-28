mod exec_ctrl;
mod exec_sync;
pub mod lower;
mod predicates;
mod state_ops;

pub use lower::{build, LoweredHlnNet, PlaceKey};
pub use state_ops::{hln_to_val, val_to_hln};
pub use unipn::hln::{
    ControlToken, HlnFrame, HlnNet, HlnState, HlnVal, LockHoldMode, RwState, WaitTag,
};

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use unipn::hln::{
    state_key as hln_state_key, HlnRetAddr,
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


pub struct UnipnEngine<'a> {
    pub program: &'a SemProgram,
    pub net: LoweredHlnNet,
    pub bounds: AnalysisBounds,
    pub monitor: MonitorConfig,
}

impl<'a> UnipnEngine<'a> {
    pub fn new(program: &'a SemProgram, bounds: AnalysisBounds) -> Self {
        Self::with_monitor(program, bounds, MonitorConfig::unbounded())
    }

    pub fn with_monitor(
        program: &'a SemProgram,
        bounds: AnalysisBounds,
        monitor: MonitorConfig,
    ) -> Self {
        let net = build(program);
        Self {
            program,
            net,
            bounds,
            monitor,
        }
    }


}

impl TransitionSystem for UnipnEngine<'_> {
    type State = HlnState;

    fn program(&self) -> &SemProgram {
        self.program
    }

    fn initial(&self) -> BackendResult<HlnState> {
        let num_places = self.net.net.places.len();
        let mut state = HlnState::empty(num_places);

        for r in self.program.resources() {
            let rid = r.id.0 as usize;
            match r.kind {
                ResKind::Var | ResKind::Atomic => {
                    let v = r
                        .init
                        .clone()
                        .or_else(|| r.ty.as_ref().map(default_value))
                        .unwrap_or(Value::Int(0));
                    state.vars.insert(rid, val_to_hln(&v));
                    if let Some(&pid) = self
                        .net
                        .place_of
                        .get(&PlaceKey::Var(r.id))
                        .or_else(|| self.net.place_of.get(&PlaceKey::Atomic(r.id)))
                    {
                        state.marking.set(pid, 1);
                    }
                }
                ResKind::Mutex => {
                    state.mutexes.insert(rid, None);
                    self.sync_mutex_marking(&mut state, rid);
                }
                ResKind::RwLock => {
                    state.rwlocks.insert(rid, RwState::Free);
                    self.sync_rwlock_marking(&mut state, rid);
                }
                ResKind::Semaphore => {
                    state.semaphores.insert(rid, r.permits);
                    self.sync_sem_marking(&mut state, rid);
                }
                ResKind::Channel => {
                    state.channels.insert(rid, VecDeque::new());
                    self.sync_chan_marking(&mut state, rid);
                }
                ResKind::Condvar => {}
            }
        }

        let entry = self.program.entry();
        let mut boundary = Vec::new();
        self.create_thread(&mut state, entry, None, &mut boundary)?;
        Ok(state)
    }

    fn successors(&self, state: &HlnState) -> BackendResult<Enabled<HlnState>> {
        let mut steps = Vec::new();
        let mut boundary = Vec::new();

        let mut tokens: Vec<(PlaceId, ControlToken)> = state
            .all_tokens()
            .filter(|(_, t)| !state.finished.contains(&t.tid))
            .map(|(p, t)| (p, t.clone()))
            .collect();
        tokens.sort_by_key(|(_, t)| t.tid);

        for (pid, tok) in tokens {
            let emitted = match &tok.wait_tag {
                None => self.step_runnable_token(state, &tok, &mut boundary)?,
                Some(tag) => self.step_waiting_token(state, pid, &tok, tag)?,
            };
            steps.extend(emitted);
        }

        Ok(Enabled { steps, boundary })
    }

    fn is_finished(&self, state: &HlnState) -> bool {
        !state
            .all_tokens()
            .any(|(_, t)| !state.finished.contains(&t.tid))
    }

    fn blocked(&self, state: &HlnState) -> Vec<BlockedRecord> {
        self.blocked_impl(state)
    }
    fn instances(&self, state: &HlnState) -> Vec<InstanceState> {
        self.instances_impl(state)
    }
    fn doom_snapshot(&self, state: &HlnState) -> DoomState {
        self.doom_snapshot_impl(state)
    }
    fn satisfied(&self, state: &HlnState, predicate: &Predicate) -> bool {
        self.satisfied_impl(state, predicate)
    }
    fn canonical(&self, state: &HlnState) -> String {
        unipn::hln::state_key(state)
    }
    fn state_key(&self, state: &HlnState) -> String {
        unipn::hln::state_key(state)
    }
}
