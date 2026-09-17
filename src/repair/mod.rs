//! Phase 4: structured CIR patches and the LLM-free iterative repair loop.
//!
//! The [`VerificationContract`] is built from data and is never modified by a
//! patch or by the loop. Candidates are proposed by a [`CandidateProvider`],
//! applied to the parsed [`Program`], then re-validated, re-lowered,
//! re-translated, and re-verified in full.
//!
//! Old-counterexample replay is intentionally *not* an acceptance criterion:
//! every candidate goes through full verification. Replay may only be used as
//! an optional fast pre-filter.

pub mod candidates;
pub mod patch;

use std::collections::HashSet;

use serde::Serialize;

use crate::ast::Program;
use crate::explore::contract::VerificationContract;
use crate::explore::{self, VerificationReport};
use crate::petri::PetriEngine;
use crate::sem::outcome::Outcome;
use crate::validate;

use candidates::{CandidateProvider, RepairContext};
use patch::CirPatch;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairOutcome {
    Repaired,
    NoAcceptableCandidate,
    BudgetExhausted,
    AnalysisUnknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoundRecord {
    pub round: usize,
    pub candidate: String,
    pub accepted: bool,
    pub reason: String,
    pub patched_outcome: Option<Outcome>,
    /// Difference the patch would make (kept for the repair log).
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

/// Run the deterministic repair loop.
///
/// Each candidate goes through: patch legality → CIR static validation →
/// supportability → re-translation → full verification of every required
/// property (and preserved behaviour) → accept or reject.
pub fn run_repair(
    program: &Program,
    contract: &VerificationContract,
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
            contract,
            round,
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
        if !tried.insert(candidate.id.clone()) {
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

        if !contract.allowed_scope.allows_function(&candidate.function) {
            rounds.push(reject(round, &candidate, "function outside allowed patch scope", None, None));
            continue;
        }

        let (patched, diff) = match patch::apply(&current, &candidate) {
            Ok(v) => v,
            Err(e) => {
                rounds.push(reject(round, &candidate, &format!("patch rejected: {e}"), None, None));
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

        let engine = PetriEngine::new(&sem, contract.bounds.clone());
        let report = explore::verify(&engine, contract);
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
