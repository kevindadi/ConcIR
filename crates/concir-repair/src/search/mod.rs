mod replay;
mod strategies;

pub use replay::{replay_artifact, reports_match, ReplayResult};
pub use strategies::run_search;

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

pub(super) struct Node {
    pub(super) parent: Option<usize>,
    pub(super) depth: usize,
    pub(super) total_edits: usize,
    pub(super) program: Program,
    pub(super) fingerprint: String,
    pub(super) report: VerificationReport,
    pub(super) incoming: Option<CirPatch>,
}

pub(super) fn effective_bounds(spec: &ContractSpec) -> AnalysisBounds {
    (&spec.bounds).into()
}

pub(super) fn empty_report(
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
            (
                RepairOutcome::BudgetExhausted,
                "max-depth",
                Some("max-depth"),
            )
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
pub(super) fn node_is_expandable(strategy: RepairStrategy, node: &NodeReport) -> bool {
    if node.report.outcome != Outcome::Fail {
        return false;
    }
    node.id == 0 || strategy != RepairStrategy::Single
}
