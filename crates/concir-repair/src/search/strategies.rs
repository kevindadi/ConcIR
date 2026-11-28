use std::collections::{BTreeMap, VecDeque};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use concir_ir::ast::Program;
use concir_verify::contract::ContractSpec;
use concir_verify::model_check::{verify_program, EngineKind, VerificationReport};
use concir_sem::outcome::{AnalysisBounds, Outcome};
use concir_ir::validate;

use crate::candidates::{
    CandidateProvider, LockOrderCompositeEnumerator, LockOrderEnumerator, NodeHistory,
    RepairContext,
};
use crate::patch::{self, function_hash, CirPatch, PatchChange, SourceRelation};
use crate::RepairOutcome;


use super::{empty_report, effective_bounds, node_is_expandable, AppliedEdit, Counts, Node, NodeReport, AttemptReport, RepairStrategy, SearchConfig, SearchReport, TerminalFacts};
use super::program_fingerprint;

pub fn run_search(program: &Program, spec: &ContractSpec, config: &SearchConfig) -> SearchReport {
    let bounds = effective_bounds(spec);

    // Budget validation happens before any work: the root verification counts.
    if config.verification_budget == 0 {
        // Reject before any verification runs: the root would already exceed
        // the budget.
        return empty_report(
            config.strategy,
            RepairOutcome::InvalidConfig,
            "verification-budget-zero",
            bounds,
        );
    }

    let mut nodes: Vec<Node> = Vec::new();
    let mut node_reports: Vec<NodeReport> = Vec::new();
    let mut attempts: Vec<AttemptReport> = Vec::new();
    let mut node_by_fp: BTreeMap<String, usize> = BTreeMap::new();
    let mut proposals = 0usize;
    let mut verifications = 0usize;
    let mut cache_hits = 0usize;
    let mut states_explored = 0usize;
    let mut candidate_budget_hit = false;
    let mut verification_budget_hit = false;
    let mut depth_truncated = false;
    let mut edits_truncated = false;
    let mut saw_unknown = false;

    let root_fp = program_fingerprint(program);
    let root_report = verify_program(program, spec, EngineKind::Petri);
    verifications += 1;
    states_explored += root_report.states_explored;

    match root_report.outcome {
        Outcome::Pass if root_report.complete => {
            node_by_fp.insert(root_fp.clone(), 0);
            nodes.push(Node {
                parent: None,
                depth: 0,
                total_edits: 0,
                program: program.clone(),
                fingerprint: root_fp.clone(),
                report: root_report.clone(),
                incoming: None,
            });
            node_reports.push(node_from_report(0, None, 0, 0, root_fp, None, root_report));
            return finish(
                config,
                bounds,
                RepairOutcome::AlreadySatisfied,
                "already-satisfied",
                saw_unknown,
                None,
                proposals,
                verifications,
                cache_hits,
                states_explored,
                nodes,
                node_reports,
                attempts,
                Vec::new(),
                None,
                None,
            );
        }
        Outcome::Invalid => {
            node_by_fp.insert(root_fp.clone(), 0);
            nodes.push(Node {
                parent: None,
                depth: 0,
                total_edits: 0,
                program: program.clone(),
                fingerprint: root_fp.clone(),
                report: root_report.clone(),
                incoming: None,
            });
            node_reports.push(node_from_report(0, None, 0, 0, root_fp, None, root_report));
            return finish(
                config,
                bounds,
                RepairOutcome::Invalid,
                "root-invalid",
                saw_unknown,
                None,
                proposals,
                verifications,
                cache_hits,
                states_explored,
                nodes,
                node_reports,
                attempts,
                Vec::new(),
                None,
                None,
            );
        }
        Outcome::Unsupported => {
            node_by_fp.insert(root_fp.clone(), 0);
            nodes.push(Node {
                parent: None,
                depth: 0,
                total_edits: 0,
                program: program.clone(),
                fingerprint: root_fp.clone(),
                report: root_report.clone(),
                incoming: None,
            });
            node_reports.push(node_from_report(0, None, 0, 0, root_fp, None, root_report));
            return finish(
                config,
                bounds,
                RepairOutcome::Unsupported,
                "root-unsupported",
                saw_unknown,
                None,
                proposals,
                verifications,
                cache_hits,
                states_explored,
                nodes,
                node_reports,
                attempts,
                Vec::new(),
                None,
                None,
            );
        }
        Outcome::Unknown => {
            saw_unknown = true;
            node_by_fp.insert(root_fp.clone(), 0);
            nodes.push(Node {
                parent: None,
                depth: 0,
                total_edits: 0,
                program: program.clone(),
                fingerprint: root_fp.clone(),
                report: root_report.clone(),
                incoming: None,
            });
            node_reports.push(node_from_report(0, None, 0, 0, root_fp, None, root_report));
            return finish(
                config,
                bounds,
                RepairOutcome::AnalysisUnknown,
                "root-unknown",
                saw_unknown,
                None,
                proposals,
                verifications,
                cache_hits,
                states_explored,
                nodes,
                node_reports,
                attempts,
                Vec::new(),
                None,
                None,
            );
        }
        _ => {}
    }

    node_by_fp.insert(root_fp.clone(), 0);
    nodes.push(Node {
        parent: None,
        depth: 0,
        total_edits: 0,
        program: program.clone(),
        fingerprint: root_fp.clone(),
        report: root_report.clone(),
        incoming: None,
    });
    node_reports.push(node_from_report(0, None, 0, 0, root_fp, None, root_report));

    let mut queue: VecDeque<usize> = VecDeque::from([0usize]);

    'outer: while let Some(nid) = queue.pop_front() {
        let (node_program, node_report, node_depth, node_edits, node_fp) = {
            let n = &nodes[nid];
            (
                n.program.clone(),
                n.report.clone(),
                n.depth,
                n.total_edits,
                n.fingerprint.clone(),
            )
        };
        if node_depth >= config.max_depth {
            depth_truncated = true;
            if let Some(nr) = node_reports.iter_mut().find(|r| r.id == nid) {
                nr.note = Some("not expanded: max_depth".into());
            }
            continue;
        }
        if node_edits >= config.max_total_edits {
            edits_truncated = true;
            if let Some(nr) = node_reports.iter_mut().find(|r| r.id == nid) {
                nr.note = Some("not expanded: max_total_edits".into());
            }
            continue;
        }
        let scope = &spec.allowed_scope;
        let mut provider: Box<dyn CandidateProvider> = match config.strategy {
            RepairStrategy::Single => Box::new(LockOrderEnumerator::new(&node_program, scope)),
            RepairStrategy::Composite => Box::new(LockOrderCompositeEnumerator::new(
                &node_program,
                scope,
                false,
            )),
            RepairStrategy::Diagnostic => Box::new(LockOrderCompositeEnumerator::new(
                &node_program,
                scope,
                true,
            )),
        };
        let history: Vec<NodeHistory> = ancestor_history(&nodes, nid);

        loop {
            if proposals >= config.candidate_budget {
                candidate_budget_hit = true;
                break 'outer;
            }
            let ctx = RepairContext {
                program: &node_program,
                spec,
                round: node_depth,
                depth: node_depth,
                report: Some(&node_report),
                history: &history,
            };
            let Some(candidate) = provider.next_candidate(&ctx) else {
                break;
            };
            proposals += 1;
            let attempt_id = attempts.len();
            match attempt_outcome(
                spec,
                &node_program,
                &node_fp,
                &candidate,
                &mut node_by_fp,
                &mut nodes,
                &mut node_reports,
                &mut verifications,
                &mut states_explored,
                config,
                &mut verification_budget_hit,
            ) {
                AttemptFlow::Reused { node, outcome } => {
                    cache_hits += 1;
                    attempts.push(AttemptReport {
                        id: attempt_id,
                        parent: nid,
                        patch: Some(candidate.clone()),
                        result: "reused".into(),
                        reused_node: Some(node),
                        program_fingerprint: Some(nodes[node].fingerprint.clone()),
                        outcome: Some(outcome),
                        reason: Some("equivalent candidate already verified".into()),
                    });
                }
                AttemptFlow::Verified { node, accepted } => {
                    attempts.push(AttemptReport {
                        id: attempt_id,
                        parent: nid,
                        patch: Some(candidate.clone()),
                        result: "verified".into(),
                        reused_node: None,
                        program_fingerprint: Some(nodes[node].fingerprint.clone()),
                        outcome: Some(nodes[node].report.outcome),
                        reason: None,
                    });
                    if accepted {
                        let chain = build_chain(&nodes, node);
                        let accepted_report = nodes[node].report.clone();
                        let accepted_program = nodes[node].program.clone();
                        let mut report = finish(
                            config,
                            bounds,
                            RepairOutcome::Repaired,
                            "solved",
                            saw_unknown,
                            None,
                            proposals,
                            verifications,
                            cache_hits,
                            states_explored,
                            nodes,
                            node_reports,
                            attempts,
                            chain,
                            Some(accepted_program),
                            Some(accepted_report),
                        );
                        report.accepted_node = Some(node);
                        return report;
                    }
                    if nodes[node].report.outcome == Outcome::Fail {
                        // Strategy A only expands the root (single edit).
                        if config.strategy != RepairStrategy::Single {
                            queue.push_back(node);
                        }
                    } else if nodes[node].report.outcome == Outcome::Unknown {
                        saw_unknown = true;
                    }
                }
                AttemptFlow::Rejected { reason } => {
                    attempts.push(AttemptReport {
                        id: attempt_id,
                        parent: nid,
                        patch: Some(candidate.clone()),
                        result: if reason.starts_with("disallowed") {
                            "denied".into()
                        } else if reason.starts_with("apply") {
                            "apply-error".into()
                        } else {
                            "static-invalid".into()
                        },
                        reused_node: None,
                        program_fingerprint: None,
                        outcome: None,
                        reason: Some(reason),
                    });
                }
                AttemptFlow::Budget { fingerprint } => {
                    verification_budget_hit = true;
                    attempts.push(AttemptReport {
                        id: attempt_id,
                        parent: nid,
                        patch: Some(candidate.clone()),
                        result: "budget-blocked".into(),
                        reused_node: None,
                        program_fingerprint: Some(fingerprint),
                        outcome: None,
                        reason: Some(
                            "verification budget exhausted before this candidate was verified"
                                .into(),
                        ),
                    });
                    break 'outer;
                }
            }
        }
    }

    let facts = TerminalFacts {
        root_unknown: false,
        candidate_budget_hit,
        verification_budget_hit,
        depth_truncated,
        edits_truncated,
        saw_unknown,
    };
    let (outcome, stop_reason, truncation) = facts.classify();
    let truncation = truncation.map(str::to_string);

    finish(
        config,
        bounds,
        outcome,
        stop_reason,
        saw_unknown,
        truncation,
        proposals,
        verifications,
        cache_hits,
        states_explored,
        nodes,
        node_reports,
        attempts,
        Vec::new(),
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish(
    config: &SearchConfig,
    bounds: AnalysisBounds,
    outcome: RepairOutcome,
    stop_reason: &str,
    saw_unknown: bool,
    truncation: Option<String>,
    proposals: usize,
    verifications: usize,
    cache_hits: usize,
    states_explored: usize,
    nodes: Vec<Node>,
    node_reports: Vec<NodeReport>,
    attempts: Vec<AttemptReport>,
    patch_chain: Vec<AppliedEdit>,
    accepted_program: Option<Program>,
    accepted_report: Option<VerificationReport>,
) -> SearchReport {
    SearchReport {
        strategy: config.strategy,
        outcome,
        stop_reason: stop_reason.into(),
        saw_unknown,
        truncation,
        effective_bounds: bounds,
        proposals,
        unique_programs: nodes.len(),
        verifications,
        cache_hits,
        states_explored,
        nodes: node_reports,
        attempts,
        patch_chain,
        accepted_node: None,
        accepted_program,
        accepted_report,
    }
}

enum AttemptFlow {
    Reused {
        node: usize,
        outcome: Outcome,
    },
    Verified {
        node: usize,
        accepted: bool,
    },
    Rejected {
        reason: String,
    },
    /// The candidate was generated, applied, and statically validated, but the
    /// verification budget was exhausted before it could be verified. No
    /// outcome or node is fabricated.
    Budget {
        fingerprint: String,
    },
}

#[allow(clippy::too_many_arguments)]
fn attempt_outcome(
    spec: &ContractSpec,
    node_program: &Program,
    node_fp: &str,
    candidate: &CirPatch,
    node_by_fp: &mut BTreeMap<String, usize>,
    nodes: &mut Vec<Node>,
    node_reports: &mut Vec<NodeReport>,
    verifications: &mut usize,
    states_explored: &mut usize,
    config: &SearchConfig,
    verification_budget_hit: &mut bool,
) -> AttemptFlow {
    let scope = &spec.allowed_scope;
    if let Err(e) = patch::check_allowed(scope, candidate) {
        return AttemptFlow::Rejected {
            reason: format!("disallowed: {e}"),
        };
    }
    let patched = match patch::apply(node_program, candidate) {
        Ok((p, _diff)) => p,
        Err(e) => {
            return AttemptFlow::Rejected {
                reason: format!("apply error: {e}"),
            }
        }
    };
    if !validate::validate(&patched).valid {
        return AttemptFlow::Rejected {
            reason: "static validation failed".into(),
        };
    }
    let child_fp = program_fingerprint(&patched);
    // Deduplicate *before* the expensive verification and before counting it.
    if let Some(&existing) = node_by_fp.get(&child_fp) {
        return AttemptFlow::Reused {
            node: existing,
            outcome: nodes[existing].report.outcome,
        };
    }
    if *verifications >= config.verification_budget {
        *verification_budget_hit = true;
        return AttemptFlow::Budget {
            fingerprint: child_fp,
        };
    }
    let child_report = verify_program(&patched, spec, EngineKind::Petri);
    *verifications += 1;
    *states_explored += child_report.states_explored;
    let accepted = child_report.outcome == Outcome::Pass && child_report.complete;
    let parent_id = node_by_fp.get(node_fp).copied().unwrap_or(0);
    let (parent_depth, parent_edits) = (nodes[parent_id].depth, nodes[parent_id].total_edits);
    let depth = parent_depth + 1;
    let total_edits = parent_edits + candidate.changes.len();
    let is_fail = child_report.outcome == Outcome::Fail;
    let is_unknown = child_report.outcome == Outcome::Unknown;
    let id = nodes.len();
    let incoming = applied_edit(candidate, node_fp, &child_fp, node_program);
    nodes.push(Node {
        parent: Some(parent_id),
        depth,
        total_edits,
        program: patched,
        fingerprint: child_fp.clone(),
        report: child_report.clone(),
        incoming: Some(candidate.clone()),
    });
    node_by_fp.insert(child_fp.clone(), id);
    let mut nr = node_from_report(
        id,
        Some(parent_id),
        depth,
        total_edits,
        child_fp,
        Some(incoming),
        child_report,
    );
    nr.note = if accepted {
        Some("accepted".into())
    } else if is_fail {
        None
    } else if is_unknown {
        Some("analysis unknown; not expanded".into())
    } else {
        Some("not expanded".into())
    };
    node_reports.push(nr);
    AttemptFlow::Verified { node: id, accepted }
}

fn node_from_report(
    id: usize,
    parent: Option<usize>,
    depth: usize,
    total_edits: usize,
    fingerprint: String,
    incoming: Option<AppliedEdit>,
    report: VerificationReport,
) -> NodeReport {
    NodeReport {
        id,
        parent,
        depth,
        total_edits,
        program_fingerprint: fingerprint,
        incoming,
        report,
        note: None,
    }
}

pub(super) fn applied_edit(
    candidate: &CirPatch,
    parent_fp: &str,
    child_fp: &str,
    parent_program: &Program,
) -> AppliedEdit {
    let original_function_hash =
        function_hash(parent_program, &candidate.module, &candidate.function)
            .unwrap_or_else(|_| candidate.original_hash.clone());
    AppliedEdit {
        module: candidate.module.clone(),
        function: candidate.function.clone(),
        changes: candidate.changes.clone(),
        provenance: candidate.provenance.clone(),
        original_function_hash,
        parent_fingerprint: parent_fp.to_string(),
        program_fingerprint: child_fp.to_string(),
    }
}

fn ancestor_history(nodes: &[Node], nid: usize) -> Vec<NodeHistory> {
    let mut ids = Vec::new();
    let mut cur = Some(nid);
    while let Some(id) = cur {
        ids.push(id);
        cur = nodes[id].parent;
    }
    ids.reverse();
    ids.into_iter()
        .map(|id| {
            let n = &nodes[id];
            NodeHistory {
                depth: n.depth,
                edits: n
                    .incoming
                    .as_ref()
                    .map(|c| format!("{}:{}:{:?}", c.module, c.function, c.changes))
                    .into_iter()
                    .collect(),
                outcome: n.report.outcome,
            }
        })
        .collect()
}

/// The patch chain from the root to `target`, each edit carrying its parent and
/// child program fingerprints.
pub(super) fn build_chain(nodes: &[Node], target: usize) -> Vec<AppliedEdit> {
    let mut ids = Vec::new();
    let mut cur = Some(target);
    while let Some(id) = cur {
        ids.push(id);
        cur = nodes[id].parent;
    }
    ids.reverse();
    let mut chain = Vec::new();
    for id in ids {
        let n = &nodes[id];
        if let Some(inc) = &n.incoming {
            chain.push(applied_edit(
                inc,
                n.parent
                    .map(|p| nodes[p].fingerprint.as_str())
                    .unwrap_or(""),
                &n.fingerprint,
                n.parent.map(|p| &nodes[p].program).unwrap_or(&n.program),
            ));
        }
    }
    chain
}

