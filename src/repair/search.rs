//! Budgeted composite patch search.
//!
//! Three comparable strategies share the same edit space, permissions,
//! verification semantics, and budgets:
//!
//! - `Single` (A): expand only the root, one edit deep — the legacy baseline.
//! - `Composite` (B): bounded BFS over nodes, no diagnostic guidance.
//! - `Diagnostic` (C): the same BFS, but a node's structured diagnostics
//!   (blocked/holder resource facts) filter the candidate edits.
//!
//! The frozen `ContractSpec` is the single source of truth for the verification
//! bounds. Candidate programs are deduplicated by content fingerprint *before*
//! verification, so a repeated program never consumes verification budget.
//! Intermediate `FAIL` nodes are kept as search nodes; only an overall complete
//! `PASS` is accepted. Every run produces a self-contained, replayable
//! [`SearchArtifact`].

use std::collections::{BTreeMap, VecDeque};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::ast::Program;
use crate::explore::contract::ContractSpec;
use crate::explore::{verify_program, EngineKind, VerificationReport};
use crate::sem::outcome::{AnalysisBounds, Outcome};
use crate::validate;

use super::candidates::{
    CandidateProvider, LockOrderCompositeEnumerator, LockOrderEnumerator, NodeHistory, RepairContext,
};
use super::patch::{self, function_hash, CirPatch, PatchChange, SourceRelation};
use super::RepairOutcome;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairStrategy {
    /// A: single, diagnostic-free enumeration on the original program.
    Single,
    /// B: bounded composite search, no diagnostic guidance.
    Composite,
    /// C: bounded composite search guided by diagnostics.
    Diagnostic,
}

/// Configuration. Verification bounds are **not** here: the frozen
/// `ContractSpec.bounds` is authoritative for every verification call, and the
/// effective bounds are exported in the artifact.
#[derive(Debug, Clone)]
pub struct SearchConfig {
    pub strategy: RepairStrategy,
    pub candidate_budget: usize,
    pub verification_budget: usize,
    pub max_depth: usize,
    pub max_total_edits: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        SearchConfig {
            strategy: RepairStrategy::Diagnostic,
            candidate_budget: 64,
            verification_budget: 64,
            max_depth: 4,
            max_total_edits: 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedEdit {
    pub module: String,
    pub function: String,
    pub changes: Vec<PatchChange>,
    pub provenance: Vec<SourceRelation>,
    /// Function content hash before the patch (the patch's version guard).
    pub original_function_hash: String,
    pub parent_fingerprint: String,
    pub program_fingerprint: String,
}

/// One unique verified program (a node of the search tree). `id` is its stable
/// index in `nodes`; `parent` is a node id or `None` for the root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeReport {
    pub id: usize,
    pub parent: Option<usize>,
    pub depth: usize,
    pub total_edits: usize,
    pub program_fingerprint: String,
    pub incoming: Option<AppliedEdit>,
    pub report: VerificationReport,
    /// Why this node was not expanded (if any).
    pub note: Option<String>,
}

/// One candidate proposal. An attempt that produced no new program is not a
/// node; it references the node it came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptReport {
    pub id: usize,
    pub parent: usize,
    pub patch: Option<CirPatch>,
    /// `verified` | `reused` | `denied` | `apply-error` | `static-invalid`.
    pub result: String,
    pub reused_node: Option<usize>,
    pub program_fingerprint: Option<String>,
    pub outcome: Option<Outcome>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceIdentity {
    pub crate_version: String,
    /// FNV fingerprint of the running executable, distinguishing this build
    /// from any other with the same crate version.
    pub binary_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectiveConfig {
    pub strategy: RepairStrategy,
    pub candidate_budget: usize,
    pub verification_budget: usize,
    pub max_depth: usize,
    pub max_total_edits: usize,
    pub bounds: AnalysisBounds,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchArtifact {
    pub schema_version: String,
    pub source: SourceIdentity,
    pub input_program: Program,
    pub frozen_contract: ContractSpec,
    pub effective_config: EffectiveConfig,
    pub nodes: Vec<NodeReport>,
    pub attempts: Vec<AttemptReport>,
    pub patch_chain: Vec<AppliedEdit>,
    pub accepted_program: Option<Program>,
    pub accepted_report: Option<VerificationReport>,
    pub outcome: RepairOutcome,
    pub stop_reason: String,
    pub saw_unknown: bool,
    pub truncation: Option<String>,
    pub counts: Counts,
    pub reproduce: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Counts {
    pub proposals: usize,
    pub unique_candidate_programs: usize,
    pub verification_calls: usize,
    pub cache_hits: usize,
    pub states_explored: usize,
    pub nodes: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchReport {
    pub strategy: RepairStrategy,
    pub outcome: RepairOutcome,
    pub stop_reason: String,
    pub saw_unknown: bool,
    pub truncation: Option<String>,
    pub effective_bounds: AnalysisBounds,
    pub proposals: usize,
    pub unique_programs: usize,
    pub verifications: usize,
    pub cache_hits: usize,
    pub states_explored: usize,
    pub nodes: Vec<NodeReport>,
    pub attempts: Vec<AttemptReport>,
    pub patch_chain: Vec<AppliedEdit>,
    #[serde(skip)]
    pub accepted_program: Option<Program>,
    #[serde(skip)]
    pub accepted_report: Option<VerificationReport>,
}

impl SearchReport {
    pub fn accepted_patch_count(&self) -> usize {
        self.patch_chain.len()
    }

    /// Build the self-contained, serializable artifact for this run.
    pub fn artifact(&self, program: &Program, spec: &ContractSpec) -> SearchArtifact {
        SearchArtifact {
            schema_version: "concir-repair-artifact-v1".into(),
            source: source_identity(),
            input_program: program.clone(),
            frozen_contract: spec.clone(),
            effective_config: EffectiveConfig {
                strategy: self.strategy,
                candidate_budget: 0, // overwritten below by caller-provided config
                verification_budget: 0,
                max_depth: 0,
                max_total_edits: 0,
                bounds: self.effective_bounds.clone(),
            },
            nodes: self.nodes.clone(),
            attempts: self.attempts.clone(),
            patch_chain: self.patch_chain.clone(),
            accepted_program: self.accepted_program.clone(),
            accepted_report: self.accepted_report.clone(),
            outcome: self.outcome,
            stop_reason: self.stop_reason.clone(),
            saw_unknown: self.saw_unknown,
            truncation: self.truncation.clone(),
            counts: Counts {
                proposals: self.proposals,
                unique_candidate_programs: self.unique_programs,
                verification_calls: self.verifications,
                cache_hits: self.cache_hits,
                states_explored: self.states_explored,
                nodes: self.nodes.len(),
            },
            reproduce: String::new(),
        }
    }

    pub fn artifact_with_config(
        &self,
        program: &Program,
        spec: &ContractSpec,
        config: &SearchConfig,
    ) -> SearchArtifact {
        let mut a = self.artifact(program, spec);
        a.effective_config = EffectiveConfig {
            strategy: config.strategy,
            candidate_budget: config.candidate_budget,
            verification_budget: config.verification_budget,
            max_depth: config.max_depth,
            max_total_edits: config.max_total_edits,
            bounds: self.effective_bounds.clone(),
        };
        a.reproduce = format!(
            "concir-backend repair <program.json> <contract.json> --strategy {}   # candidate_budget={} verification_budget={} max_depth={} max_total_edits={} bounds={:?}",
            match config.strategy {
                RepairStrategy::Single => "a",
                RepairStrategy::Composite => "b",
                RepairStrategy::Diagnostic => "c",
            },
            config.candidate_budget,
            config.verification_budget,
            config.max_depth,
            config.max_total_edits,
            self.effective_bounds
        );
        a
    }
}

pub fn program_fingerprint(program: &Program) -> String {
    let s = serde_json::to_string(program).unwrap_or_default();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn fnv_bytes(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

pub fn source_identity() -> SourceIdentity {
    static FP: OnceLock<String> = OnceLock::new();
    let binary_fingerprint = FP
        .get_or_init(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| std::fs::read(p).ok())
                .map(|b| fnv_bytes(&b))
                .unwrap_or_else(|| "unknown".into())
        })
        .clone();
    SourceIdentity {
        crate_version: env!("CARGO_PKG_VERSION").to_string(),
        binary_fingerprint,
    }
}

struct Node {
    parent: Option<usize>,
    depth: usize,
    total_edits: usize,
    program: Program,
    fingerprint: String,
    report: VerificationReport,
    incoming: Option<CirPatch>,
}

fn effective_bounds(spec: &ContractSpec) -> AnalysisBounds {
    (&spec.bounds).into()
}

fn empty_report(
    strategy: RepairStrategy,
    outcome: RepairOutcome,
    stop_reason: &str,
    bounds: AnalysisBounds,
) -> SearchReport {
    SearchReport {
        strategy,
        outcome,
        stop_reason: stop_reason.into(),
        saw_unknown: false,
        truncation: None,
        effective_bounds: bounds,
        proposals: 0,
        unique_programs: 0,
        verifications: 0,
        cache_hits: 0,
        states_explored: 0,
        nodes: Vec::new(),
        attempts: Vec::new(),
        patch_chain: Vec::new(),
        accepted_program: None,
        accepted_report: None,
    }
}

/// Run the budgeted search. Deterministic for a fixed input and budget.
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
            node_reports.push(node_from_report(
                0,
                None,
                0,
                0,
                root_fp,
                None,
                root_report,
            ));
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
                config, bounds, RepairOutcome::Invalid, "root-invalid", saw_unknown, None,
                proposals, verifications, cache_hits, states_explored, nodes, node_reports,
                attempts, Vec::new(), None, None,
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
                config, bounds, RepairOutcome::Unsupported, "root-unsupported", saw_unknown,
                None, proposals, verifications, cache_hits, states_explored, nodes,
                node_reports, attempts, Vec::new(), None, None,
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
                config, bounds, RepairOutcome::AnalysisUnknown, "root-unknown", saw_unknown,
                None, proposals, verifications, cache_hits, states_explored, nodes,
                node_reports, attempts, Vec::new(), None, None,
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
            RepairStrategy::Composite => {
                Box::new(LockOrderCompositeEnumerator::new(&node_program, scope, false))
            }
            RepairStrategy::Diagnostic => {
                Box::new(LockOrderCompositeEnumerator::new(&node_program, scope, true))
            }
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
                        return finish(
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
                AttemptFlow::Budget => {
                    verification_budget_hit = true;
                    break 'outer;
                }
            }
        }
    }

    let (outcome, stop_reason, truncation) = if candidate_budget_hit {
        (RepairOutcome::BudgetExhausted, "candidate-budget", None)
    } else if verification_budget_hit {
        (
            RepairOutcome::BudgetExhausted,
            "verification-budget",
            None,
        )
    } else if depth_truncated || edits_truncated {
        let reason = match (depth_truncated, edits_truncated) {
            (true, true) => "max-depth+max-total-edits",
            (true, false) => "max-depth",
            (false, true) => "max-total-edits",
            _ => unreachable!(),
        };
        (
            RepairOutcome::BudgetExhausted,
            reason,
            Some(reason.to_string()),
        )
    } else if saw_unknown {
        (RepairOutcome::AnalysisUnknown, "analysis-unknown", None)
    } else {
        (
            RepairOutcome::NoAcceptableCandidate,
            "no-acceptable-candidate",
            None,
        )
    };

    finish(
        config, bounds, outcome, stop_reason, saw_unknown, truncation, proposals,
        verifications, cache_hits, states_explored, nodes, node_reports, attempts, Vec::new(),
        None, None,
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
        accepted_program,
        accepted_report,
    }
}

enum AttemptFlow {
    Reused { node: usize, outcome: Outcome },
    Verified { node: usize, accepted: bool },
    Rejected { reason: String },
    Budget,
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
        return AttemptFlow::Budget;
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
    let mut nr = node_from_report(id, Some(parent_id), depth, total_edits, child_fp, Some(incoming), child_report);
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

fn applied_edit(
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
fn build_chain(nodes: &[Node], target: usize) -> Vec<AppliedEdit> {
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
                n.parent.map(|p| nodes[p].fingerprint.as_str()).unwrap_or(""),
                &n.fingerprint,
                n.parent.map(|p| &nodes[p].program).unwrap_or(&n.program),
            ));
        }
    }
    chain
}

/// Replay an artifact: rebuild every node by applying its incoming patch to its
/// parent, validate fingerprints, and re-verify the final program. Returns an
/// error string on any inconsistency (broken parent, bad patch base, bad input
/// fingerprint), never a silent success.
pub fn replay_artifact(artifact_json: &str) -> Result<ReplayResult, String> {
    let artifact: SearchArtifact =
        serde_json::from_str(artifact_json).map_err(|e| format!("artifact parse: {e}"))?;
    if artifact.schema_version != "concir-repair-artifact-v1" {
        return Err(format!("unknown artifact schema '{}'", artifact.schema_version));
    }
    // Verify the input against the frozen contract.
    let input_report = verify_program(&artifact.input_program, &artifact.frozen_contract, EngineKind::Petri);
    if program_fingerprint(&artifact.input_program)
        != artifact
            .nodes
            .first()
            .map(|n| n.program_fingerprint.clone())
            .unwrap_or_default()
    {
        return Err("root node fingerprint does not match input program".into());
    }
    // Rebuild nodes in id order (parents precede children).
    let mut rebuilt: BTreeMap<usize, Program> = BTreeMap::new();
    let mut reports: BTreeMap<usize, VerificationReport> = BTreeMap::new();
    for node in &artifact.nodes {
        let program = match node.parent {
            None => artifact.input_program.clone(),
            Some(p) => {
                let base = rebuilt
                    .get(&p)
                    .cloned()
                    .ok_or_else(|| format!("node {} references missing parent {}", node.id, p))?;
                let Some(edit) = &node.incoming else {
                    return Err(format!("node {} has a parent but no incoming patch", node.id));
                };
                let patch = CirPatch {
                    id: format!("replay:{}", node.id),
                    module: edit.module.clone(),
                    function: edit.function.clone(),
                    original_hash: edit.original_function_hash.clone(),
                    changes: edit.changes.clone(),
                    provenance: edit.provenance.clone(),
                };
                let (patched, _) = patch::apply(&base, &patch).map_err(|e| {
                    format!("node {}: patch does not apply to its parent: {e}", node.id)
                })?;
                if program_fingerprint(&patched) != node.program_fingerprint {
                    return Err(format!(
                        "node {}: rebuilt fingerprint does not match the recorded program",
                        node.id
                    ));
                }
                if edit.parent_fingerprint != program_fingerprint(&base) {
                    return Err(format!(
                        "node {}: parent fingerprint mismatch in incoming patch",
                        node.id
                    ));
                }
                patched
            }
        };
        let report = verify_program(&program, &artifact.frozen_contract, EngineKind::Petri);
        if report.outcome != node.report.outcome {
            return Err(format!(
                "node {}: rebuilt outcome {:?} != recorded {:?}",
                node.id, report.outcome, node.report.outcome
            ));
        }
        rebuilt.insert(node.id, program);
        reports.insert(node.id, report);
    }
    // Re-verify the accepted program and compare the outcome.
    let mut accepted_ok = None;
    if let Some(accepted) = &artifact.accepted_program {
        let re = verify_program(accepted, &artifact.frozen_contract, EngineKind::Petri);
        accepted_ok = Some(re.outcome == Outcome::Pass && re.complete);
        if accepted_ok != Some(true) {
            return Err(format!(
                "accepted program does not re-verify to a complete PASS (got {:?})",
                re.outcome
            ));
        }
    } else if artifact.outcome == RepairOutcome::Repaired {
        return Err("artifact reports repaired but has no accepted program".into());
    }
    Ok(ReplayResult {
        nodes: rebuilt.len(),
        input_outcome: input_report.outcome,
        accepted_ok,
        outcome: artifact.outcome,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplayResult {
    pub nodes: usize,
    pub input_outcome: Outcome,
    pub accepted_ok: Option<bool>,
    pub outcome: RepairOutcome,
}
