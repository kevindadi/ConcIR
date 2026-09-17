//! Small, fixed development benchmark for the repair search.
//!
//! This is a development regression set, **not** an independent evaluation
//! corpus. Each case carries its own model, frozen contract, an independent
//! expected outcome, the allowed edit scope, and an explanation. It exists to
//! separate "composite capability" from "diagnostic guidance" between the three
//! strategies; it must not be used to compute generalisation.

use std::time::Instant;

use serde::Serialize;

use crate::ast::Program;
use crate::explore::contract::ContractSpec;
use crate::explore::{verify_program, EngineKind};
use crate::sem::outcome::Outcome;

use super::search::{program_fingerprint, run_search, RepairStrategy, SearchArtifact, SearchConfig};
use super::RepairOutcome;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Expected {
    Repaired { chain: usize },
    AlreadySatisfied,
    NoAcceptableCandidate,
    BudgetExhausted,
}

#[derive(Debug, Clone, Copy)]
pub struct BenchCase {
    pub name: &'static str,
    pub model: &'static str,
    pub contract: &'static str,
    pub expected: Expected,
    /// Whether the single-edit baseline (A) is expected to reach `expected`.
    pub single_can_reach: bool,
    pub allowed_scope: &'static str,
    pub explanation: &'static str,
    pub candidate_budget: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchRecord {
    pub case: String,
    pub strategy: RepairStrategy,
    pub code_version: String,
    pub model_fingerprint: String,
    pub contract_fingerprint: String,
    pub candidate_budget: usize,
    pub verification_budget: usize,
    pub max_depth: usize,
    pub max_total_edits: usize,
    pub root_outcome: Option<Outcome>,
    pub outcome: RepairOutcome,
    pub stop_reason: String,
    pub candidates_tried: usize,
    pub unique_programs: usize,
    pub verifications: usize,
    pub cache_hits: usize,
    pub states_explored: usize,
    pub elapsed_ms: u64,
    pub patch_size: usize,
    pub accepted_chain_len: usize,
    pub expected: Expected,
    pub reached_expected: bool,
    /// The complete, replayable artifact (input, frozen contract, config,
    /// nodes/attempts, patch chain, accepted program, full reports).
    pub artifact: SearchArtifact,
}

const TWO_CYCLES: &str = include_str!("../../tests/repro_bench/two_cycles.json");
const TWO_CYCLES_CONTRACT: &str = include_str!("../../tests/repro_bench/two_cycles_contract.json");

pub const CASES: &[BenchCase] = &[
    BenchCase {
        name: "already_correct",
        model: include_str!("../../tests/repro_bench/already_correct.json"),
        contract: include_str!("../../tests/repro_bench/already_correct_contract.json"),
        expected: Expected::AlreadySatisfied,
        single_can_reach: true,
        allowed_scope: "all functions, lock reorder allowed",
        explanation: "the root already satisfies the contract; no patch may be produced",
        candidate_budget: None,
    },
    BenchCase {
        name: "single_cycle",
        model: include_str!("../../tests/repro_bench/single_cycle.json"),
        contract: include_str!("../../tests/repro_bench/single_cycle_contract.json"),
        expected: Expected::Repaired { chain: 1 },
        single_can_reach: true,
        allowed_scope: "all functions, lock reorder allowed",
        explanation: "one ABBA cycle; a single adjacent swap fixes it",
        candidate_budget: None,
    },
    BenchCase {
        name: "two_cycles",
        model: TWO_CYCLES,
        contract: TWO_CYCLES_CONTRACT,
        expected: Expected::Repaired { chain: 2 },
        single_can_reach: false,
        allowed_scope: "all functions, lock reorder allowed",
        explanation: "two independent ABBA cycles; one swap leaves the other deadlock",
        candidate_budget: None,
    },
    BenchCase {
        name: "cross_module_two_cycles",
        model: include_str!("../../tests/repro_bench/cross_module_two_cycles.json"),
        contract: include_str!("../../tests/repro_bench/cross_module_two_cycles_contract.json"),
        expected: Expected::Repaired { chain: 2 },
        single_can_reach: false,
        allowed_scope: "all functions in all modules; lock reorder allowed",
        explanation: "two ABBA cycles split over two modules; the repair edits both",
        candidate_budget: None,
    },
    BenchCase {
        name: "forbidden_scope",
        model: include_str!("../../tests/repro_bench/single_cycle.json"),
        contract: include_str!("../../tests/repro_bench/forbidden_scope_contract.json"),
        expected: Expected::NoAcceptableCandidate,
        single_can_reach: true,
        allowed_scope: "only main::unrelated; lock reorder allowed",
        explanation: "the only fixable function is outside the allowed patch scope",
        candidate_budget: None,
    },
    BenchCase {
        name: "preserved_unfixable",
        model: include_str!("../../tests/repro_bench/preserved_unfixable.json"),
        contract: include_str!("../../tests/repro_bench/preserved_unfixable_contract.json"),
        expected: Expected::NoAcceptableCandidate,
        single_can_reach: true,
        allowed_scope: "all functions, lock reorder allowed",
        explanation: "a preserved behaviour is unreachable and no lock edit can restore it",
        candidate_budget: None,
    },
    BenchCase {
        name: "no_lock_candidate",
        model: include_str!("../../tests/repro_bench/channel_deadlock.json"),
        contract: include_str!("../../tests/repro_bench/channel_deadlock_contract.json"),
        expected: Expected::NoAcceptableCandidate,
        single_can_reach: true,
        allowed_scope: "all functions, lock reorder allowed",
        explanation: "a channel rendezvous deadlock is not a lock-order defect",
        candidate_budget: None,
    },
    BenchCase {
        name: "budget_truncated",
        model: TWO_CYCLES,
        contract: TWO_CYCLES_CONTRACT,
        expected: Expected::BudgetExhausted,
        single_can_reach: true,
        allowed_scope: "all functions, lock reorder allowed",
        explanation: "the two-edit fix does not fit a one-candidate budget",
        candidate_budget: Some(1),
    },
];

fn spec_of(case: &BenchCase) -> ContractSpec {
    serde_json::from_str(case.contract).unwrap()
}

fn program_of(case: &BenchCase) -> Program {
    serde_json::from_str(case.model).unwrap()
}

fn matches_expected(actual: RepairOutcome, chain: usize, expected: Expected) -> bool {
    match expected {
        Expected::Repaired { chain: c } => actual == RepairOutcome::Repaired && chain == c,
        Expected::AlreadySatisfied => actual == RepairOutcome::AlreadySatisfied,
        Expected::NoAcceptableCandidate => actual == RepairOutcome::NoAcceptableCandidate,
        Expected::BudgetExhausted => actual == RepairOutcome::BudgetExhausted,
    }
}

fn fnv(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// Run every case under every strategy and return machine-readable records.
pub fn run_benchmark(strategies: &[RepairStrategy]) -> Vec<BenchRecord> {
    let mut records = Vec::new();
    for case in CASES {
        let program = program_of(case);
        let spec = spec_of(case);
        let model_fp = program_fingerprint(&program);
        let contract_fp = fnv(case.contract);
        for &strategy in strategies {
            let mut cfg = SearchConfig {
                strategy,
                ..SearchConfig::default()
            };
            if let Some(b) = case.candidate_budget {
                cfg.candidate_budget = b;
            }
            let t0 = Instant::now();
            let report = run_search(&program, &spec, &cfg);
            let elapsed_ms = t0.elapsed().as_millis() as u64;
            let patch_size: usize = report.patch_chain.iter().map(|e| e.changes.len()).sum();
            let artifact = report.artifact_with_config(&program, &spec, &cfg);
            records.push(BenchRecord {
                case: case.name.to_string(),
                strategy,
                code_version: env!("CARGO_PKG_VERSION").to_string(),
                model_fingerprint: model_fp.clone(),
                contract_fingerprint: contract_fp.clone(),
                candidate_budget: cfg.candidate_budget,
                verification_budget: cfg.verification_budget,
                max_depth: cfg.max_depth,
                max_total_edits: cfg.max_total_edits,
                root_outcome: report.nodes.first().map(|n| n.report.outcome),
                outcome: report.outcome,
                stop_reason: report.stop_reason.clone(),
                candidates_tried: report.proposals,
                unique_programs: report.unique_programs,
                verifications: report.verifications,
                cache_hits: report.cache_hits,
                states_explored: report.states_explored,
                elapsed_ms,
                patch_size,
                accepted_chain_len: report.patch_chain.len(),
                expected: case.expected,
                reached_expected: matches_expected(
                    report.outcome,
                    report.patch_chain.len(),
                    case.expected,
                ),
                artifact,
            });
        }
    }
    records
}

/// Check that the benchmark's independent expectations hold for the strategies
/// that are supposed to reach them, and return a structured summary.
pub fn check_benchmark() -> Result<(), String> {
    let records = run_benchmark(&[
        RepairStrategy::Single,
        RepairStrategy::Composite,
        RepairStrategy::Diagnostic,
    ]);
    for case in CASES {
        for &strategy in &[
            RepairStrategy::Single,
            RepairStrategy::Composite,
            RepairStrategy::Diagnostic,
        ] {
            let rec = records
                .iter()
                .find(|r| r.case == case.name && r.strategy == strategy)
                .ok_or_else(|| format!("missing record {}/{:?}", case.name, strategy))?;
            let should_reach = match strategy {
                RepairStrategy::Single => case.single_can_reach,
                RepairStrategy::Composite | RepairStrategy::Diagnostic => true,
            };
            if should_reach && !rec.reached_expected {
                return Err(format!(
                    "case '{}' strategy {:?}: expected {:?}, got {:?} ({})",
                    case.name, strategy, case.expected, rec.outcome, rec.stop_reason
                ));
            }
            if !should_reach && rec.reached_expected {
                return Err(format!(
                    "case '{}' strategy {:?} unexpectedly reached {:?}",
                    case.name, strategy, case.expected
                ));
            }
        }
    }
    Ok(())
}

/// The final accepted program of a repaired case, re-verified from JSON.
pub fn export_and_reverify(case_name: &str, strategy: RepairStrategy) -> Option<bool> {
    let case = CASES.iter().find(|c| c.name == case_name)?;
    let program = program_of(case);
    let spec = spec_of(case);
    let report = run_search(&program, &spec, &SearchConfig { strategy, ..SearchConfig::default() });
    let accepted = report.accepted_program?;
    let json = serde_json::to_string(&accepted).ok()?;
    let reparsed: Program = serde_json::from_str(&json).ok()?;
    let re = verify_program(&reparsed, &spec, EngineKind::Petri);
    Some(re.outcome == Outcome::Pass && re.complete)
}
