mod diagnostics;
mod properties;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::Hash;

use serde::{Deserialize, Serialize};
use unipn::analysis::obligations::{check_obligations_graph, BugKind, ObligationStateView};
use unipn::net::PlaceId;

use concir_engine::graph::{explore_system, ExploreOptions, StateGraph};
use concir_ir::fqn;
use concir_sem::ids::{FunctionId, ResourceId, ThreadId};
use concir_sem::outcome::{
    BackendError, BoundaryEvent, Invalid, Outcome, StepLabel, Unsupported,
};
use concir_sem::program::{ResKind, SemOp, SemProgram};
use concir_sem::system::{
    BlockedRecord, BlockKind, DoomState, DoomThread, InstanceState, Predicate, Step,
    TransitionSystem,
};

pub use crate::contract;
pub use crate::contract::patterns;
use crate::contract::{ContractError, ContractSpec, Preserved, Property, VerificationContract};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CirStatementRef {
    pub module: String,
    pub function: String,
    pub sid: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticRecord {
    pub property: String,
    pub outcome: Outcome,
    pub message: String,
    pub complete: bool,
    pub counterexample: Vec<StepLabel>,
    pub final_instances: Vec<InstanceState>,
    pub blocked: Vec<BlockedRecord>,
    pub cir_statements: Vec<CirStatementRef>,
    /// Facts the search established (kept separate from hints).
    pub proven_facts: Vec<String>,
    /// Heuristic repair suggestions (never treated as proven).
    pub repair_hints: Vec<String>,
    /// Name-rendered counterexample steps (`module::function::sid`).
    #[serde(default)]
    pub counterexample_names: Vec<String>,
    /// Who holds/waits on what in the counterexample state.
    #[serde(default)]
    pub doom_state: DoomState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyResult {
    pub id: String,
    pub outcome: Outcome,
    pub detail: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub vacuous: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vacuous_antecedents: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
}

impl PropertyResult {
    fn simple(id: impl Into<String>, outcome: Outcome, detail: impl Into<String>) -> Self {
        PropertyResult {
            id: id.into(),
            outcome,
            detail: detail.into(),
            vacuous: false,
            vacuous_antecedents: Vec::new(),
            explanation: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationReport {
    pub outcome: Outcome,
    pub complete: bool,
    pub states_explored: usize,
    pub transitions_explored: usize,
    pub properties: Vec<PropertyResult>,
    pub diagnostics: Vec<DiagnosticRecord>,
    pub boundary_events: Vec<BoundaryEvent>,
    pub unsupported: Vec<Unsupported>,
    pub invalid: Vec<Invalid>,
    /// Hash of the program model actually analyzed.
    pub model_fingerprint: String,
    /// Hash of the symbolic contract document.
    pub contract_fingerprint: String,
    /// Semantic assumptions actually used.
    pub assumptions: crate::contract::Assumptions,
    /// Analysis bounds actually used (or requested, if analysis did not run).
    pub bounds: concir_sem::outcome::AnalysisBounds,
    /// False when the analysis never started (rejected config/invalid input),
    /// so the assumptions/bounds above are the *requested* values only.
    pub analysis_started: bool,
}

impl VerificationReport {
    #[allow(dead_code)]
    fn failed(&self) -> bool {
        self.properties.iter().any(|p| p.outcome == Outcome::Fail)
    }
}

pub struct Reachability<S> {
    pub states: Vec<S>,
    pub edges: Vec<Vec<(usize, StepLabel)>>,
    pub boundary_events: Vec<BoundaryEvent>,
    pub unsupported: Vec<Unsupported>,
    pub invalid: Vec<Invalid>,
    pub truncated: bool,
}

impl<S: Clone + Eq + Hash> Reachability<S> {
    pub fn index_of(&self, state: &S) -> Option<usize> {
        self.states.iter().position(|s| s == state)
    }
}

/// Deterministically explore the reachable state graph up to the bounds.

/// Deterministically explore the reachable state graph up to the bounds.
pub fn explore<S: TransitionSystem>(
    system: &S,
    bounds: &concir_sem::outcome::AnalysisBounds,
) -> Reachability<S::State> {
    let out = explore_system(system, bounds, ExploreOptions { stop_on_boundary_limit: true });
    Reachability {
        states: out.graph.states,
        edges: out.graph.edges,
        boundary_events: out.boundary_events,
        unsupported: out.unsupported,
        invalid: out.invalid,
        truncated: out.truncated,
    }
}

/// Verify a program against a fixed contract with the given engine.
pub fn verify<S: TransitionSystem>(
    system: &S,
    contract: &VerificationContract,
) -> VerificationReport {
    let bounds = &contract.bounds;
    let program = system.program();

    let out = explore_system(system, bounds, ExploreOptions { stop_on_boundary_limit: false });
    if let Some(err) = out.init_error {
        let outcome = match err {
            BackendError::Unsupported(_) => Outcome::Unsupported,
            BackendError::Invalid(_) => Outcome::Invalid,
        };
        let mut r = diagnostics::report_early(outcome, out.unsupported, out.invalid, out.boundary_events);
        r.model_fingerprint = model_fingerprint(program);
        r.contract_fingerprint = contract_fingerprint(contract);
        r.assumptions = contract.assumptions.clone();
        r.bounds = contract.bounds.clone();
        return r;
    }

    let graph = out.graph;
    let truncated = out.truncated;
    let unsupported = out.unsupported;
    let invalid = out.invalid;
    let boundary_events = out.boundary_events;
    let transitions_explored = out.transitions_explored;

    let complete =
        !truncated && boundary_events.is_empty() && unsupported.is_empty() && invalid.is_empty();

    let mut diagnostics = Vec::new();
    let mut results = Vec::new();

    for prop in &contract.properties {
        let result = properties::check_property(
            system,
            program,
            &graph,
            &prop.id,
            &prop.property,
            complete,
            &mut diagnostics,
        );
        results.push(result);
    }

    // Preserved behaviour is checked as part of the contract: a violation is a
    // contract failure, reported under a synthetic property id.
    for preserved in &contract.preserved {
        let pid = format!("preserved: {}", preserved.description);
        match &preserved.behavior {
            Preserved::Reachable(goal) => {
                let found = graph.states.iter().any(|s| system.satisfied(s, goal));
                let outcome = if found {
                    Outcome::Pass
                } else if complete {
                    Outcome::Fail
                } else {
                    Outcome::Unknown
                };
                if outcome == Outcome::Fail {
                    let mut hints = vec![
                        "restore the statement/thread that established this behaviour".to_string(),
                    ];
                    if let Some(hint) = diagnostics::holds_all_hint(program, goal) {
                        hints.push(hint);
                    }
                    diagnostics.push(DiagnosticRecord {
                        property: pid.clone(),
                        outcome,
                        message: format!(
                            "required behaviour '{}' is no longer reachable",
                            preserved.description
                        ),
                        complete,
                        counterexample: Vec::new(),
                        final_instances: Vec::new(),
                        blocked: Vec::new(),
                        cir_statements: Vec::new(),
                        proven_facts: vec![format!(
                            "explored {} reachable states",
                            graph.states.len()
                        )],
                        repair_hints: hints,
                        counterexample_names: Vec::new(),
                        doom_state: Default::default(),
                    });
                }
                results.push(PropertyResult::simple(
                    pid,
                    outcome,
                    format!("preserved reachability: {}", goal.description()),
                ));
            }
            Preserved::Always(invariant) => {
                let violated = graph
                    .states
                    .iter()
                    .position(|s| !system.satisfied(s, invariant));
                let outcome = match violated {
                    Some(_) => Outcome::Fail,
                    None if complete => Outcome::Pass,
                    None => Outcome::Unknown,
                };
                if let Some(idx) = violated {
                    diagnostics.push(diagnostics::make_diagnostic(
                        system,
                        program,
                        &graph,
                        &pid,
                        Outcome::Fail,
                        "preserved invariant violated",
                        idx,
                        complete,
                    ));
                }
                results.push(PropertyResult::simple(
                    pid,
                    outcome,
                    format!("preserved invariant: {}", invariant.description()),
                ));
            }
        }
    }

    let mut outcome = Outcome::Pass;
    if !invalid.is_empty() {
        outcome = Outcome::Invalid;
    } else if !unsupported.is_empty() {
        outcome = Outcome::Unsupported;
    } else if results.iter().any(|p| p.outcome == Outcome::Fail) {
        outcome = Outcome::Fail;
    } else if results.iter().any(|p| p.outcome == Outcome::Unknown) || !complete {
        outcome = Outcome::Unknown;
    }

    let _ = transitions_explored;
    VerificationReport {
        outcome,
        complete,
        states_explored: graph.states.len(),
        transitions_explored,
        properties: results,
        diagnostics,
        boundary_events,
        unsupported,
        invalid,
        model_fingerprint: model_fingerprint(program),
        contract_fingerprint: contract_fingerprint(contract),
        assumptions: contract.assumptions.clone(),
        bounds: contract.bounds.clone(),
        analysis_started: true,
    }
}


pub fn reachable_canonical<S: TransitionSystem>(
    system: &S,
    bounds: &concir_sem::outcome::AnalysisBounds,
) -> (Vec<String>, Vec<BoundaryEvent>) {
    let r = explore(system, bounds);
    let mut out: Vec<String> = r.states.iter().map(|s| system.canonical(s)).collect();
    out.sort();
    out.dedup();
    (out, r.boundary_events)
}

// ─────────────────── Unified checked verification entry ───────────────────

/// Which engine to run. Both are checked implementations; they must agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    Interpreter,
    Petri,
}

fn fnv(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn model_fingerprint(program: &SemProgram) -> String {
    let mut out = String::new();
    for m in program.functions() {
        out.push_str(&format!(
            "{}::{}:{};",
            program.module_name(m.module),
            m.name,
            m.body.len()
        ));
    }
    for r in program.resources() {
        out.push_str(&format!(
            "{}::{}/{:?};",
            program.module_name(r.module),
            r.name,
            r.kind
        ));
    }
    fnv(&out)
}

fn contract_fingerprint(contract: &VerificationContract) -> String {
    match serde_json::to_string(&contract.source) {
        Ok(s) => fnv(&s),
        Err(_) => String::new(),
    }
}

impl VerificationReport {
    fn synthetic(outcome: Outcome, complete: bool) -> Self {
        VerificationReport {
            outcome,
            complete,
            states_explored: 0,
            transitions_explored: 0,
            properties: Vec::new(),
            diagnostics: Vec::new(),
            boundary_events: Vec::new(),
            unsupported: Vec::new(),
            invalid: Vec::new(),
            model_fingerprint: String::new(),
            contract_fingerprint: String::new(),
            assumptions: Default::default(),
            bounds: concir_sem::outcome::AnalysisBounds::default(),
            analysis_started: false,
        }
    }
}

/// The single checked entry point: static validation, semantic supportability,
/// contract validation and rebinding, monitor derivation, exploration, and
/// property checking. CLI and repair reuse this.
pub fn verify_program(
    program: &concir_ir::ast::Program,
    spec: &ContractSpec,
    engine: EngineKind,
) -> VerificationReport {
    let owned;
    let program = if program.has_structured_bodies() {
        let mut lowered = program.clone();
        if let Err(msg) = lowered.ensure_lowered() {
            let mut report = VerificationReport::synthetic(Outcome::Invalid, true);
            report.invalid.push(Invalid::new("E011", msg));
            report.assumptions = spec.assumptions.clone();
            report.bounds = (&spec.bounds).into();
            return report;
        }
        owned = lowered;
        &owned
    } else {
        program
    };

    let program_fp = fnv(&serde_json::to_string(program).unwrap_or_default());
    let contract_fp = fnv(&serde_json::to_string(spec).unwrap_or_default());

    let finish_meta = |mut report: VerificationReport| {
        report.model_fingerprint = program_fp.clone();
        report.contract_fingerprint = contract_fp.clone();
        // For early exits the analysis never ran; record the *requested*
        // configuration rather than a default that was never used.
        report.assumptions = spec.assumptions.clone();
        report.bounds = (&spec.bounds).into();
        report
    };

    // 1. Static validation.
    let static_report = concir_ir::validate::validate(program);
    if !static_report.valid {
        let mut report = VerificationReport::synthetic(Outcome::Invalid, true);
        report.invalid = static_report
            .diagnostics
            .iter()
            .filter(|d| d.severity == concir_ir::diagnostic::Severity::Error)
            .map(|d| {
                let inv = Invalid::new(d.code, d.message.clone());
                match &d.location {
                    Some(loc) => inv.at(loc),
                    None => inv,
                }
            })
            .collect();
        return finish_meta(report);
    }

    // 2. Lowering.
    let sem = match concir_sem::program::lower(program) {
        Ok(s) => s,
        Err(e) => {
            let mut report = VerificationReport::synthetic(Outcome::Invalid, true);
            let inv = Invalid::new("E100", e.to_string());
            report.invalid.push(match e.location() {
                Some(l) => inv.at(l),
                None => inv,
            });
            return finish_meta(report);
        }
    };

    // 3. Supportability of the program (including the entry).
    let mut unsupported = sem.unsupported().to_vec();
    let entry = sem.entry();
    let ef = sem.function(entry);
    if ef.is_nobody && !ef.is_transparent_nobody() {
        unsupported.push(Unsupported::new(
            "external entry function",
            "entry is body-less with declared effects, blocking, or a return",
        ));
    }
    if !unsupported.is_empty() {
        let mut report = VerificationReport::synthetic(Outcome::Unsupported, false);
        report.unsupported = unsupported;
        return finish_meta(report);
    }

    // 4. Contract validation and binding.
    let contract = match spec.resolve(&sem) {
        Ok(c) => c,
        Err(ContractError::Unsupported(m)) => {
            let mut report = VerificationReport::synthetic(Outcome::Unsupported, false);
            report.unsupported.push(Unsupported::new("contract", m));
            return finish_meta(report);
        }
        Err(ContractError::Invalid(m)) => {
            let mut report = VerificationReport::synthetic(Outcome::Invalid, true);
            report.invalid.push(Invalid::new("C001", m));
            return finish_meta(report);
        }
    };

    // 5. Monitors, exploration, properties.
    let monitor = concir_sem::monitor::MonitorConfig::from_predicates(contract.predicates());
    let report = match engine {
        EngineKind::Interpreter => {
            let e =
                concir_engine::interp::Interpreter::with_monitor(&sem, contract.bounds.clone(), monitor);
            verify(&e, &contract)
        }
        EngineKind::Petri => {
            let e = concir_engine::hln::UnipnEngine::with_monitor(&sem, contract.bounds.clone(), monitor);
            verify(&e, &contract)
        }
    };
    finish_meta(report)
}
