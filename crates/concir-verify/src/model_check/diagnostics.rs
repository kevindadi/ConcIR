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

use super::{CirStatementRef, DiagnosticRecord, VerificationReport};

pub(super) fn report_early(
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
        bounds: concir_sem::outcome::AnalysisBounds::default(),
        analysis_started: false,
    }
}

#[allow(clippy::too_many_arguments)]

pub(super) fn reverse_reachable<S>(graph: &StateGraph<S>, roots: &[usize]) -> HashSet<usize> {
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

pub(super) fn counterexample<S>(graph: &StateGraph<S>, idx: usize) -> Vec<StepLabel> {
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
pub(super) fn make_diagnostic<S: TransitionSystem>(
    system: &S,
    program: &SemProgram,
    graph: &StateGraph<S::State>,
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
    let ce = counterexample(graph, idx);
    let ce_names = ce.iter().map(|label| render_step_label(program, label)).collect();
    let doom_state = system.doom_snapshot(state);
    let mut hints = Vec::new();
    hints.extend(doom_hints(&doom_state));
    DiagnosticRecord {
        property: id.into(),
        outcome,
        message: message.into(),
        complete,
        counterexample: ce,
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
        repair_hints: hints,
        counterexample_names: ce_names,
        doom_state,
    }
}

/// Reachable state with the most blocked threads, if any (for a doom summary).
pub(super) fn best_doom_index<S: TransitionSystem>(system: &S, graph: &StateGraph<S::State>) -> Option<usize> {
    (0..graph.states.len())
        .max_by_key(|&i| system.blocked(&graph.states[i]).len())
        .filter(|&i| !system.blocked(&graph.states[i]).is_empty())
}

/// `(counterexample names, doom state, hints)` for an optional state index.
pub(super) fn doom_view<S: TransitionSystem>(
    system: &S,
    graph: &StateGraph<S::State>,
    idx: Option<usize>,
) -> (Vec<String>, DoomState, Vec<String>) {
    match idx {
        Some(i) => {
            let doom = system.doom_snapshot(&graph.states[i]);
            let names = counterexample(graph, i)
                .iter()
                .map(|l| {
                    let f = system.program().function(l.origin.function);
                    let sid = l
                        .origin
                        .sid
                        .and_then(|j| f.body.get(j))
                        .map(|s| s.sid.as_str())
                        .unwrap_or("-");
                    format!("{}::{}::{}", system.program().module_name(f.module), f.name, sid)
                })
                .collect();
            let hints = doom_hints(&doom);
            (names, doom, hints)
        }
        None => (Vec::new(), DoomState::default(), Vec::new()),
    }
}

/// `module::function::sid` for a step label.
pub(super) fn render_step_label(program: &SemProgram, label: &StepLabel) -> String {
    let f = program.function(label.origin.function);
    let sid = label
        .origin
        .sid
        .and_then(|i| f.body.get(i))
        .map(|s| s.sid.as_str())
        .unwrap_or("-");
    format!("{}::{}::{}", program.module_name(f.module), f.name, sid)
}

/// Templated hints derived from the doom state (lock/semaphore cycles, waits).
pub(super) fn doom_hints(doom: &DoomState) -> Vec<String> {
    let mut hints = Vec::new();
    let waiters: std::collections::HashMap<&str, &DoomThread> = doom
        .threads
        .iter()
        .filter_map(|t| t.waiting_on.as_ref().and_then(|w| w.resource.as_deref()).map(|r| (r, t)))
        .collect();
    for t in &doom.threads {
        let Some(wait) = &t.waiting_on else { continue };
        let Some(wait_res) = wait.resource.as_deref() else { continue };
        for held in &t.holds {
            if let Some(other) = waiters.get(held.as_str()) {
                let other_holds = other.holds.iter().any(|h| h == wait_res);
                if other_holds {
                    hints.push(format!(
                        "thread {} (at {}) holds {} and waits on {}; thread {} (at {}) holds {} \
                         and waits on {} — unify the acquisition order, or release {} before \
                         waiting on {}",
                        t.entry_function,
                        t.at_sid.as_deref().unwrap_or("-"),
                        held,
                        wait_res,
                        other.entry_function,
                        other.at_sid.as_deref().unwrap_or("-"),
                        wait_res,
                        held,
                        held,
                        wait_res,
                    ));
                }
            }
        }
        if wait.kind == "condvar" {
            hints.push(format!(
                "thread {} (at {}) is waiting on condvar {}; ensure some runnable thread reaches \
                 the matching notify after the predicate is set",
                t.entry_function,
                t.at_sid.as_deref().unwrap_or("-"),
                wait_res,
            ));
        }
    }
    hints
}

pub(super) fn cir_ref(program: &SemProgram, function: FunctionId, sid: Option<usize>) -> CirStatementRef {
    let f = program.function(function);
    CirStatementRef {
        module: program.module_name(f.module).to_string(),
        function: f.name.clone(),
        sid: sid.and_then(|i| f.body.get(i).map(|s| s.sid.clone())),
    }
}

/// Canonical reachable-state projection for differential comparison.

pub(super) fn holds_all_hint(
    program: &concir_sem::program::SemProgram,
    goal: &concir_sem::system::Predicate,
) -> Option<String> {
    use concir_sem::system::Predicate;
    let names = |resources: &[concir_sem::ids::ResourceId]| {
        resources
            .iter()
            .map(|r| program.resource(*r).name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match goal {
        Predicate::HoldsAll { func, resources } => Some(format!(
            "The design requires a reachable state in which `{}` holds all of [{}] simultaneously; no such state exists in this revision. Keep the nested acquisition; fix the defect by acquisition order, scope, or handshake instead of releasing early.",
            program.function(*func).name,
            names(resources)
        )),
        Predicate::NeverHoldsAll { func, resources } => Some(format!(
            "The design requires that no state exists in which `{}` holds all of [{}] simultaneously, but such a state is reachable in this revision. Break the nesting by acquisition order, scope, or handshake rather than adding unrelated statements.",
            program.function(*func).name,
            names(resources)
        )),
        _ => None,
    }
}
