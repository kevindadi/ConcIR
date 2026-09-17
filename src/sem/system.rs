//! Transition-system interface shared by the reference interpreter and the
//! Petri-net executor, plus the predicate language used by properties.

use std::hash::Hash;

use crate::expr::CmpOp;
use crate::sem::ids::{FunctionId, ResourceId, ThreadId};
use crate::sem::outcome::{BackendError, BackendResult, BoundaryEvent, StepLabel};
use crate::sem::program::SemProgram;
use crate::sem::value::Value;

/// A property predicate over a complete state. Goals for EF / AG EF and
/// safety assertions are both expressed as predicates.
#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    True,
    False,
    /// A `Var`/`Atomic` equals a value.
    VarEq { resource: ResourceId, value: Value },
    /// A `Var`/`Atomic` compares against a value.
    VarCmp { resource: ResourceId, op: CmpOp, value: Value },
    /// At least one activation of `func` completed (durable across join).
    FunctionCompleted { func: FunctionId },
    /// At least `n` activations of `func` completed.
    FunctionCompletedAtLeast { func: FunctionId, n: usize },
    /// The `scope` statement at `(func, sid)` completed all members.
    ScopeCompleted { func: FunctionId, sid: usize },
    /// The statement at `(func, sid)` was reached.
    StatementReached { func: FunctionId, sid: usize },
    MutexFree(ResourceId),
    MutexHeld(ResourceId),
    ChannelEmpty(ResourceId),
    ChannelAtLeast { resource: ResourceId, len: usize },
    Not(Box<Predicate>),
    And(Vec<Predicate>),
    Or(Vec<Predicate>),
}

impl Predicate {
    pub fn not(p: Predicate) -> Predicate {
        Predicate::Not(Box::new(p))
    }

    pub fn and(ps: Vec<Predicate>) -> Predicate {
        Predicate::And(ps)
    }

    pub fn or(ps: Vec<Predicate>) -> Predicate {
        Predicate::Or(ps)
    }

    pub fn description(&self) -> String {
        match self {
            Predicate::True => "true".into(),
            Predicate::False => "false".into(),
            Predicate::VarEq { resource, value } => {
                format!("r{resource} == {}", value.canonical())
            }
            Predicate::VarCmp { resource, op, value } => {
                format!("r{resource} {:?} {}", op, value.canonical())
            }
            Predicate::FunctionCompleted { func } => format!("completed(f{func})"),
            Predicate::FunctionCompletedAtLeast { func, n } => {
                format!("completed(f{func}) >= {n}")
            }
            Predicate::ScopeCompleted { func, sid } => format!("scope_done(f{func}@{sid})"),
            Predicate::StatementReached { func, sid } => format!("reached(f{func}@{sid})"),
            Predicate::MutexFree(r) => format!("free(r{r})"),
            Predicate::MutexHeld(r) => format!("held(r{r})"),
            Predicate::ChannelEmpty(r) => format!("channel_empty(r{r})"),
            Predicate::ChannelAtLeast { resource, len } => {
                format!("channel_len(r{resource}) >= {len}")
            }
            Predicate::Not(p) => format!("!({})", p.description()),
            Predicate::And(ps) => {
                let parts: Vec<String> = ps.iter().map(Predicate::description).collect();
                format!("({})", parts.join(" && "))
            }
            Predicate::Or(ps) => {
                let parts: Vec<String> = ps.iter().map(Predicate::description).collect();
                format!("({})", parts.join(" || "))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Lock,
    ChannelSend,
    ChannelRecv,
    Condvar,
    Semaphore,
    Join,
    Scope,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BlockedRecord {
    pub thread: ThreadId,
    pub kind: BlockKind,
    pub resource: Option<ResourceId>,
    pub holder: Option<ThreadId>,
    pub waiting: usize,
    pub detail: String,
}

/// Current position of one execution instance.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct InstanceState {
    pub thread: ThreadId,
    pub frame: Option<crate::sem::ids::FrameId>,
    pub function: FunctionId,
    pub sid: Option<usize>,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct Step<S> {
    pub label: StepLabel,
    pub state: S,
}

#[derive(Debug, Clone)]
pub struct Enabled<S> {
    pub steps: Vec<Step<S>>,
    pub boundary: Vec<BoundaryEvent>,
}

impl<S> Enabled<S> {
    pub fn empty() -> Self {
        Enabled {
            steps: Vec::new(),
            boundary: Vec::new(),
        }
    }
}

/// A deterministic transition system with a complete semantic state.
pub trait TransitionSystem {
    type State: Clone + Eq + Hash;

    fn program(&self) -> &SemProgram;
    fn initial(&self) -> BackendResult<Self::State>;
    fn successors(&self, state: &Self::State) -> BackendResult<Enabled<Self::State>>;

    /// True when every thread has finished normally.
    fn is_finished(&self, state: &Self::State) -> bool;

    /// Structured description of blocked threads (for diagnostics).
    fn blocked(&self, state: &Self::State) -> Vec<BlockedRecord>;

    /// Current positions of all execution instances (for diagnostics).
    fn instances(&self, _state: &Self::State) -> Vec<InstanceState> {
        Vec::new()
    }

    /// Evaluate a predicate against a state.
    fn satisfied(&self, state: &Self::State, predicate: &Predicate) -> bool;

    /// Canonical text of the complete state (for differential comparison).
    fn canonical(&self, state: &Self::State) -> String;
}

/// Compare two values with a comparison operator. `None` for incomparable
/// ordered comparisons.
pub fn compare_values(op: CmpOp, l: &Value, r: &Value) -> Option<bool> {
    let ord = match (l, r) {
        (Value::Int(a), Value::Int(b)) => a.partial_cmp(b),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
        (Value::Str(a), Value::Str(b)) => a.partial_cmp(b),
        (Value::Enum(a), Value::Enum(b)) => a.partial_cmp(b),
        (Value::Bool(a), Value::Bool(b)) => a.partial_cmp(b),
        _ => {
            return match op {
                CmpOp::Eq => Some(l == r),
                CmpOp::Ne => Some(l != r),
                _ => None,
            }
        }
    };
    let ord = ord?;
    Some(match op {
        CmpOp::Eq => ord == std::cmp::Ordering::Equal,
        CmpOp::Ne => ord != std::cmp::Ordering::Equal,
        CmpOp::Lt => ord == std::cmp::Ordering::Less,
        CmpOp::Le => ord != std::cmp::Ordering::Greater,
        CmpOp::Gt => ord == std::cmp::Ordering::Greater,
        CmpOp::Ge => ord != std::cmp::Ordering::Less,
    })
}

pub fn internal_error(message: impl Into<String>) -> BackendError {
    BackendError::invalid("E999", message.into())
}
