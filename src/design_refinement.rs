//! Stage 0–2 Dual-Net Extension / Stuttering Refinement Checker
//! (`concir-backend verify-design`, schema `concir-design-refinement/1`).
//!
//! Verifies $N_{\text{req}} \sqsubseteq_{\text{ext}} N_{\text{code}}$:
//! 1. **Extension Projection Stuttering Simulation (`weak_simulation`):**
//!    Projects $N_{\text{code}}$ onto requirement milestone actions
//!    $\Sigma_{\text{req}}$ (all synchronization ops, internal branches, and
//!    untagged `seq_hole`s project to silent $\tau$) and checks
//!    $\pi_{\Sigma_{\text{req}}}(N_{\text{code}}) \preceq_{\text{stutter}} N_{\text{req}}$
//!    via [`unipn::analysis::refinement::check_stuttering_refinement`].
//! 2. **No New Deadlocks & Lock-Divergence Freedom:**
//!    No reachable deadlock in $N_{\text{code}}$ and no lock-holding $\tau$-cycle
//!    unless present in $N_{\text{req}}$.
//! 3. **Milestone Capability Preservation (`milestone_preservation`):**
//!    Every milestone declared in $N_{\text{req}}$ must remain reachable on at
//!    least one execution of $N_{\text{code}}$ (preventing trivial pruning).
//! 4. **Logic-Driven Health & Contract Obligations (`health_obligations`,
//!    `contract_satisfied`):**
//!    Verifies `BugKind::default_suite()` + any contract properties/preserved goals.

use std::collections::{BTreeSet, HashMap, VecDeque};

use serde::{Deserialize, Serialize};
use unipn::analysis::obligations::BugKind;
use unipn::analysis::refinement::check_stuttering_refinement;

use crate::ast::Program;
use crate::explore::contract::{ContractSpec, PropertySpec};
use crate::explore::{verify_program, EngineKind};
use crate::fqn;
use crate::petri::PetriEngine;
use crate::req_ir::{explore_req, ReqProgram};
use crate::sem::ids::ThreadId;
use crate::sem::outcome::{AnalysisBounds, Outcome, Phase};
use crate::sem::program::{self, SemOp, SemProgram};
use crate::sem::system::TransitionSystem;
use crate::unipn_engine::HlnState;
use crate::validate;

pub const DESIGN_REFINEMENT_SCHEMA: &str = "concir-design-refinement/1";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DesignRefinementChecks {
    pub req_valid: bool,
    pub design_valid: bool,
    pub weak_simulation: bool,
    pub deadlock_free: bool,
    pub lock_divergence_free: bool,
    pub milestone_preservation: bool,
    pub health_obligations: bool,
    pub contract_satisfied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesignViolation {
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub witness_milestones: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_milestone: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub impl_trace: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesignRefinementReport {
    pub schema: String,
    pub outcome: String,
    pub relation: String,
    pub complete: bool,
    pub checks: DesignRefinementChecks,
    pub req_states: usize,
    pub design_states: usize,
    pub declared_milestones: Vec<String>,
    pub realized_milestones: Vec<String>,
    pub missing_milestones: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<DesignViolation>,
}

fn empty_report(
    outcome: &str,
    declared_milestones: Vec<String>,
    violations: Vec<DesignViolation>,
) -> DesignRefinementReport {
    DesignRefinementReport {
        schema: DESIGN_REFINEMENT_SCHEMA.to_string(),
        outcome: outcome.to_string(),
        relation: "extension_stuttering_refinement".to_string(),
        complete: outcome != "unknown",
        checks: DesignRefinementChecks::default(),
        req_states: 0,
        design_states: 0,
        declared_milestones,
        realized_milestones: Vec::new(),
        missing_milestones: Vec::new(),
        violations,
    }
}

fn step_milestone_action(
    sem: &SemProgram,
    origin_fn: crate::sem::ids::FunctionId,
    origin_sid: Option<usize>,
    phase: Phase,
    actor: Option<ThreadId>,
    post_state: &HlnState,
) -> Option<String> {
    let pc = origin_sid?;
    let stmt = sem.function(origin_fn).body.get(pc)?;
    let milestone = stmt.milestone.as_ref()?;
    match &stmt.op {
        SemOp::CondvarWait { .. } => {
            if phase == Phase::Reacquire {
                Some(milestone.clone())
            } else {
                None
            }
        }
        _ => {
            if phase == Phase::Register {
                return None;
            }
            if let Some(tid) = actor
                && let Some((_, tok)) = post_state.find_thread(tid.0 as u32)
                && tok.wait_tag.is_some()
            {
                return None;
            }
            Some(milestone.clone())
        }
    }
}

/// Verify that a concurrent SCIR/CIR design `design` is a valid extension/stuttering
/// refinement of requirement net `req` ($N_{\text{req}} \sqsubseteq_{\text{ext}} N_{\text{code}}$).
pub fn verify_design_refinement(
    req: &ReqProgram,
    design: &Program,
    contract: Option<&ContractSpec>,
    bounds: AnalysisBounds,
) -> DesignRefinementReport {
    let declared_milestones: Vec<String> = req.milestones.iter().map(|m| m.name.clone()).collect();

    // 1. Explore abstract requirement net N_req.
    let req_graph = match explore_req(req, bounds.max_states.max(1)) {
        Ok(g) => g,
        Err(diags) => {
            let violations = diags
                .into_iter()
                .map(|d| DesignViolation {
                    kind: "invalid_req_ir".into(),
                    message: format!("{}: {}", d.code, d.message),
                    witness_milestones: Vec::new(),
                    failed_milestone: None,
                    impl_trace: Vec::new(),
                })
                .collect();
            return empty_report("invalid", declared_milestones, violations);
        }
    };

    // 2. Lower & validate SCIR/CIR design N_code.
    let mut lowered_prog = design.clone();
    if lowered_prog.has_structured_bodies() {
        if let Err(e) = lowered_prog.ensure_lowered() {
            return empty_report(
                "invalid",
                declared_milestones,
                vec![DesignViolation {
                    kind: "invalid_design".into(),
                    message: format!("SCIR lowering error: {e}"),
                    witness_milestones: Vec::new(),
                    failed_milestone: None,
                    impl_trace: Vec::new(),
                }],
            );
        }
    }
    let static_report = validate::validate(&lowered_prog);
    if !static_report.valid {
        let violations = static_report
            .diagnostics
            .into_iter()
            .filter(|d| d.severity == crate::diagnostic::Severity::Error)
            .map(|d| DesignViolation {
                kind: "invalid_design".into(),
                message: format!("{}: {}", d.code, d.message),
                witness_milestones: Vec::new(),
                failed_milestone: None,
                impl_trace: Vec::new(),
            })
            .collect();
        return empty_report("invalid", declared_milestones, violations);
    }
    let sem = match program::lower(&lowered_prog) {
        Ok(s) => s,
        Err(e) => {
            return empty_report(
                "invalid",
                declared_milestones,
                vec![DesignViolation {
                    kind: "invalid_design".into(),
                    message: e.to_string(),
                    witness_milestones: Vec::new(),
                    failed_milestone: None,
                    impl_trace: Vec::new(),
                }],
            );
        }
    };
    if !sem.unsupported().is_empty() {
        let violations = sem
            .unsupported()
            .iter()
            .map(|u| DesignViolation {
                kind: "unsupported_design".into(),
                message: format!("{}: {}", u.construct, u.detail),
                witness_milestones: Vec::new(),
                failed_milestone: None,
                impl_trace: Vec::new(),
            })
            .collect();
        return empty_report("unsupported", declared_milestones, violations);
    }

    // 3. Explore N_code via UniPN HLN engine and project transitions onto Σ_req milestones.
    let mut effective_contract = contract.cloned().unwrap_or_else(|| ContractSpec {
        name: req.name.clone(),
        properties: Vec::new(),
        preserved: Vec::new(),
        assumptions: Default::default(),
        bounds: Default::default(),
        allowed_scope: Default::default(),
    });
    effective_contract.bounds.max_threads = bounds.max_threads;
    effective_contract.bounds.max_frames_per_thread = bounds.max_frames_per_thread;
    effective_contract.bounds.max_states = bounds.max_states;
    effective_contract.bounds.max_depth = bounds.max_depth;
    effective_contract.bounds.max_boundary_events = bounds.max_boundary_events;

    let has_health = effective_contract
        .properties
        .iter()
        .any(|p| matches!(p, PropertySpec::Health { .. }));
    if !has_health {
        effective_contract.properties.push(PropertySpec::Health {
            id: "__design_health".into(),
            checks: BugKind::default_suite(),
            req: Vec::new(),
        });
    }

    let monitor = match effective_contract.resolve(&sem) {
        Ok(resolved) => crate::sem::monitor::MonitorConfig::from_predicates(resolved.predicates()),
        Err(_) => crate::sem::monitor::MonitorConfig::unbounded(),
    };
    let engine = PetriEngine::with_monitor(&sem, bounds.clone(), monitor);

    let init = match engine.initial() {
        Ok(s) => s,
        Err(e) => {
            return empty_report(
                "invalid",
                declared_milestones,
                vec![DesignViolation {
                    kind: "invalid_design".into(),
                    message: e.to_string(),
                    witness_milestones: Vec::new(),
                    failed_milestone: None,
                    impl_trace: Vec::new(),
                }],
            );
        }
    };

    let mut states = Vec::new();
    let mut seen = HashMap::new();
    let mut depths = Vec::new();
    let mut expanded = Vec::new();
    let mut out_counts = Vec::new();
    let mut impl_edges: Vec<(usize, usize, Option<String>, usize)> = Vec::new();
    let mut edge_names: Vec<String> = Vec::new();
    let mut realized_set: BTreeSet<String> = BTreeSet::new();
    let mut design_truncated = false;

    seen.insert(engine.state_key(&init), 0usize);
    states.push(init);
    depths.push(0usize);
    expanded.push(false);
    out_counts.push(0usize);
    let mut queue = VecDeque::from([0usize]);

    while let Some(idx) = queue.pop_front() {
        if states.len() >= bounds.max_states {
            design_truncated = true;
            break;
        }
        let depth = depths[idx];
        if depth >= bounds.max_depth {
            design_truncated = true;
            continue;
        }
        let state = states[idx].clone();
        let Ok(enabled) = engine.successors(&state) else {
            continue;
        };
        if !enabled.boundary.is_empty() {
            design_truncated = true;
        }
        let mut fully_expanded = true;
        for step in enabled.steps {
            let ms = step_milestone_action(
                &sem,
                step.label.origin.function,
                step.label.origin.sid,
                step.label.origin.phase,
                step.label.thread,
                &step.state,
            );
            if let Some(m) = &ms {
                realized_set.insert(m.clone());
            }

            let key = engine.state_key(&step.state);
            let target = if let Some(&existing) = seen.get(&key) {
                existing
            } else {
                if states.len() >= bounds.max_states {
                    design_truncated = true;
                    fully_expanded = false;
                    continue;
                }
                let next_idx = states.len();
                seen.insert(key, next_idx);
                states.push(step.state);
                depths.push(depth + 1);
                expanded.push(false);
                out_counts.push(0);
                queue.push_back(next_idx);
                next_idx
            };

            let f = sem.function(step.label.origin.function);
            let mod_name = sem.module_name(f.module);
            let sid_str = step
                .label
                .origin
                .sid
                .and_then(|i| f.body.get(i))
                .map(|s| s.sid.as_str())
                .unwrap_or("end");
            let tag = edge_names.len();
            let mut label_desc = format!("{}::{sid_str}", fqn::fqn(mod_name, &f.name));
            if let Some(m) = &ms {
                label_desc.push_str(&format!("[milestone:{m}]"));
            }
            edge_names.push(label_desc);
            out_counts[idx] += 1;
            impl_edges.push((idx, target, ms, tag));
        }
        if fully_expanded && enabled.boundary.is_empty() {
            expanded[idx] = true;
        }
    }

    let impl_deadlock: Vec<bool> = (0..states.len())
        .map(|i| expanded[i] && out_counts[i] == 0 && !engine.is_finished(&states[i]))
        .collect();
    let impl_holds_lock: Vec<bool> = states
        .iter()
        .map(|s| s.all_tokens().any(|(_, tok)| !tok.held_locks.is_empty()))
        .collect();

    let spec_edges: Vec<(usize, usize, Option<String>)> = req_graph
        .edges
        .iter()
        .map(|(u, v, ms, _)| (*u, *v, ms.clone()))
        .collect();
    let spec_deadlock_set: std::collections::HashSet<usize> =
        req_graph.deadlock_states.iter().copied().collect();

    let stut = check_stuttering_refinement(
        states.len(),
        0,
        &impl_edges,
        req_graph.states.len(),
        0,
        &spec_edges,
        |s| impl_deadlock.get(s).copied().unwrap_or(false),
        |s| spec_deadlock_set.contains(&s),
        |s| impl_holds_lock.get(s).copied().unwrap_or(false),
        false,
    );

    let mut violations = Vec::new();
    if !stut.simulated {
        let (witness_milestones, failed_milestone, impl_trace) = match &stut.simulation_witness {
            Some(w) => (
                w.observable_trace.clone(),
                w.failed_action.clone(),
                w.impl_edge_tags
                    .iter()
                    .filter_map(|&t| edge_names.get(t).cloned())
                    .collect(),
            ),
            None => (Vec::new(), None, Vec::new()),
        };
        let msg = match &failed_milestone {
            Some(m) => format!(
                "design N_code can execute milestone '{m}' at a point not permitted by N_req"
            ),
            None => "design N_code fails stuttering simulation against requirement net N_req".into(),
        };
        violations.push(DesignViolation {
            kind: "unexpected_milestone_order".into(),
            message: msg,
            witness_milestones,
            failed_milestone,
            impl_trace,
        });
    }
    if !stut.deadlocked_impl_states.is_empty() {
        violations.push(DesignViolation {
            kind: "deadlock_introduced".into(),
            message: format!(
                "design N_code introduces {} reachable deadlock state(s): {:?}",
                stut.deadlocked_impl_states.len(),
                stut.deadlocked_impl_states
            ),
            witness_milestones: Vec::new(),
            failed_milestone: None,
            impl_trace: Vec::new(),
        });
    }
    if !stut.lock_divergences.is_empty() {
        let div_trace = stut.lock_divergences[0]
            .cycle_edge_tags
            .iter()
            .filter_map(|&t| edge_names.get(t).cloned())
            .collect();
        violations.push(DesignViolation {
            kind: "lock_divergence".into(),
            message: "design N_code exhibits a silent (tau) loop while holding a lock".into(),
            witness_milestones: Vec::new(),
            failed_milestone: None,
            impl_trace: div_trace,
        });
    }

    let missing_milestones: Vec<String> = declared_milestones
        .iter()
        .filter(|m| !realized_set.contains(*m))
        .cloned()
        .collect();
    let complete_exploration = !req_graph.truncated && !design_truncated;
    let milestone_preservation = if complete_exploration {
        missing_milestones.is_empty()
    } else {
        // Under truncated exploration, report false only if complete, or flag unknown below.
        missing_milestones.is_empty()
    };
    if !missing_milestones.is_empty() && complete_exploration {
        for m in &missing_milestones {
            violations.push(DesignViolation {
                kind: "missing_milestone_capability".into(),
                message: format!(
                    "declared N_req milestone '{m}' is never reachable in design N_code"
                ),
                witness_milestones: Vec::new(),
                failed_milestone: Some(m.clone()),
                impl_trace: Vec::new(),
            });
        }
    }

    // 4. Verify concurrency health obligations + contract properties/preserved on N_code.
    let ver_report = verify_program(&lowered_prog, &effective_contract, EngineKind::Petri);
    let mut health_ok = true;
    let mut contract_ok = true;
    for prop in &ver_report.properties {
        let is_health_prop = prop.id == "__design_health"
            || effective_contract
                .properties
                .iter()
                .any(|p| matches!(p, PropertySpec::Health { id, .. } if id == &prop.id));
        if prop.outcome != Outcome::Pass {
            if is_health_prop {
                health_ok = false;
            } else {
                contract_ok = false;
            }
            if prop.outcome == Outcome::Fail {
                violations.push(DesignViolation {
                    kind: if is_health_prop {
                        "health_obligation_failed".into()
                    } else {
                        "contract_property_failed".into()
                    },
                    message: format!("property '{}': {}", prop.id, prop.detail),
                    witness_milestones: Vec::new(),
                    failed_milestone: None,
                    impl_trace: Vec::new(),
                });
            }
        }
    }
    if ver_report.outcome == Outcome::Fail
        && violations
            .iter()
            .all(|v| v.kind != "health_obligation_failed" && v.kind != "contract_property_failed")
    {
        contract_ok = false;
        for d in &ver_report.diagnostics {
            violations.push(DesignViolation {
                kind: "contract_property_failed".into(),
                message: format!("{}: {}", d.property, d.message),
                witness_milestones: Vec::new(),
                failed_milestone: None,
                impl_trace: d.counterexample_names.clone(),
            });
        }
    }

    let full_complete = complete_exploration && ver_report.complete;
    let any_fail = !stut.simulated
        || !stut.deadlocked_impl_states.is_empty()
        || !stut.lock_divergences.is_empty()
        || (complete_exploration && !missing_milestones.is_empty())
        || ver_report.outcome == Outcome::Fail
        || !violations.is_empty();

    let outcome = if ver_report.outcome == Outcome::Invalid {
        "invalid"
    } else if ver_report.outcome == Outcome::Unsupported {
        "unsupported"
    } else if any_fail {
        "fail"
    } else if !full_complete || ver_report.outcome == Outcome::Unknown {
        "unknown"
    } else {
        "pass"
    };

    DesignRefinementReport {
        schema: DESIGN_REFINEMENT_SCHEMA.to_string(),
        outcome: outcome.to_string(),
        relation: "extension_stuttering_refinement".to_string(),
        complete: full_complete,
        checks: DesignRefinementChecks {
            req_valid: true,
            design_valid: true,
            weak_simulation: stut.simulated,
            deadlock_free: stut.deadlocked_impl_states.is_empty(),
            lock_divergence_free: stut.lock_divergences.is_empty(),
            milestone_preservation,
            health_obligations: health_ok && ver_report.outcome != Outcome::Unknown,
            contract_satisfied: contract_ok && ver_report.outcome != Outcome::Unknown,
        },
        req_states: req_graph.states.len(),
        design_states: states.len(),
        declared_milestones,
        realized_milestones: realized_set.into_iter().collect(),
        missing_milestones,
        violations,
    }
}
