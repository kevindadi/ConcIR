//! Reference interpreter (Phase 1).

pub mod exec;
pub mod state;

pub use exec::Interpreter;
pub use state::MachineState;
