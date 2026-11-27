//! Verification contract: properties, preserved behaviour, assumptions, and
//! the allowed patch scope. A contract is built from data and is never
//! modified by a candidate patch.

use serde::{Deserialize, Serialize};
use unipn::analysis::obligations::BugKind;
use unipn::analysis::temporal::{
    explain_ctl, explain_ltl, CtlFormula, FairnessConstraint, LtlFormula,
};

use super::patterns::{CompiledPattern, ConstraintPattern};

use crate::expr::CmpOp;
use crate::sem::ids::{FunctionId, ResourceId};
use crate::sem::program::{ResKind, SemProgram};
use crate::sem::system::Predicate;
use crate::sem::value::{from_json, Value};

pub const CONTRACT_EXPLANATION_SCHEMA: &str = "concir-contract-explanation/1";

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
    /// Module names this patch scope is restricted to. Empty = all modules.
    #[serde(default)]
    pub modules: Vec<String>,
    /// If true, a patch may reorder lock acquisitions.
    #[serde(default)]
    pub allow_lock_reorder: bool,
    /// If true, a patch may drop a statement.
    #[serde(default)]
    pub allow_statement_delete: bool,
}

impl PatchScope {
    pub fn allows_module(&self, name: &str) -> bool {
        self.modules.is_empty() || self.modules.iter().any(|m| m == name)
    }

    /// Match a patch target against the allowed function set. An entry with
    /// `module::function` requires exact identity; a bare entry is a legacy
    /// short name and matches the function name in any module.
    pub fn allows_function(&self, module: &str, function: &str) -> bool {
        if self.functions.is_empty() {
            return true;
        }
        self.functions
            .iter()
            .any(|entry| match entry.split_once("::") {
                Some((m, f)) => m == module && f == function,
                None => entry == function,
            })
    }

    pub fn unrestricted() -> Self {
        PatchScope {
            functions: Vec::new(),
            modules: Vec::new(),
            allow_lock_reorder: true,
            allow_statement_delete: false,
        }
    }
}

/// A contract-spec error. `Unsupported` means the requested semantics/config
/// is not implemented; `Invalid` means the input is malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    Unsupported(String),
    Invalid(String),
}

impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContractError::Unsupported(m) => write!(f, "unsupported contract: {m}"),
            ContractError::Invalid(m) => write!(f, "invalid contract: {m}"),
        }
    }
}

impl std::error::Error for ContractError {}

/// One required property.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PropertySpec {
    /// `invariant` must hold in every reachable state.
    Safety {
        id: String,
        invariant: PredicateSpec,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        req: Vec<String>,
    },
    /// No reachable deadlock state.
    DeadlockFree {
        id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        req: Vec<String>,
    },
    /// Some reachable state satisfies `goal` (EF).
    Reachability {
        id: String,
        goal: PredicateSpec,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        req: Vec<String>,
    },
    /// Every reachable state can still reach `goal` (AG EF).
    AlwaysReachable {
        id: String,
        goal: PredicateSpec,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        req: Vec<String>,
    },
    /// No reachable state satisfies `bad`.
    Unreachable {
        id: String,
        bad: PredicateSpec,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        req: Vec<String>,
    },
    /// Full CTL temporal formula evaluated via `unipn::analysis::temporal::check_ctl_graph`.
    Ctl {
        id: String,
        formula: CtlFormula<PredicateSpec>,
        #[serde(default)]
        non_vacuous: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        req: Vec<String>,
    },
    /// Full LTL temporal formula (with optional fairness) evaluated via `unipn::analysis::temporal::check_ltl_graph`.
    Ltl {
        id: String,
        formula: LtlFormula<PredicateSpec>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        fairness: Vec<FairnessConstraint<PredicateSpec>>,
        #[serde(default)]
        non_vacuous: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        req: Vec<String>,
    },
    /// Reusable constraint pattern compiled into CTL / Fair-LTL.
    Pattern {
        id: String,
        #[serde(flatten)]
        pattern: ConstraintPattern,
        #[serde(default = "yes")]
        non_vacuous: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        req: Vec<String>,
    },
    /// Pure logic-driven concurrency health suite (`unipn::analysis::obligations`).
    Health {
        id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        checks: Vec<BugKind>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        req: Vec<String>,
    },
}

/// An observable behaviour a patch must preserve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PreservedSpec {
    /// `goal` must remain reachable.
    Reachable {
        description: String,
        goal: PredicateSpec,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        req: Vec<String>,
    },
    /// `invariant` must remain true in every reachable state.
    Always {
        description: String,
        invariant: PredicateSpec,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        req: Vec<String>,
    },
}

/// Serializable predicate description, resolved against the program.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PredicateSpec {
    True,
    False,
    VarEq {
        resource: String,
        value: serde_json::Value,
    },
    VarCmp {
        resource: String,
        op: String,
        value: serde_json::Value,
    },
    FunctionCompleted {
        function: String,
    },
    FunctionCompletedAtLeast {
        function: String,
        n: usize,
    },
    ScopeCompleted {
        function: String,
        sid: String,
    },
    StatementReached {
        function: String,
        sid: String,
    },
    MutexFree {
        resource: String,
    },
    MutexHeld {
        resource: String,
    },
    /// Some thread executing `function` holds every `resources` entry.
    HoldsAll {
        function: String,
        resources: Vec<String>,
    },
    /// At most one holder of `resource`.
    MutexExclusive {
        resource: String,
    },
    /// No thread executing `function` holds all `resources`.
    NeverHoldsAll {
        function: String,
        resources: Vec<String>,
    },
    ChannelEmpty {
        resource: String,
    },
    ChannelAtLeast {
        resource: String,
        len: usize,
    },
    Not {
        predicate: Box<PredicateSpec>,
    },
    And {
        predicates: Vec<PredicateSpec>,
    },
    Or {
        predicates: Vec<PredicateSpec>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PropertySpecResolved {
    pub id: String,
    pub property: Property,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Property {
    Safety {
        invariant: Predicate,
    },
    DeadlockFree,
    Reachability {
        goal: Predicate,
    },
    AlwaysReachable {
        goal: Predicate,
    },
    Unreachable {
        bad: Predicate,
    },
    Ctl {
        formula: CtlFormula<Predicate>,
        non_vacuous: bool,
        explanation: String,
    },
    Ltl {
        formula: LtlFormula<Predicate>,
        fairness: Vec<FairnessConstraint<Predicate>>,
        non_vacuous: bool,
        explanation: String,
    },
    PatternLtl {
        formula: LtlFormula<Predicate>,
        fairness: Vec<FairnessConstraint<Predicate>>,
        non_vacuous: bool,
        witness_reachable: Option<Predicate>,
        witness_desc: Option<String>,
        explanation: String,
    },
    Health {
        checks: Vec<BugKind>,
        explanation: String,
    },
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
    /// The symbolic spec this contract was resolved from, so it can be
    /// re-bound against a patched program without reusing stale body indices.
    pub source: ContractSpec,
}

impl VerificationContract {
    /// Every predicate the contract observes (for monitor derivation).
    pub fn predicates(&self) -> Vec<&Predicate> {
        let mut out = Vec::new();
        for p in &self.properties {
            collect_property_predicates(&p.property, &mut out);
        }
        for p in &self.preserved {
            match &p.behavior {
                Preserved::Reachable(pred) | Preserved::Always(pred) => out.push(pred),
            }
        }
        out
    }

    /// Re-bind this contract against a (possibly patched) program. Fails if a
    /// referenced function, statement, resource, or type no longer exists.
    pub fn rebind(&self, program: &SemProgram) -> Result<VerificationContract, ContractError> {
        self.source.resolve(program)
    }
}

fn collect_ctl_predicates<'a>(f: &'a CtlFormula<Predicate>, out: &mut Vec<&'a Predicate>) {
    match f {
        CtlFormula::True | CtlFormula::False => {}
        CtlFormula::Atom { atom } => out.push(atom),
        CtlFormula::Not { arg }
        | CtlFormula::Ex { arg }
        | CtlFormula::Ax { arg }
        | CtlFormula::Ef { arg }
        | CtlFormula::Af { arg }
        | CtlFormula::Eg { arg }
        | CtlFormula::Ag { arg } => collect_ctl_predicates(arg, out),
        CtlFormula::And { args } | CtlFormula::Or { args } => {
            for a in args {
                collect_ctl_predicates(a, out);
            }
        }
        CtlFormula::Implies { lhs, rhs }
        | CtlFormula::Eu { lhs, rhs }
        | CtlFormula::Au { lhs, rhs } => {
            collect_ctl_predicates(lhs, out);
            collect_ctl_predicates(rhs, out);
        }
    }
}

fn collect_ltl_predicates<'a>(f: &'a LtlFormula<Predicate>, out: &mut Vec<&'a Predicate>) {
    match f {
        LtlFormula::True | LtlFormula::False => {}
        LtlFormula::Atom { atom } => out.push(atom),
        LtlFormula::Not { arg }
        | LtlFormula::Next { arg }
        | LtlFormula::Eventually { arg }
        | LtlFormula::Always { arg } => collect_ltl_predicates(arg, out),
        LtlFormula::And { args } | LtlFormula::Or { args } => {
            for a in args {
                collect_ltl_predicates(a, out);
            }
        }
        LtlFormula::Implies { lhs, rhs }
        | LtlFormula::Until { lhs, rhs }
        | LtlFormula::Release { lhs, rhs } => {
            collect_ltl_predicates(lhs, out);
            collect_ltl_predicates(rhs, out);
        }
    }
}

fn collect_property_predicates<'a>(p: &'a Property, out: &mut Vec<&'a Predicate>) {
    match p {
        Property::Safety { invariant } | Property::Unreachable { bad: invariant } => {
            out.push(invariant)
        }
        Property::Reachability { goal } | Property::AlwaysReachable { goal } => out.push(goal),
        Property::DeadlockFree => {}
        Property::Ctl { formula, .. } => collect_ctl_predicates(formula, out),
        Property::Ltl {
            formula, fairness, ..
        } => {
            collect_ltl_predicates(formula, out);
            for fc in fairness {
                match fc {
                    FairnessConstraint::Justice { atom } => out.push(atom),
                    FairnessConstraint::Compassion { enabled, taken } => {
                        out.push(enabled);
                        out.push(taken);
                    }
                }
            }
        }
        Property::PatternLtl {
            formula,
            fairness,
            witness_reachable,
            ..
        } => {
            collect_ltl_predicates(formula, out);
            for fc in fairness {
                match fc {
                    FairnessConstraint::Justice { atom } => out.push(atom),
                    FairnessConstraint::Compassion { enabled, taken } => {
                        out.push(enabled);
                        out.push(taken);
                    }
                }
            }
            if let Some(w) = witness_reachable {
                out.push(w);
            }
        }
        Property::Health { .. } => {}
    }
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
    pub fn resolve(&self, program: &SemProgram) -> Result<VerificationContract, ContractError> {
        validate_assumptions(&self.assumptions)?;
        validate_bounds(&self.bounds)?;
        let mut seen_ids = std::collections::HashSet::new();
        for p in &self.properties {
            let id = property_id(p);
            if id.trim().is_empty() {
                return Err(ContractError::Invalid(
                    "property id must not be empty".into(),
                ));
            }
            if !seen_ids.insert(id.to_string()) {
                return Err(ContractError::Invalid(format!(
                    "duplicate property id '{id}'"
                )));
            }
        }
        // Unqualified contract names resolve in the entry module's namespace,
        // which is stable under declaration/module reordering (never
        // `ModuleId(0)`).
        let default_module = program.entry_module();
        let mut properties = Vec::new();
        for p in &self.properties {
            let resolve = |e: &PredicateSpec| {
                e.resolve(program, default_module)
                    .map_err(ContractError::Invalid)
            };
            let (id, property) = match p {
                PropertySpec::Safety { id, invariant, .. } => (
                    id.clone(),
                    Property::Safety {
                        invariant: resolve(invariant)?,
                    },
                ),
                PropertySpec::DeadlockFree { id, .. } => (id.clone(), Property::DeadlockFree),
                PropertySpec::Reachability { id, goal, .. } => (
                    id.clone(),
                    Property::Reachability {
                        goal: resolve(goal)?,
                    },
                ),
                PropertySpec::AlwaysReachable { id, goal, .. } => (
                    id.clone(),
                    Property::AlwaysReachable {
                        goal: resolve(goal)?,
                    },
                ),
                PropertySpec::Unreachable { id, bad, .. } => {
                    (id.clone(), Property::Unreachable { bad: resolve(bad)? })
                }
                PropertySpec::Ctl {
                    id,
                    formula,
                    non_vacuous,
                    ..
                } => {
                    let resolved = resolve_ctl(formula, &resolve)?;
                    let explanation = explain_ctl(formula, PredicateSpec::explain);
                    (
                        id.clone(),
                        Property::Ctl {
                            formula: resolved,
                            non_vacuous: *non_vacuous,
                            explanation,
                        },
                    )
                }
                PropertySpec::Ltl {
                    id,
                    formula,
                    fairness,
                    non_vacuous,
                    ..
                } => {
                    let resolved = resolve_ltl(formula, &resolve)?;
                    let mut resolved_fairness = Vec::with_capacity(fairness.len());
                    for fc in fairness {
                        resolved_fairness.push(resolve_fairness(fc, &resolve)?);
                    }
                    let explanation = explain_ltl(formula, fairness, PredicateSpec::explain);
                    (
                        id.clone(),
                        Property::Ltl {
                            formula: resolved,
                            fairness: resolved_fairness,
                            non_vacuous: *non_vacuous,
                            explanation,
                        },
                    )
                }
                PropertySpec::Pattern {
                    id,
                    pattern,
                    non_vacuous,
                    ..
                } => {
                    let prop = match pattern.resolve(program, default_module, *non_vacuous)? {
                        CompiledPattern::Ctl {
                            formula,
                            non_vacuous,
                            explanation,
                        } => Property::Ctl {
                            formula,
                            non_vacuous,
                            explanation,
                        },
                        CompiledPattern::Ltl {
                            formula,
                            fairness,
                            non_vacuous,
                            witness_reachable,
                            witness_desc,
                            explanation,
                        } => Property::PatternLtl {
                            formula,
                            fairness,
                            non_vacuous,
                            witness_reachable,
                            witness_desc,
                            explanation,
                        },
                    };
                    (id.clone(), prop)
                }
                PropertySpec::Health { id, checks, .. } => {
                    let active = if checks.is_empty() {
                        BugKind::default_suite()
                    } else {
                        checks.clone()
                    };
                    let names: Vec<&str> = active.iter().map(|b| b.label()).collect();
                    (
                        id.clone(),
                        Property::Health {
                            checks: active,
                            explanation: format!(
                                "Check concurrency health obligations: [{}].",
                                names.join(", ")
                            ),
                        },
                    )
                }
            };
            properties.push(PropertySpecResolved { id, property });
        }
        let mut preserved = Vec::new();
        for p in &self.preserved {
            preserved.push(match p {
                PreservedSpec::Reachable {
                    description, goal, ..
                } => PreservedBehavior {
                    description: description.clone(),
                    behavior: Preserved::Reachable(
                        goal.resolve(program, default_module)
                            .map_err(ContractError::Invalid)?,
                    ),
                },
                PreservedSpec::Always {
                    description,
                    invariant,
                    ..
                } => PreservedBehavior {
                    description: description.clone(),
                    behavior: Preserved::Always(
                        invariant
                            .resolve(program, default_module)
                            .map_err(ContractError::Invalid)?,
                    ),
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
            source: self.clone(),
        })
    }

    /// Produce a deterministic natural-language explanation of every property
    /// and preserved requirement in this contract (`concir-contract-explanation/1`).
    pub fn explain(&self) -> ContractExplanation {
        let properties = self
            .properties
            .iter()
            .map(|p| match p {
                PropertySpec::Safety { id, invariant, req } => PropertyExplanation {
                    id: id.clone(),
                    kind: "safety".to_string(),
                    req: req.clone(),
                    non_vacuous: false,
                    explanation: format!("In every reachable state, {}.", invariant.explain()),
                },
                PropertySpec::DeadlockFree { id, req } => PropertyExplanation {
                    id: id.clone(),
                    kind: "deadlock_free".to_string(),
                    req: req.clone(),
                    non_vacuous: false,
                    explanation: "No reachable state is a global deadlock.".to_string(),
                },
                PropertySpec::Reachability { id, goal, req } => PropertyExplanation {
                    id: id.clone(),
                    kind: "reachability".to_string(),
                    req: req.clone(),
                    non_vacuous: false,
                    explanation: format!("There exists a reachable state where {}.", goal.explain()),
                },
                PropertySpec::AlwaysReachable { id, goal, req } => PropertyExplanation {
                    id: id.clone(),
                    kind: "always_reachable".to_string(),
                    req: req.clone(),
                    non_vacuous: false,
                    explanation: format!(
                        "From every reachable state, it remains possible to reach a state where {}.",
                        goal.explain()
                    ),
                },
                PropertySpec::Unreachable { id, bad, req } => PropertyExplanation {
                    id: id.clone(),
                    kind: "unreachable".to_string(),
                    req: req.clone(),
                    non_vacuous: false,
                    explanation: format!("No reachable state satisfies {}.", bad.explain()),
                },
                PropertySpec::Ctl {
                    id,
                    formula,
                    non_vacuous,
                    req,
                } => PropertyExplanation {
                    id: id.clone(),
                    kind: "ctl".to_string(),
                    req: req.clone(),
                    non_vacuous: *non_vacuous,
                    explanation: explain_ctl(formula, PredicateSpec::explain),
                },
                PropertySpec::Ltl {
                    id,
                    formula,
                    fairness,
                    non_vacuous,
                    req,
                } => PropertyExplanation {
                    id: id.clone(),
                    kind: "ltl".to_string(),
                    req: req.clone(),
                    non_vacuous: *non_vacuous,
                    explanation: explain_ltl(formula, fairness, PredicateSpec::explain),
                },
                PropertySpec::Pattern {
                    id,
                    pattern,
                    non_vacuous,
                    req,
                } => PropertyExplanation {
                    id: id.clone(),
                    kind: "pattern".to_string(),
                    req: req.clone(),
                    non_vacuous: *non_vacuous,
                    explanation: pattern.explain(),
                },
                PropertySpec::Health { id, checks, req } => {
                    let active = if checks.is_empty() {
                        BugKind::default_suite()
                    } else {
                        checks.clone()
                    };
                    let names: Vec<&str> = active.iter().map(|b| b.label()).collect();
                    PropertyExplanation {
                        id: id.clone(),
                        kind: "health".to_string(),
                        req: req.clone(),
                        non_vacuous: false,
                        explanation: format!(
                            "Verify concurrency health obligations: [{}].",
                            names.join(", ")
                        ),
                    }
                }
            })
            .collect();

        let preserved = self
            .preserved
            .iter()
            .map(|p| match p {
                PreservedSpec::Reachable {
                    description,
                    goal,
                    req,
                } => PreservedExplanation {
                    description: description.clone(),
                    req: req.clone(),
                    explanation: format!(
                        "Behavior '{}' must remain reachable ({}).",
                        description,
                        goal.explain()
                    ),
                },
                PreservedSpec::Always {
                    description,
                    invariant,
                    req,
                } => PreservedExplanation {
                    description: description.clone(),
                    req: req.clone(),
                    explanation: format!(
                        "Invariant '{}' must always hold ({}).",
                        description,
                        invariant.explain()
                    ),
                },
            })
            .collect();

        ContractExplanation {
            schema: CONTRACT_EXPLANATION_SCHEMA.to_string(),
            name: self.name.clone(),
            properties,
            preserved,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropertyExplanation {
    pub id: String,
    pub kind: String,
    pub req: Vec<String>,
    pub non_vacuous: bool,
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreservedExplanation {
    pub description: String,
    pub req: Vec<String>,
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractExplanation {
    pub schema: String,
    pub name: String,
    pub properties: Vec<PropertyExplanation>,
    pub preserved: Vec<PreservedExplanation>,
}

fn resolve_ctl<F>(
    f: &CtlFormula<PredicateSpec>,
    resolve: &F,
) -> Result<CtlFormula<Predicate>, ContractError>
where
    F: Fn(&PredicateSpec) -> Result<Predicate, ContractError>,
{
    Ok(match f {
        CtlFormula::True => CtlFormula::True,
        CtlFormula::False => CtlFormula::False,
        CtlFormula::Atom { atom } => CtlFormula::Atom {
            atom: resolve(atom)?,
        },
        CtlFormula::Not { arg } => CtlFormula::Not {
            arg: Box::new(resolve_ctl(arg, resolve)?),
        },
        CtlFormula::And { args } => CtlFormula::And {
            args: args
                .iter()
                .map(|a| resolve_ctl(a, resolve))
                .collect::<Result<Vec<_>, _>>()?,
        },
        CtlFormula::Or { args } => CtlFormula::Or {
            args: args
                .iter()
                .map(|a| resolve_ctl(a, resolve))
                .collect::<Result<Vec<_>, _>>()?,
        },
        CtlFormula::Implies { lhs, rhs } => CtlFormula::Implies {
            lhs: Box::new(resolve_ctl(lhs, resolve)?),
            rhs: Box::new(resolve_ctl(rhs, resolve)?),
        },
        CtlFormula::Ex { arg } => CtlFormula::Ex {
            arg: Box::new(resolve_ctl(arg, resolve)?),
        },
        CtlFormula::Ax { arg } => CtlFormula::Ax {
            arg: Box::new(resolve_ctl(arg, resolve)?),
        },
        CtlFormula::Ef { arg } => CtlFormula::Ef {
            arg: Box::new(resolve_ctl(arg, resolve)?),
        },
        CtlFormula::Af { arg } => CtlFormula::Af {
            arg: Box::new(resolve_ctl(arg, resolve)?),
        },
        CtlFormula::Eg { arg } => CtlFormula::Eg {
            arg: Box::new(resolve_ctl(arg, resolve)?),
        },
        CtlFormula::Ag { arg } => CtlFormula::Ag {
            arg: Box::new(resolve_ctl(arg, resolve)?),
        },
        CtlFormula::Eu { lhs, rhs } => CtlFormula::Eu {
            lhs: Box::new(resolve_ctl(lhs, resolve)?),
            rhs: Box::new(resolve_ctl(rhs, resolve)?),
        },
        CtlFormula::Au { lhs, rhs } => CtlFormula::Au {
            lhs: Box::new(resolve_ctl(lhs, resolve)?),
            rhs: Box::new(resolve_ctl(rhs, resolve)?),
        },
    })
}

fn resolve_ltl<F>(
    f: &LtlFormula<PredicateSpec>,
    resolve: &F,
) -> Result<LtlFormula<Predicate>, ContractError>
where
    F: Fn(&PredicateSpec) -> Result<Predicate, ContractError>,
{
    Ok(match f {
        LtlFormula::True => LtlFormula::True,
        LtlFormula::False => LtlFormula::False,
        LtlFormula::Atom { atom } => LtlFormula::Atom {
            atom: resolve(atom)?,
        },
        LtlFormula::Not { arg } => LtlFormula::Not {
            arg: Box::new(resolve_ltl(arg, resolve)?),
        },
        LtlFormula::And { args } => LtlFormula::And {
            args: args
                .iter()
                .map(|a| resolve_ltl(a, resolve))
                .collect::<Result<Vec<_>, _>>()?,
        },
        LtlFormula::Or { args } => LtlFormula::Or {
            args: args
                .iter()
                .map(|a| resolve_ltl(a, resolve))
                .collect::<Result<Vec<_>, _>>()?,
        },
        LtlFormula::Implies { lhs, rhs } => LtlFormula::Implies {
            lhs: Box::new(resolve_ltl(lhs, resolve)?),
            rhs: Box::new(resolve_ltl(rhs, resolve)?),
        },
        LtlFormula::Next { arg } => LtlFormula::Next {
            arg: Box::new(resolve_ltl(arg, resolve)?),
        },
        LtlFormula::Eventually { arg } => LtlFormula::Eventually {
            arg: Box::new(resolve_ltl(arg, resolve)?),
        },
        LtlFormula::Always { arg } => LtlFormula::Always {
            arg: Box::new(resolve_ltl(arg, resolve)?),
        },
        LtlFormula::Until { lhs, rhs } => LtlFormula::Until {
            lhs: Box::new(resolve_ltl(lhs, resolve)?),
            rhs: Box::new(resolve_ltl(rhs, resolve)?),
        },
        LtlFormula::Release { lhs, rhs } => LtlFormula::Release {
            lhs: Box::new(resolve_ltl(lhs, resolve)?),
            rhs: Box::new(resolve_ltl(rhs, resolve)?),
        },
    })
}

fn resolve_fairness<F>(
    fc: &FairnessConstraint<PredicateSpec>,
    resolve: &F,
) -> Result<FairnessConstraint<Predicate>, ContractError>
where
    F: Fn(&PredicateSpec) -> Result<Predicate, ContractError>,
{
    Ok(match fc {
        FairnessConstraint::Justice { atom } => FairnessConstraint::Justice {
            atom: resolve(atom)?,
        },
        FairnessConstraint::Compassion { enabled, taken } => FairnessConstraint::Compassion {
            enabled: resolve(enabled)?,
            taken: resolve(taken)?,
        },
    })
}

fn property_id(p: &PropertySpec) -> &str {
    match p {
        PropertySpec::Safety { id, .. }
        | PropertySpec::DeadlockFree { id, .. }
        | PropertySpec::Reachability { id, .. }
        | PropertySpec::AlwaysReachable { id, .. }
        | PropertySpec::Unreachable { id, .. }
        | PropertySpec::Ctl { id, .. }
        | PropertySpec::Ltl { id, .. }
        | PropertySpec::Pattern { id, .. }
        | PropertySpec::Health { id, .. } => id,
    }
}

fn validate_assumptions(a: &Assumptions) -> Result<(), ContractError> {
    if !a.sequential_consistency {
        return Err(ContractError::Unsupported(
            "sequential_consistency = false is not modeled".into(),
        ));
    }
    if !a.no_spurious_wakeups {
        return Err(ContractError::Unsupported(
            "no_spurious_wakeups = false is not modeled".into(),
        ));
    }
    Ok(())
}

fn validate_bounds(b: &BoundsSpec) -> Result<(), ContractError> {
    for (name, v) in [
        ("max_threads", b.max_threads),
        ("max_frames_per_thread", b.max_frames_per_thread),
        ("max_states", b.max_states),
        ("max_depth", b.max_depth),
        ("max_boundary_events", b.max_boundary_events),
    ] {
        if v == 0 {
            return Err(ContractError::Invalid(format!("{name} must be >= 1")));
        }
    }
    Ok(())
}

impl PredicateSpec {
    /// Deterministic human-readable rendering of this predicate for `explain-contract`.
    pub fn explain(&self) -> String {
        match self {
            PredicateSpec::True => "true".to_string(),
            PredicateSpec::False => "false".to_string(),
            PredicateSpec::VarEq { resource, value } => format!("{resource} == {value}"),
            PredicateSpec::VarCmp {
                resource,
                op,
                value,
            } => format!("{resource} {op} {value}"),
            PredicateSpec::FunctionCompleted { function } => {
                format!("function '{function}' has completed")
            }
            PredicateSpec::FunctionCompletedAtLeast { function, n } => {
                format!("function '{function}' has completed at least {n} time(s)")
            }
            PredicateSpec::ScopeCompleted { function, sid } => {
                format!("scope at '{function}::{sid}' has completed")
            }
            PredicateSpec::StatementReached { function, sid } => {
                format!("statement '{function}::{sid}' is reached")
            }
            PredicateSpec::MutexFree { resource } => format!("lock '{resource}' is free"),
            PredicateSpec::MutexHeld { resource } => format!("lock '{resource}' is held"),
            PredicateSpec::HoldsAll {
                function,
                resources,
            } => format!(
                "a thread in '{function}' holds all of [{}]",
                resources.join(", ")
            ),
            PredicateSpec::MutexExclusive { resource } => {
                format!("lock '{resource}' maintains mutual exclusion")
            }
            PredicateSpec::NeverHoldsAll {
                function,
                resources,
            } => format!(
                "no thread in '{function}' simultaneously holds all of [{}]",
                resources.join(", ")
            ),
            PredicateSpec::ChannelEmpty { resource } => format!("channel '{resource}' is empty"),
            PredicateSpec::ChannelAtLeast { resource, len } => {
                format!("channel '{resource}' has at least {len} buffered item(s)")
            }
            PredicateSpec::Not { predicate } => format!("not ({})", predicate.explain()),
            PredicateSpec::And { predicates } => {
                if predicates.is_empty() {
                    "true".to_string()
                } else {
                    let parts: Vec<String> = predicates.iter().map(PredicateSpec::explain).collect();
                    format!("({})", parts.join(" and "))
                }
            }
            PredicateSpec::Or { predicates } => {
                if predicates.is_empty() {
                    "false".to_string()
                } else {
                    let parts: Vec<String> = predicates.iter().map(PredicateSpec::explain).collect();
                    format!("({})", parts.join(" or "))
                }
            }
        }
    }

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
                let rid = resolve_resource(program, default_module, resource)?;
                expect_kind(program, rid, resource, &[ResKind::Mutex, ResKind::RwLock])?;
                Predicate::MutexFree(rid)
            }
            PredicateSpec::MutexHeld { resource } => {
                let rid = resolve_resource(program, default_module, resource)?;
                expect_kind(program, rid, resource, &[ResKind::Mutex, ResKind::RwLock])?;
                Predicate::MutexHeld(rid)
            }
            PredicateSpec::HoldsAll {
                function,
                resources,
            } => {
                let fid = resolve_function(program, default_module, function)?;
                let mut rids = Vec::new();
                for name in resources {
                    let rid = resolve_resource(program, default_module, name)?;
                    expect_kind(
                        program,
                        rid,
                        name,
                        &[ResKind::Mutex, ResKind::RwLock, ResKind::Semaphore],
                    )?;
                    rids.push(rid);
                }
                Predicate::HoldsAll {
                    func: fid,
                    resources: rids,
                }
            }
            PredicateSpec::MutexExclusive { resource } => {
                let rid = resolve_resource(program, default_module, resource)?;
                expect_kind(
                    program,
                    rid,
                    resource,
                    &[ResKind::Mutex, ResKind::RwLock, ResKind::Semaphore],
                )?;
                Predicate::MutexExclusive(rid)
            }
            PredicateSpec::NeverHoldsAll {
                function,
                resources,
            } => {
                let fid = resolve_function(program, default_module, function)?;
                let mut rids = Vec::new();
                for name in resources {
                    let rid = resolve_resource(program, default_module, name)?;
                    expect_kind(
                        program,
                        rid,
                        name,
                        &[ResKind::Mutex, ResKind::RwLock, ResKind::Semaphore],
                    )?;
                    rids.push(rid);
                }
                Predicate::NeverHoldsAll {
                    func: fid,
                    resources: rids,
                }
            }
            PredicateSpec::ChannelEmpty { resource } => {
                let rid = resolve_resource(program, default_module, resource)?;
                expect_kind(program, rid, resource, &[ResKind::Channel])?;
                Predicate::ChannelEmpty(rid)
            }
            PredicateSpec::ChannelAtLeast { resource, len } => {
                let rid = resolve_resource(program, default_module, resource)?;
                expect_kind(program, rid, resource, &[ResKind::Channel])?;
                Predicate::ChannelAtLeast {
                    resource: rid,
                    len: *len,
                }
            }
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

fn expect_kind(
    program: &SemProgram,
    resource: ResourceId,
    name: &str,
    allowed: &[ResKind],
) -> Result<(), String> {
    let r = program.resource(resource);
    if allowed.contains(&r.kind) {
        Ok(())
    } else {
        Err(format!(
            "resource '{name}' has an incompatible kind for this predicate"
        ))
    }
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
