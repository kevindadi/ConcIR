//! Unified finite-state BFS explorer shared across model checking and refinement.

use std::collections::{HashMap, VecDeque};

use concir_sem::outcome::{
    AnalysisBounds, BackendError, BoundaryEvent, BoundaryKind, Invalid, StepLabel, Unsupported,
};
use concir_sem::system::{Step, TransitionSystem};

/// Explored state-transition graph indexed by normalized canonical state key.
#[derive(Debug, Clone)]
pub struct StateGraph<S> {
    pub states: Vec<S>,
    pub index: HashMap<String, usize>,
    pub edges: Vec<Vec<(usize, StepLabel)>>,
    pub pred: Vec<Option<(usize, StepLabel)>>,
    pub had_boundary: Vec<bool>,
    pub depth: Vec<usize>,
    pub processed: Vec<bool>,
}

/// Options controlling bounded graph exploration.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExploreOptions {
    /// Stop immediately when `boundary_events.len() > bounds.max_boundary_events`
    /// (matches lightweight `explore()` behavior).
    pub stop_on_boundary_limit: bool,
}

/// Full result of exploring a [`TransitionSystem`] under `bounds`.
#[derive(Debug, Clone)]
pub struct ExploreOutcome<S> {
    pub graph: StateGraph<S>,
    pub transitions_explored: usize,
    pub boundary_events: Vec<BoundaryEvent>,
    pub unsupported: Vec<Unsupported>,
    pub invalid: Vec<Invalid>,
    pub truncated: bool,
    pub init_error: Option<BackendError>,
}

/// Deterministically explore a [`TransitionSystem`] using canonical `state_key`
/// deduplication up to `bounds`.
pub fn explore_system<S: TransitionSystem>(
    system: &S,
    bounds: &AnalysisBounds,
    opts: ExploreOptions,
) -> ExploreOutcome<S::State> {
    let mut graph = StateGraph {
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
        Err(err) => {
            match &err {
                BackendError::Unsupported(u) => unsupported.push(u.clone()),
                BackendError::Invalid(i) => invalid.push(i.clone()),
            }
            return ExploreOutcome {
                graph,
                transitions_explored,
                boundary_events,
                unsupported,
                invalid,
                truncated,
                init_error: Some(err),
            };
        }
    };

    let mut queue = VecDeque::new();
    graph.index.insert(system.state_key(&init), 0);
    graph.states.push(init);
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
        let depth = graph.depth[idx];
        if depth >= bounds.max_depth {
            graph.processed[idx] = true;
            truncated = true;
            graph.had_boundary[idx] = true;
            boundary_events.push(BoundaryEvent::new(
                BoundaryKind::DepthLimit,
                format!("depth limit {} reached", bounds.max_depth),
            ));
            if opts.stop_on_boundary_limit && boundary_events.len() > bounds.max_boundary_events {
                break;
            }
            continue;
        }

        let state = graph.states[idx].clone();
        match system.successors(&state) {
            Ok(enabled) => {
                graph.processed[idx] = true;
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
                    } else if graph.states.len() >= bounds.max_states {
                        truncated = true;
                        graph.had_boundary[idx] = true;
                        boundary_events.push(BoundaryEvent::new(
                            BoundaryKind::StateLimit,
                            format!("state limit {} reached", bounds.max_states),
                        ));
                    } else {
                        let target = graph.states.len();
                        graph.index.insert(key, target);
                        graph.states.push(succ);
                        graph.edges[idx].push((target, label.clone()));
                        graph.edges.push(Vec::new());
                        graph.pred.push(Some((idx, label)));
                        graph.had_boundary.push(false);
                        graph.depth.push(depth + 1);
                        graph.processed.push(false);
                        queue.push_back(target);
                    }
                }
                if opts.stop_on_boundary_limit
                    && boundary_events.len() > bounds.max_boundary_events
                {
                    truncated = true;
                    break;
                }
            }
            Err(BackendError::Unsupported(u)) => {
                unsupported.push(u);
                break;
            }
            Err(BackendError::Invalid(i)) => {
                invalid.push(i);
                break;
            }
        }
    }

    ExploreOutcome {
        graph,
        transitions_explored,
        boundary_events,
        unsupported,
        invalid,
        truncated,
        init_error: None,
    }
}
