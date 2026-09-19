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
    VarEq {
        resource: ResourceId,
        value: Value,
    },
    /// A `Var`/`Atomic` compares against a value.
    VarCmp {
        resource: ResourceId,
        op: CmpOp,
        value: Value,
    },
    /// At least one activation of `func` completed (durable across join).
    FunctionCompleted {
        func: FunctionId,
    },
    /// At least `n` activations of `func` completed.
    FunctionCompletedAtLeast {
        func: FunctionId,
        n: usize,
    },
    /// The `scope` statement at `(func, sid)` completed all members.
    ScopeCompleted {
        func: FunctionId,
        sid: usize,
    },
    /// The statement at `(func, sid)` was reached.
    StatementReached {
        func: FunctionId,
        sid: usize,
    },
    MutexFree(ResourceId),
    MutexHeld(ResourceId),
    /// Some thread currently executing `func` (at any frame depth) holds every
    /// resource in `resources` (mutex held by that thread; a semaphore counts
    /// as held while permits are below its initial count).
    HoldsAll {
        func: FunctionId,
        resources: Vec<ResourceId>,
    },
    /// At most one holder of `resource` at a time (mutex invariant, or a
    /// non-negative semaphore count).
    MutexExclusive(ResourceId),
    /// No thread executing `func` holds all of `resources` (forbids nesting).
    NeverHoldsAll {
        func: FunctionId,
        resources: Vec<ResourceId>,
    },
    ChannelEmpty(ResourceId),
    ChannelAtLeast {
        resource: ResourceId,
        len: usize,
    },
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
            Predicate::VarCmp {
                resource,
                op,
                value,
            } => {
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
            Predicate::HoldsAll { func, resources } => {
                let rs: Vec<String> = resources.iter().map(|r| format!("r{r}")).collect();
                format!("holds_all(f{func}, [{}])", rs.join(", "))
            }
            Predicate::MutexExclusive(r) => format!("mutex_exclusive(r{r})"),
            Predicate::NeverHoldsAll { func, resources } => {
                let rs: Vec<String> = resources.iter().map(|r| format!("r{r}")).collect();
                format!("never_holds_all(f{func}, [{}])", rs.join(", "))
            }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BlockedRecord {
    pub thread: ThreadId,
    pub kind: BlockKind,
    pub resource: Option<ResourceId>,
    /// Stable `module::entity` name of the blocking resource (for providers).
    pub resource_name: Option<String>,
    pub holder: Option<ThreadId>,
    pub waiting: usize,
    pub detail: String,
}

/// What a thread is waiting on, rendered by name.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DoomWait {
    pub kind: String,
    pub resource: Option<String>,
}

/// One thread's position and resources in a stuck ("doom") state.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DoomThread {
    pub thread: u32,
    pub entry_function: String,
    /// `module::function` of the current frame.
    pub function: String,
    /// `sid` of the statement the thread is at, when it is a real statement.
    pub at_sid: Option<String>,
    /// Held mutex resources (FQNs).
    pub holds: Vec<String>,
    pub waiting_on: Option<DoomWait>,
}

/// A human-readable summary of why a state cannot progress (or cannot reach a
/// goal): per-thread holds/waiting plus the currently free mutexes.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DoomState {
    pub threads: Vec<DoomThread>,
    pub free_resources: Vec<String>,
}

/// Current position of one execution instance.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

    /// Per-thread holds/waiting plus free mutexes, for readable diagnostics.
    fn doom_snapshot(&self, _state: &Self::State) -> DoomState {
        DoomState::default()
    }

    /// Evaluate a predicate against a state.
    fn satisfied(&self, state: &Self::State, predicate: &Predicate) -> bool;

    /// Human-readable rendering of the complete state (diagnostics only).
    fn canonical(&self, state: &Self::State) -> String;

    /// Unambiguous, identity-normalized semantic key used for state
    /// deduplication. Equal keys must imply equal predicate truth and equal
    /// successor quotient behavior. Never used as display text.
    fn state_key(&self, state: &Self::State) -> String;
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
