//! Deterministic finite-state exploration, property checking, and structured
//! diagnostics.
//!
//! The explorer is generic over a [`TransitionSystem`]; both the reference
//! interpreter and the Petri-net executor are explored with exactly this code.

pub mod contract;

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;

use serde::{Deserialize, Serialize};

use crate::sem::ids::FunctionId;
use crate::sem::outcome::{
    BackendError, BoundaryEvent, BoundaryKind, Invalid, Outcome, StepLabel, Unsupported,
};
use crate::sem::program::SemProgram;
use crate::sem::system::{BlockedRecord, InstanceState, Step, TransitionSystem};

use contract::{ContractError, ContractSpec, Preserved, Property, VerificationContract};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyResult {
    pub id: String,
    pub outcome: Outcome,
    pub detail: String,
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
    pub assumptions: crate::explore::contract::Assumptions,
    /// Analysis bounds actually used (or requested, if analysis did not run).
    pub bounds: crate::sem::outcome::AnalysisBounds,
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

struct Graph<S> {
    states: Vec<S>,
    /// Dedup key: the engine's fully identity-normalized canonical state, so
    /// states differing only by fresh thread/frame/handle numbering are one.
    index: HashMap<String, usize>,
    edges: Vec<Vec<(usize, StepLabel)>>,
    pred: Vec<Option<(usize, StepLabel)>>,
    had_boundary: Vec<bool>,
    depth: Vec<usize>,
    processed: Vec<bool>,
}

/// A reachable-state exploration result, exposed for tests and repair.
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
pub fn explore<S: TransitionSystem>(
    system: &S,
    bounds: &crate::sem::outcome::AnalysisBounds,
) -> Reachability<S::State> {
    let mut graph = Graph {
        states: Vec::new(),
        index: HashMap::new(),
        edges: Vec::new(),
        pred: Vec::new(),
        had_boundary: Vec::new(),
        depth: Vec::new(),
        processed: Vec::new(),
    };
    let mut unsupported = Vec::new();
    let mut invalid = Vec::new();
    let mut boundary_events = Vec::new();
    let mut truncated = false;

    let Ok(init) = system.initial() else {
        return Reachability {
            states: Vec::new(),
            edges: Vec::new(),
            boundary_events,
            unsupported,
            invalid,
            truncated,
        };
    };
    let mut queue = VecDeque::new();
    graph.index.insert(system.state_key(&init), 0);
    graph.states.push(init.clone());
    graph.edges.push(Vec::new());
    graph.pred.push(None);
    graph.had_boundary.push(false);
    graph.depth.push(0);
    graph.processed.push(false);
    queue.push_back(0usize);

    while let Some(idx) = queue.pop_front() {
        if graph.states.len() >= bounds.max_states {
            truncated = true;
            boundary_events.push(BoundaryEvent::new(
                BoundaryKind::StateLimit,
                format!("state limit {} reached", bounds.max_states),
            ));
            break;
        }
        graph.processed[idx] = true;
        let depth = graph.depth[idx];
        if depth >= bounds.max_depth {
            truncated = true;
            graph.had_boundary[idx] = true;
            boundary_events.push(BoundaryEvent::new(
                BoundaryKind::DepthLimit,
                format!("depth limit {} reached", bounds.max_depth),
            ));
            if boundary_events.len() > bounds.max_boundary_events {
                break;
            }
            continue;
        }
        let state = graph.states[idx].clone();
        match system.successors(&state) {
            Ok(enabled) => {
                if !enabled.boundary.is_empty() {
                    graph.had_boundary[idx] = true;
                    for b in &enabled.boundary {
                        boundary_events.push(b.clone());
                    }
                }
                for Step { label, state: succ } in enabled.steps {
                    let key = system.state_key(&succ);
                    if let Some(&target) = graph.index.get(&key) {
                        graph.edges[idx].push((target, label));
                    } else {
                        let target = graph.states.len();
                        graph.index.insert(key, target);
                        graph.states.push(succ);
                        graph.edges.push(Vec::new());
                        graph.edges[idx].push((target, label.clone()));
                        graph.pred.push(Some((idx, label)));
                        graph.had_boundary.push(false);
                        graph.depth.push(depth + 1);
                        graph.processed.push(false);
                        queue.push_back(target);
                    }
                }
            }
            Err(BackendError::Unsupported(u)) => {
                graph.had_boundary[idx] = true;
                unsupported.push(u);
            }
            Err(BackendError::Invalid(i)) => {
                graph.had_boundary[idx] = true;
                invalid.push(i);
            }
        }
        if boundary_events.len() > bounds.max_boundary_events {
            truncated = true;
            break;
        }
    }

    Reachability {
        states: graph.states,
        edges: graph.edges,
        boundary_events,
        unsupported,
        invalid,
        truncated,
    }
}

/// Verify a program against a fixed contract with the given engine.
pub fn verify<S: TransitionSystem>(
    system: &S,
    contract: &VerificationContract,
) -> VerificationReport {
    let bounds = &contract.bounds;
    let program = system.program();

    let mut graph = Graph {
        states: Vec::new(),
        index: HashMap::new(),
        edges: Vec::new(),
        pred: Vec::new(),
        had_boundary: Vec::new(),
        depth: Vec::new(),
        processed: Vec::new(),
    };
    let mut unsupported = Vec::new();
    let mut invalid = Vec::new();
    let mut boundary_events = Vec::new();
    let mut truncated = false;
    let mut transitions_explored = 0usize;

    let init = match system.initial() {
        Ok(s) => s,
        Err(BackendError::Unsupported(u)) => {
            unsupported.push(u);
            let mut r = report_early(Outcome::Unsupported, unsupported, invalid, boundary_events);
            r.model_fingerprint = model_fingerprint(program);
            r.contract_fingerprint = contract_fingerprint(contract);
            r.assumptions = contract.assumptions.clone();
            r.bounds = contract.bounds.clone();
            return r;
        }
        Err(BackendError::Invalid(i)) => {
            invalid.push(i);
            let mut r = report_early(Outcome::Invalid, unsupported, invalid, boundary_events);
            r.model_fingerprint = model_fingerprint(program);
            r.contract_fingerprint = contract_fingerprint(contract);
            r.assumptions = contract.assumptions.clone();
            r.bounds = contract.bounds.clone();
            return r;
        }
    };

    graph.index.insert(system.state_key(&init), 0);
    graph.states.push(init);
    graph.edges.push(Vec::new());
    graph.pred.push(None);
    graph.had_boundary.push(false);
    graph.depth.push(0);
    graph.processed.push(false);
    let mut queue = VecDeque::new();
    queue.push_back(0usize);

    while let Some(idx) = queue.pop_front() {
        if graph.states.len() >= bounds.max_states {
            truncated = true;
            boundary_events.push(BoundaryEvent::new(
                BoundaryKind::StateLimit,
                format!("state limit {} reached", bounds.max_states),
            ));
            break;
        }
        graph.processed[idx] = true;
        let depth = graph.depth[idx];
        if depth >= bounds.max_depth {
            truncated = true;
            graph.had_boundary[idx] = true;
            boundary_events.push(BoundaryEvent::new(
                BoundaryKind::DepthLimit,
                format!("depth limit {} reached", bounds.max_depth),
            ));
            continue;
        }
        let state = graph.states[idx].clone();
        match system.successors(&state) {
            Ok(enabled) => {
                if !enabled.boundary.is_empty() {
                    graph.had_boundary[idx] = true;
                    for b in &enabled.boundary {
                        boundary_events.push(b.clone());
                    }
                }
                for Step { label, state: succ } in enabled.steps {
                    transitions_explored += 1;
                    let key = system.state_key(&succ);
                    if let Some(&target) = graph.index.get(&key) {
                        graph.edges[idx].push((target, label));
                    } else {
                        let target = graph.states.len();
                        graph.index.insert(key, target);
                        graph.states.push(succ);
                        graph.edges.push(Vec::new());
                        graph.edges[idx].push((target, label.clone()));
                        graph.pred.push(Some((idx, label)));
                        graph.had_boundary.push(false);
                        graph.depth.push(depth + 1);
                        graph.processed.push(false);
                        queue.push_back(target);
                    }
                }
            }
            Err(BackendError::Unsupported(u)) => {
                graph.had_boundary[idx] = true;
                unsupported.push(u);
            }
            Err(BackendError::Invalid(i)) => {
                graph.had_boundary[idx] = true;
                invalid.push(i);
            }
        }
        if boundary_events.len() > bounds.max_boundary_events {
            truncated = true;
            break;
        }
    }

    let complete =
        !truncated && boundary_events.is_empty() && unsupported.is_empty() && invalid.is_empty();

    let mut diagnostics = Vec::new();
    let mut results = Vec::new();

    for prop in &contract.properties {
        let result = check_property(
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
                        repair_hints: vec![
                            "restore the statement/thread that established this behaviour".into(),
                        ],
                    });
                }
                results.push(PropertyResult {
                    id: pid,
                    outcome,
                    detail: format!("preserved reachability: {}", goal.description()),
                });
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
                    diagnostics.push(make_diagnostic(
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
                results.push(PropertyResult {
                    id: pid,
                    outcome,
                    detail: format!("preserved invariant: {}", invariant.description()),
                });
            }
        }
    }

    let mut outcome = Outcome::Pass;
    if results.iter().any(|p| p.outcome == Outcome::Fail) {
        outcome = Outcome::Fail;
    } else if !invalid.is_empty() {
        outcome = Outcome::Invalid;
    } else if !unsupported.is_empty() {
        outcome = Outcome::Unsupported;
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

fn report_early(
    outcome: Outcome,
    unsupported: Vec<Unsupported>,
    invalid: Vec<Invalid>,
    boundary_events: Vec<BoundaryEvent>,
) -> VerificationReport {
    VerificationReport {
        outcome,
        complete: false,
        states_explored: 0,
        transitions_explored: 0,
        properties: Vec::new(),
        diagnostics: Vec::new(),
        boundary_events,
        unsupported,
        invalid,
        model_fingerprint: String::new(),
        contract_fingerprint: String::new(),
        assumptions: Default::default(),
        bounds: crate::sem::outcome::AnalysisBounds::default(),
        analysis_started: false,
    }
}

#[allow(clippy::too_many_arguments)]
fn check_property<S: TransitionSystem>(
    system: &S,
    program: &SemProgram,
    graph: &Graph<S::State>,
    id: &str,
    property: &Property,
    complete: bool,
    diagnostics: &mut Vec<DiagnosticRecord>,
) -> PropertyResult {
    match property {
        Property::Safety { invariant } => {
            let found = graph
                .states
                .iter()
                .position(|s| !system.satisfied(s, invariant));
            match found {
                Some(idx) => {
                    diagnostics.push(make_diagnostic(
                        system,
                        program,
                        graph,
                        id,
                        Outcome::Fail,
                        "safety invariant violated",
                        idx,
                        complete,
                    ));
                    PropertyResult {
                        id: id.into(),
                        outcome: Outcome::Fail,
                        detail: "counterexample reached a violating state".into(),
                    }
                }
                None => PropertyResult {
                    id: id.into(),
                    outcome: if complete {
                        Outcome::Pass
                    } else {
                        Outcome::Unknown
                    },
                    detail: if complete {
                        "invariant held in all reachable states".into()
                    } else {
                        "no violation found, but the search was incomplete".into()
                    },
                },
            }
        }
        Property::Unreachable { bad } => {
            let found = graph.states.iter().position(|s| system.satisfied(s, bad));
            match found {
                Some(idx) => {
                    diagnostics.push(make_diagnostic(
                        system,
                        program,
                        graph,
                        id,
                        Outcome::Fail,
                        "forbidden state reached",
                        idx,
                        complete,
                    ));
                    PropertyResult {
                        id: id.into(),
                        outcome: Outcome::Fail,
                        detail: "counterexample reached a forbidden state".into(),
                    }
                }
                None => PropertyResult {
                    id: id.into(),
                    outcome: if complete {
                        Outcome::Pass
                    } else {
                        Outcome::Unknown
                    },
                    detail: if complete {
                        "forbidden state is unreachable".into()
                    } else {
                        "not found, but the search was incomplete".into()
                    },
                },
            }
        }
        Property::DeadlockFree => {
            let deadlock = graph.states.iter().enumerate().position(|(i, s)| {
                graph.processed[i]
                    && !graph.had_boundary[i]
                    && graph.edges[i].is_empty()
                    && !system.is_finished(s)
            });
            match deadlock {
                Some(idx) => {
                    let mut d = make_diagnostic(
                        system,
                        program,
                        graph,
                        id,
                        Outcome::Fail,
                        "reachable global deadlock",
                        idx,
                        complete,
                    );
                    d.repair_hints.push(
                        "check lock acquisition order, channel capacity, and missing notify/join"
                            .into(),
                    );
                    diagnostics.push(d);
                    PropertyResult {
                        id: id.into(),
                        outcome: Outcome::Fail,
                        detail: "a reachable state has no enabled step and unfinished threads"
                            .into(),
                    }
                }
                None => PropertyResult {
                    id: id.into(),
                    outcome: if complete {
                        Outcome::Pass
                    } else {
                        Outcome::Unknown
                    },
                    detail: if complete {
                        "no deadlock state reachable".into()
                    } else {
                        "no deadlock found, but the search was incomplete".into()
                    },
                },
            }
        }
        Property::Reachability { goal } => {
            let found = graph.states.iter().position(|s| system.satisfied(s, goal));
            match found {
                Some(idx) => {
                    let ce = counterexample(graph, idx);
                    PropertyResult {
                        id: id.into(),
                        outcome: Outcome::Pass,
                        detail: format!(
                            "goal reachable via {} step(s) ({})",
                            ce.len(),
                            goal.description()
                        ),
                    }
                }
                None => {
                    let outcome = if complete {
                        Outcome::Fail
                    } else {
                        Outcome::Unknown
                    };
                    if outcome == Outcome::Fail {
                        diagnostics.push(DiagnosticRecord {
                            property: id.into(),
                            outcome,
                            message: format!("goal '{}' is unreachable", goal.description()),
                            complete,
                            counterexample: Vec::new(),
                            final_instances: Vec::new(),
                            blocked: Vec::new(),
                            cir_statements: Vec::new(),
                            proven_facts: vec![format!(
                                "exhaustively explored {} reachable states",
                                graph.states.len()
                            )],
                            repair_hints: vec![
                                "a goal that is unreachable cannot be fixed by reordering; check whether the goal is ever produced".into(),
                            ],
                        });
                    }
                    PropertyResult {
                        id: id.into(),
                        outcome,
                        detail: if outcome == Outcome::Fail {
                            format!(
                                "goal '{}' is not reachable in any execution",
                                goal.description()
                            )
                        } else {
                            format!("goal '{}' not found; search incomplete", goal.description())
                        },
                    }
                }
            }
        }
        Property::AlwaysReachable { goal } => {
            let goal_states: Vec<usize> = graph
                .states
                .iter()
                .enumerate()
                .filter(|(_, s)| system.satisfied(s, goal))
                .map(|(i, _)| i)
                .collect();
            if goal_states.is_empty() {
                diagnostics.push(DiagnosticRecord {
                    property: id.into(),
                    outcome: Outcome::Fail,
                    message: format!(
                        "goal '{}' is unreachable, so AG EF fails",
                        goal.description()
                    ),
                    complete,
                    counterexample: Vec::new(),
                    final_instances: Vec::new(),
                    blocked: Vec::new(),
                    cir_statements: Vec::new(),
                    proven_facts: vec![format!("explored {} reachable states", graph.states.len())],
                    repair_hints: vec!["make the goal reachable before requiring AG EF".into()],
                });
                return PropertyResult {
                    id: id.into(),
                    outcome: Outcome::Fail,
                    detail: "goal never reachable".into(),
                };
            }
            let can_reach = reverse_reachable(graph, &goal_states);
            let bad = (0..graph.states.len()).find(|i| !can_reach.contains(i));
            match bad {
                Some(idx) => {
                    let mut d = make_diagnostic(
                        system,
                        program,
                        graph,
                        id,
                        Outcome::Fail,
                        "reachable state that can no longer reach the goal",
                        idx,
                        complete,
                    );
                    d.repair_hints.push(
                        "ensure every branch can still reach the goal (avoid a terminal state that blocks it)"
                            .into(),
                    );
                    diagnostics.push(d);
                    PropertyResult {
                        id: id.into(),
                        outcome: Outcome::Fail,
                        detail: "AG EF violated: a reachable state cannot reach the goal".into(),
                    }
                }
                None => PropertyResult {
                    id: id.into(),
                    outcome: if complete {
                        Outcome::Pass
                    } else {
                        Outcome::Unknown
                    },
                    detail: if complete {
                        "every reachable state can reach the goal".into()
                    } else {
                        "AG EF held on the explored fragment, but the search was incomplete".into()
                    },
                },
            }
        }
    }
}

fn reverse_reachable<S>(graph: &Graph<S>, roots: &[usize]) -> HashSet<usize> {
    let mut rev: Vec<Vec<usize>> = vec![Vec::new(); graph.states.len()];
    for (i, edges) in graph.edges.iter().enumerate() {
        for (target, _) in edges {
            rev[*target].push(i);
        }
    }
    let mut seen: HashSet<usize> = roots.iter().copied().collect();
    let mut stack: Vec<usize> = roots.to_vec();
    while let Some(i) = stack.pop() {
        for &p in &rev[i] {
            if seen.insert(p) {
                stack.push(p);
            }
        }
    }
    seen
}

fn counterexample<S>(graph: &Graph<S>, idx: usize) -> Vec<StepLabel> {
    let mut labels = Vec::new();
    let mut cur = idx;
    while let Some((parent, label)) = &graph.pred[cur] {
        labels.push(label.clone());
        cur = *parent;
    }
    labels.reverse();
    labels
}

#[allow(clippy::too_many_arguments)]
fn make_diagnostic<S: TransitionSystem>(
    system: &S,
    program: &SemProgram,
    graph: &Graph<S::State>,
    id: &str,
    outcome: Outcome,
    message: &str,
    idx: usize,
    complete: bool,
) -> DiagnosticRecord {
    let state = &graph.states[idx];
    let instances = system.instances(state);
    let blocked = system.blocked(state);
    let cir_statements = instances
        .iter()
        .map(|i| cir_ref(program, i.function, i.sid))
        .collect();
    DiagnosticRecord {
        property: id.into(),
        outcome,
        message: message.into(),
        complete,
        counterexample: counterexample(graph, idx),
        final_instances: instances,
        blocked,
        cir_statements,
        proven_facts: vec![
            format!(
                "reached a counterexample state after {} step(s)",
                graph.depth[idx]
            ),
            format!("explored {} reachable states", graph.states.len()),
        ],
        repair_hints: Vec::new(),
    }
}

fn cir_ref(program: &SemProgram, function: FunctionId, sid: Option<usize>) -> CirStatementRef {
    let f = program.function(function);
    CirStatementRef {
        module: program.module_name(f.module).to_string(),
        function: f.name.clone(),
        sid: sid.and_then(|i| f.body.get(i).map(|s| s.sid.clone())),
    }
}

/// Canonical reachable-state projection for differential comparison.
pub fn reachable_canonical<S: TransitionSystem>(
    system: &S,
    bounds: &crate::sem::outcome::AnalysisBounds,
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
            bounds: crate::sem::outcome::AnalysisBounds::default(),
            analysis_started: false,
        }
    }
}

/// The single checked entry point: static validation, semantic supportability,
/// contract validation and rebinding, monitor derivation, exploration, and
/// property checking. CLI and repair reuse this.
pub fn verify_program(
    program: &crate::ast::Program,
    spec: &ContractSpec,
    engine: EngineKind,
) -> VerificationReport {
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
    let static_report = crate::validate::validate(program);
    if !static_report.valid {
        let mut report = VerificationReport::synthetic(Outcome::Invalid, true);
        report.invalid = static_report
            .diagnostics
            .iter()
            .filter(|d| d.severity == crate::diagnostic::Severity::Error)
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
    let sem = match crate::sem::program::lower(program) {
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
    let monitor = crate::sem::monitor::MonitorConfig::from_predicates(contract.predicates());
    let report = match engine {
        EngineKind::Interpreter => {
            let e =
                crate::interp::Interpreter::with_monitor(&sem, contract.bounds.clone(), monitor);
            verify(&e, &contract)
        }
        EngineKind::Petri => {
            let e = crate::petri::PetriEngine::with_monitor(&sem, contract.bounds.clone(), monitor);
            verify(&e, &contract)
        }
    };
    finish_meta(report)
}
