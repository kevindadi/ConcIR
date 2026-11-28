//! Verification contract: properties, preserved behaviour, assumptions, and
//! the allowed patch scope. A contract is built from data and is never
//! modified by a candidate patch.

use serde::{Deserialize, Serialize};
use unipn::analysis::obligations::BugKind;
use unipn::analysis::temporal::{
    explain_ctl, explain_ltl, CtlFormula, FairnessConstraint, LtlFormula,
};

use super::patterns::{CompiledPattern, ConstraintPattern};

use concir_ir::expr::CmpOp;
use concir_sem::ids::{FunctionId, ResourceId};
use concir_sem::program::{ResKind, SemProgram};
use concir_sem::system::Predicate;
use concir_sem::value::{from_json, Value};

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
    MilestoneReached {
        milestone: String,
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
    pub bounds: concir_sem::outcome::AnalysisBounds,
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

impl From<&BoundsSpec> for concir_sem::outcome::AnalysisBounds {
    fn from(b: &BoundsSpec) -> Self {
        concir_sem::outcome::AnalysisBounds {
            max_threads: b.max_threads,
            max_frames_per_thread: b.max_frames_per_thread,
            max_states: b.max_states,
            max_depth: b.max_depth,
            max_boundary_events: b.max_boundary_events,
        }
    }
}

