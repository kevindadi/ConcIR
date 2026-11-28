mod canonical;
mod exec_ctrl;
mod exec_sync;
mod predicates;
pub mod state;

pub use state::MachineState;

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

use state::{
    BlockReason, FrameView, MutexState, PendingSend, RetAddr, RwLockState,
    ScopeState, ThreadState, ThreadStatus,
};


pub struct Interpreter<'a> {
    pub program: &'a SemProgram,
    pub bounds: AnalysisBounds,
    pub monitor: MonitorConfig,
}

impl<'a> Interpreter<'a> {
    pub fn new(program: &'a SemProgram, bounds: AnalysisBounds) -> Self {
        Interpreter {
            program,
            bounds,
            monitor: MonitorConfig::unbounded(),
        }
    }

    pub fn with_monitor(
        program: &'a SemProgram,
        bounds: AnalysisBounds,
        monitor: MonitorConfig,
    ) -> Self {
        Interpreter {
            program,
            bounds,
            monitor,
        }
    }


}

impl<'a> TransitionSystem for Interpreter<'a> {
    type State = MachineState;

    fn program(&self) -> &SemProgram {
        self.program
    }

    fn initial(&self) -> BackendResult<MachineState> {
        let mut state = MachineState::initial(self.program)?;
        // Drop the transparent-entry completion if the contract does not
        // observe it.
        let entry = self.program.entry();
        if self.monitor.max_for(entry) == 0 {
            state.completed_functions.remove(&entry);
        }
        Ok(state)
    }

    fn successors(&self, state: &MachineState) -> BackendResult<Enabled<MachineState>> {
        let mut enabled = Enabled::empty();
        for tid in state.threads.keys().copied().collect::<Vec<_>>() {
            match &state.threads[&tid].status {
                ThreadStatus::Runnable => {
                    for step in self.step_thread(state, tid, &mut enabled.boundary)? {
                        enabled.steps.push(step);
                    }
                }
                ThreadStatus::Blocked(reason) => {
                    for step in self.resume_thread(state, tid, reason)? {
                        enabled.steps.push(step);
                    }
                }
                ThreadStatus::Finished => {}
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
        self.blocked_impl(state)
    }
    fn instances(&self, state: &MachineState) -> Vec<concir_sem::system::InstanceState> {
        self.instances_impl(state)
    }
    fn doom_snapshot(&self, state: &MachineState) -> concir_sem::system::DoomState {
        self.doom_snapshot_impl(state)
    }
    fn satisfied(&self, state: &MachineState, predicate: &Predicate) -> bool {
        self.satisfied_impl(state, predicate)
    }
    fn canonical(&self, state: &MachineState) -> String {
        canonical::render_state(self.program, state, &|v| v.canonical())
    }
    fn state_key(&self, state: &MachineState) -> String {
        canonical::render_state(self.program, state, &|v| v.key())
    }
}
