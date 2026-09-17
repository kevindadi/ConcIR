//! Verification contract: properties, preserved behaviour, assumptions, and
//! the allowed patch scope. A contract is built from data and is never
//! modified by a candidate patch.

use serde::{Deserialize, Serialize};

use crate::expr::CmpOp;
use crate::sem::ids::{FunctionId, ResourceId};
use crate::sem::program::{ResKind, SemProgram};
use crate::sem::system::Predicate;
use crate::sem::value::{from_json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assumptions {
    /// Sequential consistency for atomics and shared reads/writes.
    #[serde(default = "yes")]
    pub sequential_consistency: bool,
    /// No spurious condvar wakeups are assumed.
    #[serde(default = "yes")]
    pub no_spurious_wakeups: bool,
}

fn yes() -> bool {
    true
}

impl Default for Assumptions {
    fn default() -> Self {
        Assumptions {
            sequential_consistency: true,
            no_spurious_wakeups: true,
        }
    }
}

/// Which functions / statements a patch is allowed to touch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatchScope {
    /// If empty, every function is in scope.
    #[serde(default)]
    pub functions: Vec<String>,
    /// If true, a patch may reorder lock acquisitions.
    #[serde(default)]
    pub allow_lock_reorder: bool,
    /// If true, a patch may drop a statement.
    #[serde(default)]
    pub allow_statement_delete: bool,
}

impl PatchScope {
    pub fn allows_function(&self, name: &str) -> bool {
        self.functions.is_empty() || self.functions.iter().any(|f| f == name)
    }

    pub fn unrestricted() -> Self {
        PatchScope {
            functions: Vec::new(),
            allow_lock_reorder: true,
            allow_statement_delete: false,
        }
    }
}

/// One required property.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PropertySpec {
    /// `invariant` must hold in every reachable state.
    Safety { id: String, invariant: PredicateSpec },
    /// No reachable deadlock state.
    DeadlockFree { id: String },
    /// Some reachable state satisfies `goal` (EF).
    Reachability { id: String, goal: PredicateSpec },
    /// Every reachable state can still reach `goal` (AG EF).
    AlwaysReachable { id: String, goal: PredicateSpec },
    /// No reachable state satisfies `bad`.
    Unreachable { id: String, bad: PredicateSpec },
}

/// An observable behaviour a patch must preserve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PreservedSpec {
    /// `goal` must remain reachable.
    Reachable { description: String, goal: PredicateSpec },
    /// `invariant` must remain true in every reachable state.
    Always { description: String, invariant: PredicateSpec },
}

/// Serializable predicate description, resolved against the program.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PredicateSpec {
    True,
    False,
    VarEq { resource: String, value: serde_json::Value },
    VarCmp {
        resource: String,
        op: String,
        value: serde_json::Value,
    },
    FunctionCompleted { function: String },
    FunctionCompletedAtLeast { function: String, n: usize },
    ScopeCompleted { function: String, sid: String },
    StatementReached { function: String, sid: String },
    MutexFree { resource: String },
    MutexHeld { resource: String },
    ChannelEmpty { resource: String },
    ChannelAtLeast { resource: String, len: usize },
    Not { predicate: Box<PredicateSpec> },
    And { predicates: Vec<PredicateSpec> },
    Or { predicates: Vec<PredicateSpec> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PropertySpecResolved {
    pub id: String,
    pub property: Property,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Property {
    Safety { invariant: Predicate },
    DeadlockFree,
    Reachability { goal: Predicate },
    AlwaysReachable { goal: Predicate },
    Unreachable { bad: Predicate },
}

#[derive(Debug, Clone)]
pub struct PreservedBehavior {
    pub description: String,
    pub behavior: Preserved,
}

#[derive(Debug, Clone)]
pub enum Preserved {
    Reachable(Predicate),
    Always(Predicate),
}

/// The fixed verification contract.
#[derive(Debug, Clone)]
pub struct VerificationContract {
    pub name: String,
    pub properties: Vec<PropertySpecResolved>,
    pub preserved: Vec<PreservedBehavior>,
    pub assumptions: Assumptions,
    pub bounds: crate::sem::outcome::AnalysisBounds,
    pub allowed_scope: PatchScope,
}

/// Serializable contract document.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContractSpec {
    pub name: String,
    pub properties: Vec<PropertySpec>,
    #[serde(default)]
    pub preserved: Vec<PreservedSpec>,
    #[serde(default)]
    pub assumptions: Assumptions,
    #[serde(default)]
    pub bounds: BoundsSpec,
    #[serde(default)]
    pub allowed_scope: PatchScope,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BoundsSpec {
    #[serde(default = "d_threads")]
    pub max_threads: usize,
    #[serde(default = "d_frames")]
    pub max_frames_per_thread: usize,
    #[serde(default = "d_states")]
    pub max_states: usize,
    #[serde(default = "d_depth")]
    pub max_depth: usize,
    #[serde(default = "d_events")]
    pub max_boundary_events: usize,
}

fn d_threads() -> usize {
    16
}
fn d_frames() -> usize {
    32
}
fn d_states() -> usize {
    200_000
}
fn d_depth() -> usize {
    400
}
fn d_events() -> usize {
    4096
}

impl Default for BoundsSpec {
    fn default() -> Self {
        BoundsSpec {
            max_threads: d_threads(),
            max_frames_per_thread: d_frames(),
            max_states: d_states(),
            max_depth: d_depth(),
            max_boundary_events: d_events(),
        }
    }
}

impl From<&BoundsSpec> for crate::sem::outcome::AnalysisBounds {
    fn from(b: &BoundsSpec) -> Self {
        crate::sem::outcome::AnalysisBounds {
            max_threads: b.max_threads,
            max_frames_per_thread: b.max_frames_per_thread,
            max_states: b.max_states,
            max_depth: b.max_depth,
            max_boundary_events: b.max_boundary_events,
        }
    }
}

impl ContractSpec {
    pub fn resolve(&self, program: &SemProgram) -> Result<VerificationContract, String> {
        let default_module = crate::sem::ids::ModuleId(0);
        let mut properties = Vec::new();
        for p in &self.properties {
            let (id, property) = match p {
                PropertySpec::Safety { id, invariant } => (
                    id.clone(),
                    Property::Safety {
                        invariant: invariant.resolve(program, default_module)?,
                    },
                ),
                PropertySpec::DeadlockFree { id } => (id.clone(), Property::DeadlockFree),
                PropertySpec::Reachability { id, goal } => (
                    id.clone(),
                    Property::Reachability {
                        goal: goal.resolve(program, default_module)?,
                    },
                ),
                PropertySpec::AlwaysReachable { id, goal } => (
                    id.clone(),
                    Property::AlwaysReachable {
                        goal: goal.resolve(program, default_module)?,
                    },
                ),
                PropertySpec::Unreachable { id, bad } => (
                    id.clone(),
                    Property::Unreachable {
                        bad: bad.resolve(program, default_module)?,
                    },
                ),
            };
            properties.push(PropertySpecResolved { id, property });
        }
        let mut preserved = Vec::new();
        for p in &self.preserved {
            preserved.push(match p {
                PreservedSpec::Reachable { description, goal } => PreservedBehavior {
                    description: description.clone(),
                    behavior: Preserved::Reachable(goal.resolve(program, default_module)?),
                },
                PreservedSpec::Always {
                    description,
                    invariant,
                } => PreservedBehavior {
                    description: description.clone(),
                    behavior: Preserved::Always(invariant.resolve(program, default_module)?),
                },
            });
        }
        Ok(VerificationContract {
            name: self.name.clone(),
            properties,
            preserved,
            assumptions: self.assumptions.clone(),
            bounds: (&self.bounds).into(),
            allowed_scope: self.allowed_scope.clone(),
        })
    }
}

impl PredicateSpec {
    pub fn resolve(
        &self,
        program: &SemProgram,
        default_module: crate::sem::ids::ModuleId,
    ) -> Result<Predicate, String> {
        Ok(match self {
            PredicateSpec::True => Predicate::True,
            PredicateSpec::False => Predicate::False,
            PredicateSpec::VarEq { resource, value } => {
                let rid = resolve_resource(program, default_module, resource)?;
                let v = resolve_value(program, rid, value)?;
                Predicate::VarEq {
                    resource: rid,
                    value: v,
                }
            }
            PredicateSpec::VarCmp {
                resource,
                op,
                value,
            } => {
                let rid = resolve_resource(program, default_module, resource)?;
                let v = resolve_value(program, rid, value)?;
                Predicate::VarCmp {
                    resource: rid,
                    op: parse_op(op)?,
                    value: v,
                }
            }
            PredicateSpec::FunctionCompleted { function } => Predicate::FunctionCompleted {
                func: resolve_function(program, default_module, function)?,
            },
            PredicateSpec::FunctionCompletedAtLeast { function, n } => {
                Predicate::FunctionCompletedAtLeast {
                    func: resolve_function(program, default_module, function)?,
                    n: *n,
                }
            }
            PredicateSpec::ScopeCompleted { function, sid } => {
                let f = resolve_function(program, default_module, function)?;
                let idx = program
                    .function(f)
                    .stmt_index(sid)
                    .ok_or_else(|| format!("function '{function}' has no statement '{sid}'"))?;
                Predicate::ScopeCompleted { func: f, sid: idx }
            }
            PredicateSpec::StatementReached { function, sid } => {
                let f = resolve_function(program, default_module, function)?;
                let idx = program
                    .function(f)
                    .stmt_index(sid)
                    .ok_or_else(|| format!("function '{function}' has no statement '{sid}'"))?;
                Predicate::StatementReached { func: f, sid: idx }
            }
            PredicateSpec::MutexFree { resource } => {
                Predicate::MutexFree(resolve_resource(program, default_module, resource)?)
            }
            PredicateSpec::MutexHeld { resource } => {
                Predicate::MutexHeld(resolve_resource(program, default_module, resource)?)
            }
            PredicateSpec::ChannelEmpty { resource } => {
                Predicate::ChannelEmpty(resolve_resource(program, default_module, resource)?)
            }
            PredicateSpec::ChannelAtLeast { resource, len } => Predicate::ChannelAtLeast {
                resource: resolve_resource(program, default_module, resource)?,
                len: *len,
            },
            PredicateSpec::Not { predicate } => {
                Predicate::not(predicate.resolve(program, default_module)?)
            }
            PredicateSpec::And { predicates } => Predicate::and(
                predicates
                    .iter()
                    .map(|p| p.resolve(program, default_module))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            PredicateSpec::Or { predicates } => Predicate::or(
                predicates
                    .iter()
                    .map(|p| p.resolve(program, default_module))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        })
    }
}

fn resolve_resource(
    program: &SemProgram,
    default_module: crate::sem::ids::ModuleId,
    name: &str,
) -> Result<ResourceId, String> {
    program
        .resolve_resource(default_module, name)
        .ok_or_else(|| format!("unknown resource '{name}'"))
}

fn resolve_function(
    program: &SemProgram,
    default_module: crate::sem::ids::ModuleId,
    name: &str,
) -> Result<FunctionId, String> {
    program
        .resolve_function(default_module, name)
        .ok_or_else(|| format!("unknown function '{name}'"))
}

fn resolve_value(
    program: &SemProgram,
    resource: ResourceId,
    json: &serde_json::Value,
) -> Result<Value, String> {
    let r = program.resource(resource);
    if !matches!(r.kind, ResKind::Var | ResKind::Atomic) {
        return Err(format!("resource '{}' is not a Var/Atomic", r.name));
    }
    let ty = r.ty.as_ref().ok_or("resource has no value type")?;
    from_json(json, ty)
}

fn parse_op(op: &str) -> Result<CmpOp, String> {
    Ok(match op {
        "==" => CmpOp::Eq,
        "!=" => CmpOp::Ne,
        "<" => CmpOp::Lt,
        "<=" => CmpOp::Le,
        ">" => CmpOp::Gt,
        ">=" => CmpOp::Ge,
        other => return Err(format!("unknown comparison operator '{other}'")),
    })
}
