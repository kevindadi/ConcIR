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

use super::diagnostics::{cir_ref, doom_hints, doom_view, render_step_label};
use super::{DiagnosticRecord, PropertyResult};

pub(super) fn check_property<S: TransitionSystem>(
    system: &S,
    program: &SemProgram,
    graph: &StateGraph<S::State>,
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
                    diagnostics.push(super::diagnostics::make_diagnostic(
                        system,
                        program,
                        graph,
                        id,
                        Outcome::Fail,
                        "safety invariant violated",
                        idx,
                        complete,
                    ));
                    PropertyResult::simple(
                        id,
                        Outcome::Fail,
                        "counterexample reached a violating state",
                    )
                }
                None => PropertyResult::simple(
                    id,
                    if complete {
                        Outcome::Pass
                    } else {
                        Outcome::Unknown
                    },
                    if complete {
                        "invariant held in all reachable states"
                    } else {
                        "no violation found, but the search was incomplete"
                    },
                ),
            }
        }
        Property::Unreachable { bad } => {
            let found = graph.states.iter().position(|s| system.satisfied(s, bad));
            match found {
                Some(idx) => {
                    diagnostics.push(super::diagnostics::make_diagnostic(
                        system,
                        program,
                        graph,
                        id,
                        Outcome::Fail,
                        "forbidden state reached",
                        idx,
                        complete,
                    ));
                    PropertyResult::simple(
                        id,
                        Outcome::Fail,
                        "counterexample reached a forbidden state",
                    )
                }
                None => PropertyResult::simple(
                    id,
                    if complete {
                        Outcome::Pass
                    } else {
                        Outcome::Unknown
                    },
                    if complete {
                        "forbidden state is unreachable"
                    } else {
                        "not found, but the search was incomplete"
                    },
                ),
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
                    let mut d = super::diagnostics::make_diagnostic(
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
                    PropertyResult::simple(
                        id,
                        Outcome::Fail,
                        "a reachable state has no enabled step and unfinished threads",
                    )
                }
                None => PropertyResult::simple(
                    id,
                    if complete {
                        Outcome::Pass
                    } else {
                        Outcome::Unknown
                    },
                    if complete {
                        "no deadlock state reachable"
                    } else {
                        "no deadlock found, but the search was incomplete"
                    },
                ),
            }
        }
        Property::Reachability { goal } => {
            let found = graph.states.iter().position(|s| system.satisfied(s, goal));
            match found {
                Some(idx) => {
                    let ce = super::diagnostics::counterexample(graph, idx);
                    PropertyResult::simple(
                        id,
                        Outcome::Pass,
                        format!(
                            "goal reachable via {} step(s) ({})",
                            ce.len(),
                            goal.description()
                        ),
                    )
                }
                None => {
                    let outcome = if complete {
                        Outcome::Fail
                    } else {
                        Outcome::Unknown
                    };
                    if outcome == Outcome::Fail {
                        let (ce_names, doom_state, mut hints) =
                            doom_view(system, graph, super::diagnostics::best_doom_index(system, graph));
                        hints.push(
                            "a goal that is unreachable cannot be fixed by reordering; check whether the goal is ever produced".into(),
                        );
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
                            repair_hints: hints,
                            counterexample_names: ce_names,
                            doom_state,
                        });
                    }
                    PropertyResult::simple(
                        id,
                        outcome,
                        if outcome == Outcome::Fail {
                            format!(
                                "goal '{}' is not reachable in any execution",
                                goal.description()
                            )
                        } else {
                            format!("goal '{}' not found; search incomplete", goal.description())
                        },
                    )
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
                let (ce_names, doom_state, mut hints) =
                    doom_view(system, graph, super::diagnostics::best_doom_index(system, graph));
                hints.push("make the goal reachable before requiring AG EF".into());
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
                    repair_hints: hints,
                    counterexample_names: ce_names,
                    doom_state,
                });
                return PropertyResult::simple(id, Outcome::Fail, "goal never reachable");
            }
            let can_reach = super::diagnostics::reverse_reachable(graph, &goal_states);
            let bad = (0..graph.states.len()).find(|i| !can_reach.contains(i));
            match bad {
                Some(idx) => {
                    let mut d = super::diagnostics::make_diagnostic(
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
                    PropertyResult::simple(
                        id,
                        Outcome::Fail,
                        "AG EF violated: a reachable state cannot reach the goal",
                    )
                }
                None => PropertyResult::simple(
                    id,
                    if complete {
                        Outcome::Pass
                    } else {
                        Outcome::Unknown
                    },
                    if complete {
                        "every reachable state can reach the goal"
                    } else {
                        "AG EF held on the explored fragment, but the search was incomplete"
                    },
                ),
            }
        }
        Property::Ctl {
            formula,
            non_vacuous,
            explanation,
        } => {
            let (edges, edge_labels) = flatten_graph_edges(graph);
            let res = unipn::analysis::temporal::check_ctl_graph(
                graph.states.len(),
                0,
                &edges,
                formula,
                |s, pred| system.satisfied(&graph.states[s], pred),
                |pred| pred.description(),
            );
            finalize_temporal_result(
                system,
                program,
                graph,
                id,
                "CTL",
                *non_vacuous,
                explanation,
                res,
                &edge_labels,
                complete,
                diagnostics,
            )
        }
        Property::Ltl {
            formula,
            fairness,
            non_vacuous,
            explanation,
        } => {
            let (edges, edge_labels) = flatten_graph_edges(graph);
            let res = unipn::analysis::temporal::check_ltl_graph(
                graph.states.len(),
                0,
                &edges,
                formula,
                fairness,
                |s, pred| system.satisfied(&graph.states[s], pred),
                |pred| pred.description(),
            );
            finalize_temporal_result(
                system,
                program,
                graph,
                id,
                "LTL",
                *non_vacuous,
                explanation,
                res,
                &edge_labels,
                complete,
                diagnostics,
            )
        }
        Property::PatternLtl {
            formula,
            fairness,
            non_vacuous,
            witness_reachable,
            witness_desc,
            explanation,
        } => {
            let (edges, edge_labels) = flatten_graph_edges(graph);
            let res = unipn::analysis::temporal::check_ltl_graph(
                graph.states.len(),
                0,
                &edges,
                formula,
                fairness,
                |s, pred| system.satisfied(&graph.states[s], pred),
                |pred| pred.description(),
            );
            let mut prop_res = finalize_temporal_result(
                system,
                program,
                graph,
                id,
                "Pattern-LTL",
                *non_vacuous,
                explanation,
                res,
                &edge_labels,
                complete,
                diagnostics,
            );
            if prop_res.outcome == Outcome::Pass
                && *non_vacuous
                && let Some(witness) = witness_reachable
                && !graph.states.iter().any(|s| system.satisfied(s, witness))
            {
                let desc = witness_desc
                    .clone()
                    .unwrap_or_else(|| witness.description());
                diagnostics.push(DiagnosticRecord {
                    property: id.into(),
                    outcome: Outcome::Fail,
                    message: format!(
                        "Pattern '{id}' passed vacuously because target effect [{desc}] is never reachable in any execution"
                    ),
                    complete,
                    counterexample: Vec::new(),
                    final_instances: Vec::new(),
                    blocked: Vec::new(),
                    cir_statements: Vec::new(),
                    proven_facts: vec![
                        format!("exhaustively explored {} reachable states", graph.states.len()),
                        format!("unreachable effect: {desc}"),
                    ],
                    repair_hints: vec![
                        "ensure the modeled program can reach the target condition rather than failing trivially before trigger".into(),
                    ],
                    counterexample_names: Vec::new(),
                    doom_state: Default::default(),
                });
                prop_res.outcome = Outcome::Fail;
                prop_res.vacuous = true;
                prop_res.vacuous_antecedents = vec![desc.clone()];
                prop_res.detail = format!(
                    "vacuous pass rejected (non_vacuous=true): target effect [{desc}] is never reachable in any execution"
                );
            }
            prop_res
        }
        Property::Health { checks, explanation } => {
            check_health_property(system, program, graph, id, checks, explanation, complete, diagnostics)
        }
    }
}

struct GraphObligationView<'a, S: TransitionSystem> {
    system: &'a S,
    program: &'a SemProgram,
    graph: &'a StateGraph<S::State>,
    deadlock_set: HashSet<usize>,
    lock_resources: Vec<ResourceId>,
}

impl<'a, S: TransitionSystem> GraphObligationView<'a, S> {
    fn new(system: &'a S, program: &'a SemProgram, graph: &'a StateGraph<S::State>) -> Self {
        let deadlock_set = graph
            .states
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                (graph.processed[i]
                    && !graph.had_boundary[i]
                    && graph.edges[i].is_empty()
                    && !system.is_finished(s))
                .then_some(i)
            })
            .collect();
        let lock_resources = program
            .resources()
            .iter()
            .filter(|r| matches!(r.kind, ResKind::Mutex | ResKind::RwLock))
            .map(|r| r.id)
            .collect();
        Self {
            system,
            program,
            graph,
            deadlock_set,
            lock_resources,
        }
    }

    fn site_place(func: FunctionId, sid: Option<usize>) -> PlaceId {
        PlaceId((func.0 as usize) * 1024 + sid.unwrap_or(0))
    }

    fn thread_place(&self, state_idx: usize, tid: u32) -> PlaceId {
        self.system
            .instances(&self.graph.states[state_idx])
            .into_iter()
            .find(|i| i.thread.0 as u32 == tid)
            .map_or(PlaceId(0), |i| Self::site_place(i.function, i.sid))
    }
}

impl<S: TransitionSystem> ObligationStateView for GraphObligationView<'_, S> {
    fn is_deadlock_state(&self, state_idx: usize) -> bool {
        self.deadlock_set.contains(&state_idx)
    }

    fn all_threads_finished(&self, state_idx: usize) -> bool {
        self.system.is_finished(&self.graph.states[state_idx])
    }

    fn all_locks_restored(&self, state_idx: usize) -> bool {
        let st = &self.graph.states[state_idx];
        self.lock_resources
            .iter()
            .all(|&r| self.system.satisfied(st, &Predicate::MutexFree(r)))
    }

    fn thread_ids(&self, state_idx: usize) -> Vec<u32> {
        let ids: BTreeSet<u32> = self
            .system
            .instances(&self.graph.states[state_idx])
            .into_iter()
            .map(|i| i.thread.0 as u32)
            .collect();
        ids.into_iter().collect()
    }

    fn thread_active(&self, state_idx: usize, tid: u32) -> bool {
        self.system
            .instances(&self.graph.states[state_idx])
            .into_iter()
            .any(|i| i.thread.0 as u32 == tid && i.status != "finished")
    }

    fn condvar_wait_sites(&self, state_idx: usize) -> Vec<(u32, PlaceId)> {
        self.system
            .blocked(&self.graph.states[state_idx])
            .into_iter()
            .filter(|b| b.kind == BlockKind::Condvar)
            .map(|b| {
                let tid = b.thread.0 as u32;
                (tid, self.thread_place(state_idx, tid))
            })
            .collect()
    }

    fn at_condvar_wait(&self, state_idx: usize, tid: u32, place: PlaceId) -> bool {
        self.system
            .blocked(&self.graph.states[state_idx])
            .into_iter()
            .any(|b| {
                b.kind == BlockKind::Condvar
                    && b.thread.0 as u32 == tid
                    && self.thread_place(state_idx, tid) == place
            })
    }

    fn contend_sites(&self, state_idx: usize) -> Vec<(u32, PlaceId)> {
        self.system
            .blocked(&self.graph.states[state_idx])
            .into_iter()
            .filter(|b| {
                matches!(
                    b.kind,
                    BlockKind::Lock
                        | BlockKind::Semaphore
                        | BlockKind::ChannelSend
                        | BlockKind::ChannelRecv
                )
            })
            .map(|b| {
                let tid = b.thread.0 as u32;
                (tid, self.thread_place(state_idx, tid))
            })
            .collect()
    }

    fn at_contend(&self, state_idx: usize, tid: u32, place: PlaceId) -> bool {
        self.system
            .blocked(&self.graph.states[state_idx])
            .into_iter()
            .any(|b| {
                matches!(
                    b.kind,
                    BlockKind::Lock
                        | BlockKind::Semaphore
                        | BlockKind::ChannelSend
                        | BlockKind::ChannelRecv
                ) && b.thread.0 as u32 == tid
                    && self.thread_place(state_idx, tid) == place
            })
    }

    fn contend_resume_enabled(&self, state_idx: usize, tid: u32, place: PlaceId) -> bool {
        if !self.at_contend(state_idx, tid, place) {
            return false;
        }
        self.graph.edges[state_idx]
            .iter()
            .any(|(_, lbl)| lbl.thread == Some(ThreadId(tid as u64)))
    }

    fn double_lock_detail(&self, state_idx: usize) -> Option<(u32, String)> {
        let state = &self.graph.states[state_idx];
        let doom = self.system.doom_snapshot(state);
        let insts = self.system.instances(state);
        for inst in insts {
            if inst.status == "finished" {
                continue;
            }
            let Some(sid) = inst.sid else { continue };
            let Some(stmt) = self.program.function(inst.function).body.get(sid) else {
                continue;
            };
            let lock_res = match &stmt.op {
                SemOp::MutexLock { resource } | SemOp::RwLockWrite { resource } => *resource,
                _ => continue,
            };
            let r = self.program.resource(lock_res);
            let r_fqn = fqn::fqn(self.program.module_name(r.module), &r.name);
            let tid = inst.thread.0 as u32;
            if doom
                .threads
                .iter()
                .any(|t| t.thread == tid && t.holds.iter().any(|h| h == &r_fqn))
            {
                return Some((tid, r_fqn));
            }
        }
        None
    }

    fn data_race_detail(&self, state_idx: usize) -> Option<String> {
        let state = &self.graph.states[state_idx];
        let insts = self.system.instances(state);
        let runnable: Vec<_> = insts
            .into_iter()
            .filter(|i| matches!(i.status.as_str(), "runnable" | "running"))
            .filter_map(|i| {
                let sid = i.sid?;
                let stmt = self.program.function(i.function).body.get(sid)?;
                let (reads, writes) = concir_engine::hln::lower::stmt_rw_vars(self.program, stmt);
                (!reads.is_empty() || !writes.is_empty())
                    .then_some((i.thread.0 as u32, reads, writes))
            })
            .collect();
        for i in 0..runnable.len() {
            let (tid1, r1, w1) = &runnable[i];
            for j in (i + 1)..runnable.len() {
                let (tid2, r2, w2) = &runnable[j];
                if tid1 == tid2 {
                    continue;
                }
                for &v in w1 {
                    if w2.contains(&v) || r2.contains(&v) {
                        let res = self.program.resource(ResourceId(v as u32));
                        return Some(fqn::fqn(self.program.module_name(res.module), &res.name));
                    }
                }
                for &v in w2 {
                    if r1.contains(&v) {
                        let res = self.program.resource(ResourceId(v as u32));
                        return Some(fqn::fqn(self.program.module_name(res.module), &res.name));
                    }
                }
            }
        }
        None
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn check_health_property<S: TransitionSystem>(
    system: &S,
    program: &SemProgram,
    graph: &StateGraph<S::State>,
    id: &str,
    checks: &[BugKind],
    explanation: &str,
    complete: bool,
    diagnostics: &mut Vec<DiagnosticRecord>,
) -> PropertyResult {
    let (edges, edge_labels) = flatten_graph_edges(graph);
    let view = GraphObligationView::new(system, program, graph);
    let report = check_obligations_graph(graph.states.len(), 0, &edges, !complete, checks, &view);

    if let Some(v) = report.violations.first() {
        let is_liveness = matches!(
            v.kind,
            BugKind::PartialDeadlock
                | BugKind::LostWakeup
                | BugKind::WakeStarvation
                | BugKind::AcquireStarvation
        );
        if !complete && is_liveness {
            let touches_boundary = v.trace.as_ref().is_none_or(|tr| {
                tr.prefix_states
                    .iter()
                    .chain(tr.cycle_states.iter())
                    .any(|&s| !graph.processed[s] || graph.had_boundary[s])
            });
            if touches_boundary {
                return PropertyResult {
                    id: id.into(),
                    outcome: Outcome::Unknown,
                    detail: format!(
                        "health check '{}' inconclusive because exploration was incomplete",
                        v.kind.label()
                    ),
                    vacuous: false,
                    vacuous_antecedents: Vec::new(),
                    explanation: Some(explanation.to_string()),
                };
            }
        }

        let (ce, final_idx) = if let Some(tr) = &v.trace {
            let mut steps = Vec::new();
            for &tag in tr.prefix_edges.iter().chain(tr.cycle_edges.iter()) {
                if let Some(lbl) = edge_labels.get(tag) {
                    steps.push(lbl.clone());
                }
            }
            let idx = tr
                .cycle_states
                .last()
                .or_else(|| tr.prefix_states.last())
                .copied()
                .unwrap_or(0);
            (steps, idx)
        } else {
            (Vec::new(), 0)
        };
        let state = &graph.states[final_idx];
        let instances = system.instances(state);
        let blocked = system.blocked(state);
        let cir_statements = instances
            .iter()
            .map(|i| cir_ref(program, i.function, i.sid))
            .collect();
        let ce_names: Vec<String> = ce.iter().map(|l| render_step_label(program, l)).collect();
        let doom_state = system.doom_snapshot(state);
        diagnostics.push(DiagnosticRecord {
            property: id.into(),
            outcome: Outcome::Fail,
            message: format!("health check '{}' failed: {}", v.kind.label(), v.message),
            complete,
            counterexample: ce,
            final_instances: instances,
            blocked,
            cir_statements,
            proven_facts: vec![
                format!("obligation formula: {}", v.formula),
                format!("explored {} reachable states", graph.states.len()),
            ],
            repair_hints: doom_hints(&doom_state),
            counterexample_names: ce_names,
            doom_state,
        });
        return PropertyResult {
            id: id.into(),
            outcome: Outcome::Fail,
            detail: v.message.clone(),
            vacuous: false,
            vacuous_antecedents: Vec::new(),
            explanation: Some(explanation.to_string()),
        };
    }

    if !complete {
        return PropertyResult {
            id: id.into(),
            outcome: Outcome::Unknown,
            detail: "health obligations held on explored fragment, but search was incomplete".into(),
            vacuous: false,
            vacuous_antecedents: Vec::new(),
            explanation: Some(explanation.to_string()),
        };
    }

    PropertyResult {
        id: id.into(),
        outcome: Outcome::Pass,
        detail: format!(
            "all {} concurrency health obligation(s) passed across {} states",
            report.checks_run.len(),
            graph.states.len()
        ),
        vacuous: false,
        vacuous_antecedents: Vec::new(),
        explanation: Some(explanation.to_string()),
    }
}

pub(super) fn flatten_graph_edges<S>(graph: &StateGraph<S>) -> (Vec<(usize, usize, usize)>, Vec<StepLabel>) {
    let mut edges = Vec::new();
    let mut labels = Vec::new();
    for (src, outs) in graph.edges.iter().enumerate() {
        for (dst, label) in outs {
            let tag = labels.len();
            labels.push(label.clone());
            edges.push((src, *dst, tag));
        }
    }
    (edges, labels)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn finalize_temporal_result<S: TransitionSystem>(
    system: &S,
    program: &SemProgram,
    graph: &StateGraph<S::State>,
    id: &str,
    logic: &str,
    non_vacuous: bool,
    explanation: &str,
    res: unipn::analysis::temporal::TemporalCheckResult,
    edge_labels: &[StepLabel],
    complete: bool,
    diagnostics: &mut Vec<DiagnosticRecord>,
) -> PropertyResult {
    if res.satisfied {
        if !complete {
            return PropertyResult {
                id: id.into(),
                outcome: Outcome::Unknown,
                detail: format!("{logic} property held on the explored fragment, but the search was incomplete"),
                vacuous: res.vacuous,
                vacuous_antecedents: res.vacuous_antecedents,
                explanation: Some(explanation.to_string()),
            };
        }
        if non_vacuous && res.vacuous {
            let antecedents = if res.vacuous_antecedents.is_empty() {
                "implication antecedent".to_string()
            } else {
                res.vacuous_antecedents.join("; ")
            };
            diagnostics.push(DiagnosticRecord {
                property: id.into(),
                outcome: Outcome::Fail,
                message: format!(
                    "{logic} property '{id}' passed vacuously because implication antecedent(s) [{antecedents}] are never reachable in any execution"
                ),
                complete,
                counterexample: Vec::new(),
                final_instances: Vec::new(),
                blocked: Vec::new(),
                cir_statements: Vec::new(),
                proven_facts: vec![
                    format!("exhaustively explored {} reachable states", graph.states.len()),
                    format!("unreachable antecedent(s): {antecedents}"),
                ],
                repair_hints: vec![
                    "ensure the modeled program can reach the trigger/antecedent condition, or check whether a dead branch/wrong initial state prevents it".into(),
                ],
                counterexample_names: Vec::new(),
                doom_state: Default::default(),
            });
            return PropertyResult {
                id: id.into(),
                outcome: Outcome::Fail,
                detail: format!(
                    "vacuous pass rejected (non_vacuous=true): implication antecedent(s) [{antecedents}] are never reachable in any execution"
                ),
                vacuous: true,
                vacuous_antecedents: res.vacuous_antecedents,
                explanation: Some(explanation.to_string()),
            };
        }
        return PropertyResult {
            id: id.into(),
            outcome: Outcome::Pass,
            detail: format!("{logic} formula satisfied across all {} reachable states", graph.states.len()),
            vacuous: res.vacuous,
            vacuous_antecedents: res.vacuous_antecedents,
            explanation: Some(explanation.to_string()),
        };
    }

    // Formula not satisfied: check whether a counterexample trace touches an
    // incomplete truncation boundary when `!complete`.
    if !complete {
        let touches_boundary = res.trace.as_ref().is_none_or(|tr| {
            tr.prefix_states
                .iter()
                .chain(tr.cycle_states.iter())
                .any(|&s| !graph.processed[s] || graph.had_boundary[s])
        });
        if touches_boundary {
            return PropertyResult {
                id: id.into(),
                outcome: Outcome::Unknown,
                detail: format!("{logic} check inconclusive because state-space exploration was incomplete"),
                vacuous: false,
                vacuous_antecedents: Vec::new(),
                explanation: Some(explanation.to_string()),
            };
        }
    }

    let (ce, final_idx, cycle_len) = if let Some(tr) = &res.trace {
        let mut steps = Vec::new();
        for &tag in &tr.prefix_edges {
            if let Some(lbl) = edge_labels.get(tag) {
                steps.push(lbl.clone());
            }
        }
        for &tag in &tr.cycle_edges {
            if let Some(lbl) = edge_labels.get(tag) {
                steps.push(lbl.clone());
            }
        }
        let idx = tr
            .cycle_states
            .last()
            .or_else(|| tr.prefix_states.last())
            .copied()
            .unwrap_or(0);
        (steps, idx, tr.cycle_states.len())
    } else {
        (Vec::new(), 0, 0)
    };

    let state = &graph.states[final_idx];
    let instances = system.instances(state);
    let blocked = system.blocked(state);
    let cir_statements = instances
        .iter()
        .map(|i| cir_ref(program, i.function, i.sid))
        .collect();
    let ce_names: Vec<String> = ce.iter().map(|l| render_step_label(program, l)).collect();
    let doom_state = system.doom_snapshot(state);
    let mut hints = doom_hints(&doom_state);
    if cycle_len > 0 {
        hints.push(format!(
            "{logic} counterexample contains a repeating cycle of {cycle_len} state(s); check fairness/progress conditions"
        ));
    }
    let mut proven_facts = vec![
        format!("explored {} reachable states", graph.states.len()),
        format!("{logic} counterexample has {} transition(s)", ce.len()),
    ];
    if cycle_len > 0 {
        proven_facts.push(format!("counterexample lasso cycle length: {cycle_len}"));
    }
    diagnostics.push(DiagnosticRecord {
        property: id.into(),
        outcome: Outcome::Fail,
        message: format!("{logic} property '{id}' violated: {explanation}"),
        complete,
        counterexample: ce,
        final_instances: instances,
        blocked,
        cir_statements,
        proven_facts,
        repair_hints: hints,
        counterexample_names: ce_names,
        doom_state,
    });

    PropertyResult {
        id: id.into(),
        outcome: Outcome::Fail,
        detail: format!("{logic} property violated"),
        vacuous: false,
        vacuous_antecedents: Vec::new(),
        explanation: Some(explanation.to_string()),
    }
}

