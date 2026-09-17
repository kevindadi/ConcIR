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
    /// The id of the accepted node (final chain node), if any.
    pub accepted_node: Option<usize>,
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
    /// The id of the accepted node (final chain node), if any.
    pub accepted_node: Option<usize>,
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
            accepted_node: self.accepted_node,
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
        accepted_node: None,
        accepted_program: None,
        accepted_report: None,
    }
}

/// The verifiable search facts that determine a run's terminal classification.
///
/// Both the online search and `replay_artifact` derive these facts (from the
/// live counters and from the recorded graph, respectively) and feed them to
/// [`TerminalFacts::classify`], so the outcome/stop_reason/truncation triple has
/// a single definition instead of two string branches that can drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalFacts {
    /// The root report is UNKNOWN and the search returned before any expansion.
    pub root_unknown: bool,
    /// The candidate proposal budget was reached.
    pub candidate_budget_hit: bool,
    /// The verification budget was reached (a generated candidate could not be
    /// verified).
    pub verification_budget_hit: bool,
    /// An expandable node was left unexpanded by `max_depth`.
    pub depth_truncated: bool,
    /// An expandable node passed the depth bound but was left unexpanded by
    /// `max_total_edits`.
    pub edits_truncated: bool,
    /// An UNKNOWN verification result was observed during the search.
    pub saw_unknown: bool,
}

impl TerminalFacts {
    /// The single source of truth for the terminal triple once the search loop
    /// ends without an accepted repair. The priority is: root-unknown, candidate
    /// budget, verification budget, depth+edits truncation, depth truncation,
    /// edit truncation, UNKNOWN, no acceptable candidate.
    pub fn classify(&self) -> (RepairOutcome, &'static str, Option<&'static str>) {
        if self.root_unknown {
            (RepairOutcome::AnalysisUnknown, "root-unknown", None)
        } else if self.candidate_budget_hit {
            (RepairOutcome::BudgetExhausted, "candidate-budget", None)
        } else if self.verification_budget_hit {
            (RepairOutcome::BudgetExhausted, "verification-budget", None)
        } else if self.depth_truncated && self.edits_truncated {
            (
                RepairOutcome::BudgetExhausted,
                "max-depth+max-total-edits",
                Some("max-depth+max-total-edits"),
            )
        } else if self.depth_truncated {
            (RepairOutcome::BudgetExhausted, "max-depth", Some("max-depth"))
        } else if self.edits_truncated {
            (
                RepairOutcome::BudgetExhausted,
                "max-total-edits",
                Some("max-total-edits"),
            )
        } else if self.saw_unknown {
            (RepairOutcome::AnalysisUnknown, "analysis-unknown", None)
        } else {
            (
                RepairOutcome::NoAcceptableCandidate,
                "no-acceptable-candidate",
                None,
            )
        }
    }
}

/// Whether a node was eligible for expansion under the strategy. The search
/// enqueues the root and, for the multi-step strategies, every verified FAIL
/// child; strategy A only ever expands the root. `replay_artifact` uses this to
/// distinguish "reached the bound" from "was stopped by the bound".
fn node_is_expandable(strategy: RepairStrategy, node: &NodeReport) -> bool {
    if node.report.outcome != Outcome::Fail {
        return false;
    }
    node.id == 0 || strategy != RepairStrategy::Single
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
        accepted_node: None,
        accepted_program,
        accepted_report,
    }
}

enum AttemptFlow {
    Reused { node: usize, outcome: Outcome },
    Verified { node: usize, accepted: bool },
    Rejected { reason: String },
    /// The candidate was generated, applied, and statically validated, but the
    /// verification budget was exhausted before it could be verified. No
    /// outcome or node is fabricated.
    Budget { fingerprint: String },
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

/// G1: every attempt's actual patch must explain its recorded result.
fn validate_attempts(artifact: &SearchArtifact, rebuilt: &BTreeMap<usize, Program>) -> Result<(), String> {
    // Nodes are verified in attempt order: root first, then one per verified
    // attempt. `verified_created` is the next expected node id and the running
    // verification count.
    let mut verified_created = if artifact.nodes.is_empty() { 0usize } else { 1usize };
    let budget = artifact.effective_config.verification_budget;
    for (i, a) in artifact.attempts.iter().enumerate() {
        let parent = rebuilt
            .get(&a.parent)
            .ok_or_else(|| format!("attempt {i}: parent node {} was not rebuilt", a.parent))?;
        let patch = a
            .patch
            .clone()
            .ok_or_else(|| format!("attempt {i}: no patch"))?;
        let allowed = patch::check_allowed(&artifact.frozen_contract.allowed_scope, &patch);
        match a.result.as_str() {
            "denied" => {
                if allowed.is_ok() {
                    return Err(format!(
                        "attempt {i}: recorded denied but the frozen contract allows the patch"
                    ));
                }
            }
            "apply-error" => {
                allowed.map_err(|e| {
                    format!("attempt {i}: recorded apply-error but it is disallowed: {e}")
                })?;
                if patch::apply(parent, &patch).is_ok() {
                    return Err(format!("attempt {i}: recorded apply-error but the patch applies"));
                }
            }
            "static-invalid" => {
                allowed.map_err(|e| {
                    format!("attempt {i}: recorded static-invalid but it is disallowed: {e}")
                })?;
                let (patched, _) = patch::apply(parent, &patch).map_err(|e| {
                    format!("attempt {i}: recorded static-invalid but it does not apply: {e}")
                })?;
                if validate::validate(&patched).valid {
                    return Err(format!(
                        "attempt {i}: recorded static-invalid but the program validates"
                    ));
                }
            }
            "verified" => {
                allowed.map_err(|e| format!("attempt {i}: verified patch is disallowed: {e}"))?;
                let (patched, _) = patch::apply(parent, &patch)
                    .map_err(|e| format!("attempt {i}: verified patch does not apply: {e}"))?;
                let fp = program_fingerprint(&patched);
                if a.program_fingerprint.as_deref() != Some(fp.as_str()) {
                    return Err(format!("attempt {i}: verified result fingerprint mismatch"));
                }
                let node = artifact
                    .nodes
                    .get(verified_created)
                    .filter(|n| n.program_fingerprint == fp && n.parent == Some(a.parent))
                    .ok_or_else(|| {
                        format!(
                            "attempt {i}: no node at verification position {verified_created} matches its result"
                        )
                    })?;
                let inc = node
                    .incoming
                    .as_ref()
                    .ok_or_else(|| format!("attempt {i}: verified node has no incoming patch"))?;
                if inc.module != patch.module
                    || inc.function != patch.function
                    || inc.changes != patch.changes
                    || inc.original_function_hash != patch.original_hash
                {
                    return Err(format!(
                        "attempt {i}: verified node incoming patch does not match the attempt patch"
                    ));
                }
                verified_created += 1;
            }
            "reused" => {
                allowed.map_err(|e| format!("attempt {i}: reused patch is disallowed: {e}"))?;
                let (patched, _) = patch::apply(parent, &patch)
                    .map_err(|e| format!("attempt {i}: reused patch does not apply: {e}"))?;
                let fp = program_fingerprint(&patched);
                if a.program_fingerprint.as_deref() != Some(fp.as_str()) {
                    return Err(format!("attempt {i}: reused result fingerprint mismatch"));
                }
                let reused = a.reused_node.unwrap();
                if reused >= verified_created {
                    return Err(format!(
                        "attempt {i}: reused node {reused} was not verified before this attempt"
                    ));
                }
                if artifact.nodes[reused].program_fingerprint != fp {
                    return Err(format!("attempt {i}: reused node fingerprint mismatch"));
                }
            }
            "budget-blocked" => {
                allowed.map_err(|e| {
                    format!("attempt {i}: budget-blocked patch is disallowed: {e}")
                })?;
                let (patched, _) = patch::apply(parent, &patch)
                    .map_err(|e| format!("attempt {i}: budget-blocked patch does not apply: {e}"))?;
                let fp = program_fingerprint(&patched);
                if a.program_fingerprint.as_deref() != Some(fp.as_str()) {
                    return Err(format!("attempt {i}: budget-blocked result fingerprint mismatch"));
                }
                if !validate::validate(&patched).valid {
                    return Err(format!(
                        "attempt {i}: budget-blocked candidate is not statically valid"
                    ));
                }
                if artifact.nodes.iter().any(|n| n.program_fingerprint == fp) {
                    return Err(format!(
                        "attempt {i}: budget-blocked candidate was already verified as a node"
                    ));
                }
                if verified_created < budget {
                    return Err(format!(
                        "attempt {i}: budget-blocked but the verification budget is not exhausted"
                    ));
                }
            }
            _ => {}
        }
    }
    if verified_created != artifact.nodes.len() {
        return Err(format!(
            "verified attempts account for {verified_created} nodes but {} are recorded",
            artifact.nodes.len()
        ));
    }
    Ok(())
}

/// Derive the terminal search facts from an artifact's recorded graph. This
/// mirrors the live search's expansion decisions: the root is expandable when
/// its report FAILs, and, for the multi-step strategies, so is every verified
/// FAIL node; strategy A never expands beyond the root. A node is "reached the
/// bound" only when such an expandable node is left unexpanded by the budget.
fn derive_terminal_facts(a: &SearchArtifact) -> TerminalFacts {
    let cfg = &a.effective_config;
    let root_unknown = a.nodes.len() == 1 && a.nodes[0].report.outcome == Outcome::Unknown;
    let saw_unknown = a.nodes.iter().any(|n| n.report.outcome == Outcome::Unknown);
    let verification_budget_hit = a.attempts.iter().any(|x| x.result == "budget-blocked");
    let expandable = |n: &NodeReport| node_is_expandable(cfg.strategy, n);
    let depth_truncated = a.nodes.iter().any(|n| expandable(n) && n.depth >= cfg.max_depth);
    let edits_truncated = a.nodes.iter().any(|n| {
        expandable(n) && n.depth < cfg.max_depth && n.total_edits >= cfg.max_total_edits
    });
    let can_expand = a.nodes.iter().any(|n| {
        expandable(n) && n.depth < cfg.max_depth && n.total_edits < cfg.max_total_edits
    });
    // The candidate budget is only checked while expanding an allowed node, and
    // the verification-budget break happens before that check.
    let candidate_budget_hit = !verification_budget_hit
        && a.counts.proposals >= cfg.candidate_budget
        && can_expand;
    TerminalFacts {
        root_unknown,
        candidate_budget_hit,
        verification_budget_hit,
        depth_truncated,
        edits_truncated,
        saw_unknown,
    }
}

/// G3: the terminal classification must be supported by recorded facts. The
/// post-loop outcomes are re-derived through the same [`TerminalFacts`] used by
/// the online search; the whole outcome/stop_reason/truncation/saw_unknown
/// quadruple is compared, never the strings in isolation.
fn validate_outcome_evidence(a: &SearchArtifact) -> Result<(), String> {
    let facts = derive_terminal_facts(a);
    if a.saw_unknown != facts.saw_unknown {
        return Err(format!(
            "saw_unknown={} does not match the recorded UNKNOWN results ({})",
            a.saw_unknown, facts.saw_unknown
        ));
    }
    let simple = |want: &str| -> Result<(), String> {
        if a.stop_reason != want {
            return Err(format!(
                "outcome {:?} requires stop_reason '{want}', got '{}'",
                a.outcome, a.stop_reason
            ));
        }
        if a.truncation.is_some() {
            return Err(format!("outcome {:?} must not record truncation", a.outcome));
        }
        Ok(())
    };
    match a.outcome {
        RepairOutcome::Repaired => simple("solved")?,
        RepairOutcome::AlreadySatisfied => simple("already-satisfied")?,
        RepairOutcome::Invalid => simple("root-invalid")?,
        RepairOutcome::Unsupported => simple("root-unsupported")?,
        RepairOutcome::InvalidConfig => simple("verification-budget-zero")?,
        RepairOutcome::AnalysisUnknown
        | RepairOutcome::NoAcceptableCandidate
        | RepairOutcome::BudgetExhausted => {
            let (outcome, stop_reason, truncation) = facts.classify();
            if a.outcome != outcome {
                return Err(format!(
                    "outcome {:?} is not supported by the recorded search facts (expected {:?})",
                    a.outcome, outcome
                ));
            }
            if a.stop_reason != stop_reason {
                return Err(format!(
                    "stop_reason '{}' is not supported by the recorded search facts (expected '{stop_reason}')",
                    a.stop_reason
                ));
            }
            let expected = truncation.map(str::to_string);
            if a.truncation != expected {
                return Err(format!(
                    "truncation {:?} does not match the recorded search facts (expected {expected:?})",
                    a.truncation
                ));
            }
        }
    }
    Ok(())
}

/// Replay an artifact: rebuild every node by applying its incoming patch to its
/// parent, validate fingerprints, and re-verify the final program. Returns an
/// error string on any inconsistency (broken parent, bad patch base, bad input
/// fingerprint), never a silent success.
/// Structural normalization of one diagnostic: the fields that carry semantic
/// content (property, verdict, counterexample bindings, blocking facts,
/// instances, CIR statements, completeness). Human-readable prose is excluded.
fn diag_norm(d: &crate::explore::DiagnosticRecord) -> serde_json::Value {
    serde_json::json!({
        "property": d.property,
        "outcome": d.outcome,
        "complete": d.complete,
        "counterexample": d.counterexample.iter().map(|s| serde_json::json!({
            "module": s.origin.module.0,
            "function": s.origin.function.0,
            "sid": s.origin.sid,
            "phase": s.origin.phase,
            "thread": s.thread,
            "frame": s.frame,
        })).collect::<Vec<_>>(),
        "blocked": d.blocked.iter().map(|b| serde_json::json!({
            "thread": b.thread,
            "kind": b.kind,
            "resource": b.resource,
            "resource_name": b.resource_name,
            "holder": b.holder,
            "waiting": b.waiting,
        })).collect::<Vec<_>>(),
        "instances": d.final_instances.iter().map(|i| serde_json::json!({
            "thread": i.thread,
            "frame": i.frame,
            "function": i.function,
            "sid": i.sid,
            "status": i.status,
        })).collect::<Vec<_>>(),
        "cir_statements": d.cir_statements.iter().map(|c| serde_json::json!({
            "module": c.module,
            "function": c.function,
            "sid": c.sid,
        })).collect::<Vec<_>>(),
    })
}

/// Compare the normative fields of two verification reports, including costs,
/// the analysis-started marker, and the structured diagnostic evidence. The
/// producing binary may differ, so `source.binary_fingerprint` is not compared;
/// every field that determines the verdict or the failure evidence is.
fn reports_match(a: &VerificationReport, b: &VerificationReport, what: &str) -> Result<(), String> {
    let prop = |r: &VerificationReport| -> Vec<(String, Outcome)> {
        r.properties.iter().map(|p| (p.id.clone(), p.outcome)).collect()
    };
    if a.outcome != b.outcome {
        return Err(format!("{what}: outcome {:?} != recorded {:?}", a.outcome, b.outcome));
    }
    if a.complete != b.complete {
        return Err(format!("{what}: complete {} != recorded {}", a.complete, b.complete));
    }
    if a.analysis_started != b.analysis_started {
        return Err(format!("{what}: analysis_started mismatch"));
    }
    if a.states_explored != b.states_explored {
        return Err(format!(
            "{what}: states_explored {} != recorded {}",
            a.states_explored, b.states_explored
        ));
    }
    if a.transitions_explored != b.transitions_explored {
        return Err(format!(
            "{what}: transitions_explored {} != recorded {}",
            a.transitions_explored, b.transitions_explored
        ));
    }
    if a.model_fingerprint != b.model_fingerprint {
        return Err(format!("{what}: model fingerprint mismatch"));
    }
    if a.contract_fingerprint != b.contract_fingerprint {
        return Err(format!("{what}: contract fingerprint mismatch"));
    }
    if a.assumptions != b.assumptions {
        return Err(format!("{what}: assumptions mismatch"));
    }
    if a.bounds != b.bounds {
        return Err(format!("{what}: analysis bounds mismatch"));
    }
    if prop(a) != prop(b) {
        return Err(format!(
            "{what}: property verdicts differ: {:?} != recorded {:?}",
            prop(a),
            prop(b)
        ));
    }
    let dn = |r: &VerificationReport| -> Vec<serde_json::Value> {
        r.diagnostics.iter().map(diag_norm).collect()
    };
    if dn(a) != dn(b) {
        return Err(format!("{what}: structured diagnostic evidence differs"));
    }
    let codes = |v: &[crate::sem::outcome::Unsupported]| -> Vec<(String, Option<String>)> {
        v.iter().map(|u| (u.construct.clone(), u.location.clone())).collect()
    };
    if codes(&a.unsupported) != codes(&b.unsupported) {
        return Err(format!("{what}: unsupported construct set differs"));
    }
    let icodes = |v: &[crate::sem::outcome::Invalid]| -> Vec<(String, Option<String>)> {
        v.iter().map(|i| (i.code.clone(), i.location.clone())).collect()
    };
    if icodes(&a.invalid) != icodes(&b.invalid) {
        return Err(format!("{what}: invalid set differs"));
    }
    let be = |r: &VerificationReport| -> Vec<serde_json::Value> {
        r.boundary_events
            .iter()
            .map(|b| {
                serde_json::json!({
                    "kind": format!("{:?}", b.kind),
                    "thread": b.thread,
                    "frame": b.frame,
                    "function": b.function,
                })
            })
            .collect()
    };
    if be(a) != be(b) {
        return Err(format!("{what}: boundary event evidence differs"));
    }
    Ok(())
}

fn patch_from_edit(id: String, edit: &AppliedEdit) -> CirPatch {
    CirPatch {
        id,
        module: edit.module.clone(),
        function: edit.function.clone(),
        original_hash: edit.original_function_hash.clone(),
        changes: edit.changes.clone(),
        provenance: edit.provenance.clone(),
    }
}

/// Structural validation, done before any expensive verification.
fn validate_structure(artifact: &SearchArtifact) -> Result<(), String> {
    if artifact.schema_version != "concir-repair-artifact-v1" {
        return Err(format!("unknown artifact schema '{}'", artifact.schema_version));
    }
    if artifact.source.crate_version.is_empty() || artifact.source.binary_fingerprint.is_empty() {
        return Err("artifact is missing source identity".into());
    }
    // Nodes.
    for (i, node) in artifact.nodes.iter().enumerate() {
        if node.id != i {
            return Err(format!("node at position {i} has non-sequential id {}", node.id));
        }
        match (i, node.parent) {
            (0, None) => {
                if node.depth != 0 || node.total_edits != 0 || node.incoming.is_some() {
                    return Err("root node must have depth 0, 0 edits, and no incoming patch".into());
                }
            }
            (0, Some(_)) => return Err("root node must not have a parent".into()),
            (_, None) => return Err(format!("node {i} has no parent but is not the root")),
            (_, Some(p)) => {
                if p >= i {
                    return Err(format!("node {i} parent {p} is not an earlier node (cycle)"));
                }
                let parent = &artifact.nodes[p];
                let Some(edit) = &node.incoming else {
                    return Err(format!("node {i} has a parent but no incoming patch"));
                };
                if node.depth != parent.depth + 1 {
                    return Err(format!("node {i} depth {} != parent depth + 1", node.depth));
                }
                if node.total_edits != parent.total_edits + edit.changes.len() {
                    return Err(format!("node {i} total_edits is inconsistent with its edit count"));
                }
            }
        }
    }
    // Effective bounds must equal the frozen contract's bounds.
    let expected_bounds: AnalysisBounds = (&artifact.frozen_contract.bounds).into();
    if artifact.effective_config.bounds != expected_bounds {
        return Err(format!(
            "effective_config.bounds {:?} != frozen contract bounds {:?}",
            artifact.effective_config.bounds, expected_bounds
        ));
    }
    // Attempts.
    for (i, a) in artifact.attempts.iter().enumerate() {
        if a.id != i {
            return Err(format!("attempt at position {i} has non-sequential id {}", a.id));
        }
        if a.parent >= artifact.nodes.len() {
            return Err(format!("attempt {i} references missing parent node {}", a.parent));
        }
        if a.patch.is_none() {
            return Err(format!("attempt {i} has no patch"));
        }
        match a.result.as_str() {
            "verified" => {
                let fp = a
                    .program_fingerprint
                    .as_ref()
                    .ok_or_else(|| format!("verified attempt {i} has no program fingerprint"))?;
                let node = artifact
                    .nodes
                    .iter()
                    .find(|n| &n.program_fingerprint == fp)
                    .ok_or_else(|| format!("verified attempt {i} references no node"))?;
                if node.parent != Some(a.parent) {
                    return Err(format!(
                        "verified attempt {i} parent {} != node {} parent {:?}",
                        a.parent, node.id, node.parent
                    ));
                }
                if a.outcome != Some(node.report.outcome) {
                    return Err(format!("verified attempt {i} outcome != node outcome"));
                }
                if a.reused_node.is_some() {
                    return Err(format!("verified attempt {i} must not set reused_node"));
                }
            }
            "reused" => {
                let reused = a
                    .reused_node
                    .ok_or_else(|| format!("reused attempt {i} has no reused_node"))?;
                let node = artifact
                    .nodes
                    .get(reused)
                    .ok_or_else(|| format!("reused attempt {i} references missing node {reused}"))?;
                if a.program_fingerprint.as_deref() != Some(node.program_fingerprint.as_str()) {
                    return Err(format!("reused attempt {i} fingerprint != reused node fingerprint"));
                }
                if a.outcome != Some(node.report.outcome) {
                    return Err(format!("reused attempt {i} outcome != reused node outcome"));
                }
            }
            "budget-blocked" => {
                if a.program_fingerprint.is_none() {
                    return Err(format!("budget-blocked attempt {i} has no program fingerprint"));
                }
                if a.reused_node.is_some() || a.outcome.is_some() {
                    return Err(format!("budget-blocked attempt {i} must have no node or outcome"));
                }
            }
            "denied" | "apply-error" | "static-invalid" => {
                if a.program_fingerprint.is_some() || a.reused_node.is_some() || a.outcome.is_some() {
                    return Err(format!(
                        "rejected attempt {i} must have no fingerprint, node, or outcome"
                    ));
                }
            }
            other => return Err(format!("attempt {i} has unknown result '{other}'")),
        }
    }
    // Derivable counts.
    let proposals = artifact.attempts.len();
    let unique = artifact.nodes.len();
    let cache_hits = artifact
        .attempts
        .iter()
        .filter(|a| a.result == "reused")
        .count();
    let states: usize = artifact.nodes.iter().map(|n| n.report.states_explored).sum();
    let counts = &artifact.counts;
    if counts.proposals != proposals {
        return Err(format!("counts.proposals {} != attempts {}", counts.proposals, proposals));
    }
    if counts.unique_candidate_programs != unique {
        return Err(format!(
            "counts.unique_candidate_programs {} != nodes {}",
            counts.unique_candidate_programs, unique
        ));
    }
    if counts.verification_calls != unique {
        return Err(format!(
            "counts.verification_calls {} != verified nodes {}",
            counts.verification_calls, unique
        ));
    }
    if counts.cache_hits != cache_hits {
        return Err(format!("counts.cache_hits {} != reused attempts {}", counts.cache_hits, cache_hits));
    }
    if counts.states_explored != states {
        return Err(format!(
            "counts.states_explored {} != sum of node states {}",
            counts.states_explored, states
        ));
    }
    if counts.nodes != unique {
        return Err(format!("counts.nodes {} != nodes {}", counts.nodes, unique));
    }
    // Budget coherence.
    let cfg = &artifact.effective_config;
    if cfg.candidate_budget < proposals {
        return Err("candidate_budget is smaller than the recorded proposals".into());
    }
    if cfg.verification_budget < counts.verification_calls {
        return Err("verification_budget is smaller than the recorded verification calls".into());
    }
    let max_depth = artifact.nodes.iter().map(|n| n.depth).max().unwrap_or(0);
    let max_edits = artifact.nodes.iter().map(|n| n.total_edits).max().unwrap_or(0);
    if max_depth > cfg.max_depth {
        return Err("a node is deeper than max_depth".into());
    }
    if max_edits > cfg.max_total_edits {
        return Err("a node has more edits than max_total_edits".into());
    }
    if matches!(cfg.strategy, RepairStrategy::Single) && max_depth > 1 {
        return Err("strategy single produced a node deeper than one edit".into());
    }
    // Strategy A only expands the root: every patch must start at node 0. This
    // binds the recorded strategy to the graph the terminal facts are derived
    // from.
    if matches!(cfg.strategy, RepairStrategy::Single) {
        if artifact.nodes.iter().any(|n| n.id != 0 && n.parent != Some(0)) {
            return Err("strategy single has a node that does not descend from the root".into());
        }
        if artifact.attempts.iter().any(|x| x.parent != 0) {
            return Err("strategy single has an attempt not rooted at node 0".into());
        }
    }
    if cfg.verification_budget == 0 && (!artifact.nodes.is_empty() || counts.verification_calls != 0)
    {
        return Err("verification_budget=0 must have no verified nodes".into());
    }
    // Outcome / accepted consistency.
    match artifact.outcome {
        RepairOutcome::Repaired => {
            if artifact.accepted_program.is_none()
                || artifact.accepted_report.is_none()
                || artifact.accepted_node.is_none()
                || artifact.patch_chain.is_empty()
            {
                return Err("repaired artifact must have an accepted node, chain, program, and report".into());
            }
        }
        RepairOutcome::AlreadySatisfied => {
            if artifact.nodes.len() != 1
                || artifact.nodes[0].report.outcome != Outcome::Pass
                || !artifact.nodes[0].report.complete
                || !artifact.patch_chain.is_empty()
                || artifact.accepted_program.is_some()
            {
                return Err("already_satisfied must have one PASS root and no accepted result".into());
            }
        }
        RepairOutcome::Invalid | RepairOutcome::Unsupported => {
            if artifact.nodes.len() != 1
                || artifact.accepted_program.is_some()
                || !artifact.patch_chain.is_empty()
            {
                return Err(format!(
                    "{:?} must have a single root report and no accepted result",
                    artifact.outcome
                ));
            }
            let expected = if artifact.outcome == RepairOutcome::Invalid {
                Outcome::Invalid
            } else {
                Outcome::Unsupported
            };
            if artifact.nodes[0].report.outcome != expected {
                return Err("root report outcome is incompatible with the artifact outcome".into());
            }
        }
        RepairOutcome::InvalidConfig => {
            if !artifact.nodes.is_empty() || counts.verification_calls != 0 {
                return Err("invalid_config must have no nodes or verifications".into());
            }
        }
        RepairOutcome::AnalysisUnknown | RepairOutcome::NoAcceptableCandidate
        | RepairOutcome::BudgetExhausted => {
            if artifact.accepted_program.is_some() || !artifact.patch_chain.is_empty() {
                return Err(format!("{:?} must not carry an accepted result", artifact.outcome));
            }
            if artifact.nodes.is_empty() {
                return Err(format!("{:?} must retain the root report", artifact.outcome));
            }
        }
    }
    Ok(())
}

/// Replay an artifact: validate structure, rebuild every node by applying its
/// incoming patch (with the frozen contract's permissions), compare the
/// normative reports, replay the accepted patch chain, and re-verify the
/// accepted program. Any inconsistency is an explicit error.
pub fn replay_artifact(artifact_json: &str) -> Result<ReplayResult, String> {
    let artifact: SearchArtifact =
        serde_json::from_str(artifact_json).map_err(|e| format!("artifact parse: {e}"))?;
    validate_structure(&artifact)?;

    // Re-verify the input and compare it to the root node's report.
    let input_report =
        verify_program(&artifact.input_program, &artifact.frozen_contract, EngineKind::Petri);
    let root_fp = program_fingerprint(&artifact.input_program);
    if artifact.nodes.is_empty() {
        // InvalidConfig: nothing else to check.
        if artifact.outcome != RepairOutcome::InvalidConfig {
            return Err("artifact has no nodes but is not invalid_config".into());
        }
        return Ok(ReplayResult {
            nodes: 0,
            input_outcome: input_report.outcome,
            accepted_ok: None,
            accepted_node: None,
            chain_len: 0,
            outcome: artifact.outcome,
        });
    }
    if artifact.nodes[0].program_fingerprint != root_fp {
        return Err("root node fingerprint does not match the input program".into());
    }
    reports_match(&input_report, &artifact.nodes[0].report, "root node")?;

    // Rebuild nodes in id order, re-checking permissions and fingerprints.
    let mut rebuilt: BTreeMap<usize, Program> = BTreeMap::new();
    let mut fresh: BTreeMap<usize, VerificationReport> = BTreeMap::new();
    for node in &artifact.nodes {
        let program = match node.parent {
            None => artifact.input_program.clone(),
            Some(p) => {
                let base = rebuilt
                    .get(&p)
                    .cloned()
                    .ok_or_else(|| format!("node {} references missing parent {}", node.id, p))?;
                let edit = node
                    .incoming
                    .as_ref()
                    .ok_or_else(|| format!("node {} has a parent but no incoming patch", node.id))?;
                let patch = patch_from_edit(format!("replay:node:{}", node.id), edit);
                patch::check_allowed(&artifact.frozen_contract.allowed_scope, &patch)
                    .map_err(|e| format!("node {}: patch is not allowed by the frozen contract: {e}", node.id))?;
                let base_fp = program_fingerprint(&base);
                if edit.parent_fingerprint != base_fp {
                    return Err(format!("node {}: incoming parent fingerprint mismatch", node.id));
                }
                let (patched, _) = patch::apply(&base, &patch)
                    .map_err(|e| format!("node {}: patch does not apply to its parent: {e}", node.id))?;
                if program_fingerprint(&patched) != node.program_fingerprint {
                    return Err(format!("node {}: rebuilt fingerprint mismatch", node.id));
                }
                if edit.program_fingerprint != node.program_fingerprint {
                    return Err(format!("node {}: incoming result fingerprint mismatch", node.id));
                }
                patched
            }
        };
        let report = verify_program(&program, &artifact.frozen_contract, EngineKind::Petri);
        reports_match(&report, &node.report, &format!("node {}", node.id))?;
        fresh.insert(node.id, report);
        rebuilt.insert(node.id, program);
    }

    // G1: each attempt's patch must explain its recorded result.
    validate_attempts(&artifact, &rebuilt)?;
    // G3: the terminal classification must be supported by the recorded facts.
    validate_outcome_evidence(&artifact)?;
    // G2: the total cost must match the freshly re-verified per-node costs.
    let fresh_states: usize = fresh.values().map(|r| r.states_explored).sum();
    if fresh_states != artifact.counts.states_explored {
        return Err(format!(
            "counts.states_explored {} != freshly re-verified sum {}",
            artifact.counts.states_explored, fresh_states
        ));
    }

    // Replay the accepted patch chain from the root.
    let mut accepted_ok = None;
    let mut accepted_node = None;
    let mut chain_len = artifact.patch_chain.len();
    if artifact.outcome == RepairOutcome::Repaired {
        let node_id = artifact
            .accepted_node
            .ok_or_else(|| "repaired artifact has no accepted_node".to_string())?;
        let node = artifact
            .nodes
            .get(node_id)
            .ok_or_else(|| format!("accepted_node {node_id} is out of range"))?;
        if node.depth != artifact.patch_chain.len() {
            return Err(format!(
                "accepted node depth {} != patch chain length {}",
                node.depth,
                artifact.patch_chain.len()
            ));
        }
        let mut program = artifact.input_program.clone();
        let mut running_fp = program_fingerprint(&program);
        let mut running_edits = 0usize;
        for (i, edit) in artifact.patch_chain.iter().enumerate() {
            let patch = patch_from_edit(format!("replay:chain:{i}"), edit);
            patch::check_allowed(&artifact.frozen_contract.allowed_scope, &patch)
                .map_err(|e| format!("patch_chain[{i}] is not allowed by the frozen contract: {e}"))?;
            if edit.parent_fingerprint != running_fp {
                return Err(format!("patch_chain[{i}] parent fingerprint mismatch"));
            }
            let (patched, _) = patch::apply(&program, &patch)
                .map_err(|e| format!("patch_chain[{i}] does not apply: {e}"))?;
            let fp = program_fingerprint(&patched);
            if fp != edit.program_fingerprint {
                return Err(format!("patch_chain[{i}] result fingerprint mismatch"));
            }
            running_edits += edit.changes.len();
            program = patched;
            running_fp = fp;
        }
        if running_edits != node.total_edits {
            return Err(format!(
                "patch chain edits {running_edits} != accepted node total_edits {}",
                node.total_edits
            ));
        }
        if running_fp != node.program_fingerprint {
            return Err("patch chain end does not match the accepted node fingerprint".into());
        }
        let accepted = artifact
            .accepted_program
            .as_ref()
            .ok_or_else(|| "repaired artifact has no accepted_program".to_string())?;
        if program_fingerprint(accepted) != running_fp {
            return Err("patch chain result does not match accepted_program".into());
        }
        // Re-verify the accepted program and compare to both reports.
        let re = verify_program(accepted, &artifact.frozen_contract, EngineKind::Petri);
        let accepted_report = artifact
            .accepted_report
            .as_ref()
            .ok_or_else(|| "repaired artifact has no accepted_report".to_string())?;
        reports_match(&re, accepted_report, "accepted report")?;
        reports_match(&re, &node.report, "accepted node report")?;
        if !(re.outcome == Outcome::Pass && re.complete) {
            return Err(format!(
                "accepted program does not re-verify to a complete PASS (got {:?})",
                re.outcome
            ));
        }
        accepted_ok = Some(true);
        accepted_node = Some(node_id);
        chain_len = artifact.patch_chain.len();
    } else if artifact.accepted_program.is_some() || artifact.accepted_report.is_some() {
        return Err("non-repaired artifact must not carry an accepted program or report".into());
    }

    Ok(ReplayResult {
        nodes: rebuilt.len(),
        input_outcome: input_report.outcome,
        accepted_ok,
        accepted_node,
        chain_len,
        outcome: artifact.outcome,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplayResult {
    pub nodes: usize,
    pub input_outcome: Outcome,
    pub accepted_ok: Option<bool>,
    pub accepted_node: Option<usize>,
    pub chain_len: usize,
    pub outcome: RepairOutcome,
}
