//! Outcomes, analysis bounds, boundary events, and structured origins.

use crate::sem::ids::{FrameId, FunctionId, ModuleId, ThreadId};
use serde::{Deserialize, Serialize};

/// Result of a verification run over a fixed contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Outcome {
    Pass,
    Fail,
    Unknown,
    Invalid,
    Unsupported,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Pass => "PASS",
            Outcome::Fail => "FAIL",
            Outcome::Unknown => "UNKNOWN",
            Outcome::Invalid => "INVALID",
            Outcome::Unsupported => "UNSUPPORTED",
        }
    }
}

/// A construct outside the supported subset (§1.2 of the design).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unsupported {
    pub construct: String,
    pub location: Option<String>,
    pub detail: String,
}

impl Unsupported {
    pub fn new(construct: impl Into<String>, detail: impl Into<String>) -> Self {
        Unsupported {
            construct: construct.into(),
            location: None,
            detail: detail.into(),
        }
    }

    pub fn at(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }
}

/// A semantic error of the program under the fixed semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invalid {
    pub code: String,
    pub message: String,
    pub location: Option<String>,
}

impl Invalid {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Invalid {
            code: code.into(),
            message: message.into(),
            location: None,
        }
    }

    pub fn at(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }
}

/// A backend error: unsupported construct, invalid program, or a hard bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    Unsupported(Unsupported),
    Invalid(Invalid),
}

impl BackendError {
    pub fn unsupported(construct: impl Into<String>, detail: impl Into<String>) -> Self {
        BackendError::Unsupported(Unsupported::new(construct, detail))
    }

    pub fn invalid(code: impl Into<String>, message: impl Into<String>) -> Self {
        BackendError::Invalid(Invalid::new(code, message))
    }

    pub fn location(&self) -> Option<&str> {
        match self {
            BackendError::Unsupported(u) => u.location.as_deref(),
            BackendError::Invalid(i) => i.location.as_deref(),
        }
    }

    pub fn at(self, location: impl Into<String>) -> Self {
        match self {
            BackendError::Unsupported(mut u) => {
                if u.location.is_none() {
                    u.location = Some(location.into());
                }
                BackendError::Unsupported(u)
            }
            BackendError::Invalid(mut i) => {
                if i.location.is_none() {
                    i.location = Some(location.into());
                }
                BackendError::Invalid(i)
            }
        }
    }
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::Unsupported(u) => {
                write!(f, "unsupported {}: {}", u.construct, u.detail)
            }
            BackendError::Invalid(i) => write!(f, "invalid [{}]: {}", i.code, i.message),
        }
    }
}

impl std::error::Error for BackendError {}

pub type BackendResult<T> = Result<T, BackendError>;

/// Analyzer limits. These are *not* part of the program semantics: reaching
/// one is recorded as a boundary and makes a search incomplete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisBounds {
    pub max_threads: usize,
    pub max_frames_per_thread: usize,
    pub max_states: usize,
    pub max_depth: usize,
    pub max_boundary_events: usize,
}

impl Default for AnalysisBounds {
    fn default() -> Self {
        AnalysisBounds {
            max_threads: 16,
            max_frames_per_thread: 32,
            max_states: 200_000,
            max_depth: 400,
            max_boundary_events: 4096,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryKind {
    ThreadLimit,
    FrameLimit,
    StateLimit,
    DepthLimit,
    EventLimit,
    UnboundedData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundaryEvent {
    pub kind: BoundaryKind,
    pub thread: Option<ThreadId>,
    pub frame: Option<FrameId>,
    pub function: Option<FunctionId>,
    pub detail: String,
}

impl BoundaryEvent {
    pub fn new(kind: BoundaryKind, detail: impl Into<String>) -> Self {
        BoundaryEvent {
            kind,
            thread: None,
            frame: None,
            function: None,
            detail: detail.into(),
        }
    }

    pub fn with_thread(mut self, thread: ThreadId) -> Self {
        self.thread = Some(thread);
        self
    }

    pub fn with_frame(mut self, frame: FrameId) -> Self {
        self.frame = Some(frame);
        self
    }
}

/// Structured origin of a transition or step.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TransitionOrigin {
    pub module: ModuleId,
    pub function: FunctionId,
    pub sid: Option<usize>,
    pub phase: Phase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// A single CIR statement's direct effect.
    Statement,
    /// Registration of a rendezvous / wait side.
    Register,
    /// The pairing of a rendezvous.
    Rendezvous,
    /// Re-acquisition of a lock after a condvar wait.
    Reacquire,
    /// Wake / notify bookkeeping.
    Wake,
    /// Completion / join bookkeeping.
    Complete,
}

/// An observable execution step: its origin plus the concrete binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepLabel {
    pub origin: TransitionOrigin,
    pub thread: Option<ThreadId>,
    pub frame: Option<FrameId>,
}

impl StepLabel {
    pub fn new(origin: TransitionOrigin) -> Self {
        StepLabel {
            origin,
            thread: None,
            frame: None,
        }
    }

    pub fn with_binding(mut self, thread: ThreadId, frame: FrameId) -> Self {
        self.thread = Some(thread);
        self.frame = Some(frame);
        self
    }

    /// Canonical text used by differential comparison.
    pub fn canonical(&self) -> String {
        format!(
            "{}::{}#{:?}/{:?}/t{:?}/f{:?}",
            self.origin.module,
            self.origin.function,
            self.origin.sid,
            self.origin.phase,
            self.thread,
            self.frame
        )
    }
}

/// Stable location `module::function.sid` for diagnostics.
pub fn location_of(
    program: &crate::sem::program::SemProgram,
    function: FunctionId,
    sid: Option<usize>,
) -> String {
    let f = program.function(function);
    let m = program.module_name(f.module);
    match sid.and_then(|i| f.body.get(i)).map(|s| s.sid.as_str()) {
        Some(sid) => crate::fqn::location(m, &f.name, sid),
        None => crate::fqn::fqn(m, &f.name),
    }
}
