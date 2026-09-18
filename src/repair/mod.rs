//! Phase 4: structured CIR patches and the LLM-free iterative repair loop.
//!
//! The [`ContractSpec`](crate::explore::contract::ContractSpec) is the frozen,
//! symbolic contract. It is re-resolved against every candidate program, so
//! statement/scope targets are re-bound (never stale body indices). Every
//! candidate — automatic or file-provided — goes through the same permission
//! check, static validation, supportability, re-binding, re-translation, and
//! full verification.
//!
//! Old-counterexample replay is intentionally *not* an acceptance criterion.

pub mod benchmark;
pub mod candidates;
pub mod patch;
pub mod search;

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::ast::Program;
use crate::explore::contract::{ContractError, ContractSpec};
use crate::explore::{self, EngineKind, VerificationReport};
use crate::sem::outcome::Outcome;
use crate::validate;

use candidates::{CandidateProvider, RepairContext};
use patch::CirPatch;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairOutcome {
    Repaired,
    AlreadySatisfied,
    NoAcceptableCandidate,
    BudgetExhausted,
    AnalysisUnknown,
    Invalid,
    Unsupported,
    /// The requested configuration is invalid (e.g. a zero verification
    /// budget). No analysis was run.
    InvalidConfig,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoundRecord {
    pub round: usize,
    pub candidate: String,
    pub accepted: bool,
    pub reason: String,
    pub patched_outcome: Option<Outcome>,
    pub diff: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepairReport {
    pub outcome: RepairOutcome,
    pub candidates_tried: usize,
    pub rounds: Vec<RoundRecord>,
    #[serde(skip)]
    pub accepted: Option<CirPatch>,
    #[serde(skip)]
    pub accepted_program: Option<Program>,
    #[serde(skip)]
    pub accepted_report: Option<VerificationReport>,
}

fn content_key(patch: &CirPatch) -> String {
    let changes = serde_json::to_string(&patch.changes).unwrap_or_default();
    format!("{}::{}::{changes}", patch.module, patch.function)
}

/// Run the deterministic repair loop.
pub fn run_repair(
    program: &Program,
    spec: &ContractSpec,
    provider: &mut dyn CandidateProvider,
    budget: usize,
) -> RepairReport {
    let current = program.clone();
    let mut tried: HashSet<String> = HashSet::new();
    let mut rounds = Vec::new();
    let mut saw_unknown = false;
    let mut candidates_tried = 0usize;

    for round in 0..budget {
        let ctx = RepairContext {
            program: &current,
            spec,
            round,
            depth: 0,
            report: None,
            history: &[],
        };
        let Some(candidate) = provider.next_candidate(&ctx) else {
            return RepairReport {
                outcome: if saw_unknown {
                    RepairOutcome::AnalysisUnknown
                } else {
                    RepairOutcome::NoAcceptableCandidate
                },
                candidates_tried,
                rounds,
                accepted: None,
                accepted_program: None,
                accepted_report: None,
            };
        };
        // Deduplicate by normalized change content, not by a user-chosen id.
        if !tried.insert(content_key(&candidate)) {
            rounds.push(RoundRecord {
                round,
                candidate: candidate.id.clone(),
                accepted: false,
                reason: "duplicate candidate skipped".into(),
                patched_outcome: None,
                diff: None,
            });
            continue;
        }
        candidates_tried += 1;

        // Unified, provider-independent permission check.
        if let Err(e) = patch::check_allowed(&spec.allowed_scope, &candidate) {
            rounds.push(reject(
                round,
                &candidate,
                &format!("disallowed: {e}"),
                None,
                None,
            ));
            continue;
        }

        let (patched, diff) = match patch::apply(&current, &candidate) {
            Ok(v) => v,
            Err(e) => {
                rounds.push(reject(
                    round,
                    &candidate,
                    &format!("patch rejected: {e}"),
                    None,
                    None,
                ));
                continue;
            }
        };

        let static_report = validate::validate(&patched);
        if !static_report.valid {
            let n = static_report
                .diagnostics
                .iter()
                .filter(|d| d.severity == crate::diagnostic::Severity::Error)
                .count();
            rounds.push(reject(
                round,
                &candidate,
                &format!("CIR static validation failed with {n} error(s)"),
                None,
                Some(diff),
            ));
            continue;
        }

        let sem = match crate::sem::program::lower(&patched) {
            Ok(s) => s,
            Err(e) => {
                rounds.push(reject(
                    round,
                    &candidate,
                    &format!("lowering failed: {e}"),
                    None,
                    Some(diff),
                ));
                continue;
            }
        };
        if !sem.unsupported().is_empty() {
            let names: Vec<String> = sem
                .unsupported()
                .iter()
                .map(|u| u.construct.clone())
                .collect();
            rounds.push(reject(
                round,
                &candidate,
                &format!("unsupported constructs present: {}", names.join(", ")),
                None,
                Some(diff),
            ));
            continue;
        }

        // Re-bind the frozen symbolic contract against the *new* program. A
        // deleted target fails here instead of being silently redirected.
        let rebound = match spec.resolve(&sem) {
            Ok(c) => c,
            Err(ContractError::Unsupported(m)) => {
                rounds.push(reject(
                    round,
                    &candidate,
                    &format!("contract unsupported after patch: {m}"),
                    Some(Outcome::Unsupported),
                    Some(diff),
                ));
                continue;
            }
            Err(ContractError::Invalid(m)) => {
                rounds.push(reject(
                    round,
                    &candidate,
                    &format!("contract cannot be re-bound after patch: {m}"),
                    Some(Outcome::Invalid),
                    Some(diff),
                ));
                continue;
            }
        };

        let report = explore::verify_program(&patched, spec, EngineKind::Petri);
        match report.outcome {
            Outcome::Pass => {
                rounds.push(RoundRecord {
                    round,
                    candidate: candidate.id.clone(),
                    accepted: true,
                    reason: "all required properties and preserved behaviour hold".into(),
                    patched_outcome: Some(Outcome::Pass),
                    diff: Some(diff),
                });
                let _ = rebound;
                return RepairReport {
                    outcome: RepairOutcome::Repaired,
                    candidates_tried,
                    rounds,
                    accepted: Some(candidate),
                    accepted_program: Some(patched),
                    accepted_report: Some(report),
                };
            }
            Outcome::Fail => {
                let summary = report
                    .diagnostics
                    .first()
                    .map(|d| format!("{}: {}", d.property, d.message))
                    .unwrap_or_else(|| "verification failed".into());
                rounds.push(reject(
                    round,
                    &candidate,
                    &format!("verification FAIL: {summary}"),
                    Some(Outcome::Fail),
                    Some(diff),
                ));
            }
            Outcome::Unknown => {
                saw_unknown = true;
                rounds.push(reject(
                    round,
                    &candidate,
                    "verification UNKNOWN: incomplete search; not accepted",
                    Some(Outcome::Unknown),
                    Some(diff),
                ));
            }
            Outcome::Invalid => {
                rounds.push(reject(
                    round,
                    &candidate,
                    "verification INVALID: program has a semantic error",
                    Some(Outcome::Invalid),
                    Some(diff),
                ));
            }
            Outcome::Unsupported => {
                rounds.push(reject(
                    round,
                    &candidate,
                    "verification UNSUPPORTED",
                    Some(Outcome::Unsupported),
                    Some(diff),
                ));
            }
        }
    }

    RepairReport {
        outcome: if saw_unknown {
            RepairOutcome::AnalysisUnknown
        } else {
            RepairOutcome::BudgetExhausted
        },
        candidates_tried,
        rounds,
        accepted: None,
        accepted_program: None,
        accepted_report: None,
    }
}

fn reject(
    round: usize,
    candidate: &CirPatch,
    reason: &str,
    patched_outcome: Option<Outcome>,
    diff: Option<String>,
) -> RoundRecord {
    RoundRecord {
        round,
        candidate: candidate.id.clone(),
        accepted: false,
        reason: reason.into(),
        patched_outcome,
        diff,
    }
}
