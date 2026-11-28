use std::process;

use concir_repair::benchmark::{check_benchmark, run_benchmark};
use concir_repair::candidates::FileCandidateProvider;
use concir_repair::external::{
    build_context, evaluate_patch, replay_external_patch, ARTIFACT_SCHEMA as EXTERNAL_ARTIFACT_SCHEMA,
};
use concir_repair::search::{replay_artifact, run_search, RepairStrategy, SearchConfig};
use concir_repair::{run_repair, RepairOutcome};

use super::{parse_contract, parse_program, parse_usize_arg, read, write_if_requested, EXIT_FAIL, EXIT_INVALID, EXIT_UNKNOWN, EXIT_UNSUPPORTED, EXIT_USAGE};

fn repair_exit(outcome: RepairOutcome) -> i32 {
    match outcome {
        RepairOutcome::Repaired | RepairOutcome::AlreadySatisfied => 0,
        RepairOutcome::NoAcceptableCandidate | RepairOutcome::BudgetExhausted => EXIT_FAIL,
        RepairOutcome::AnalysisUnknown => EXIT_UNKNOWN,
        RepairOutcome::Invalid | RepairOutcome::InvalidConfig => EXIT_INVALID,
        RepairOutcome::Unsupported => EXIT_UNSUPPORTED,
    }
}

fn strategy_of(s: &str) -> RepairStrategy {
    match s {
        "a" => RepairStrategy::Single,
        "b" => RepairStrategy::Composite,
        "c" => RepairStrategy::Diagnostic,
        _ => super::usage(),
    }
}

fn eval_exit(reason: &Option<concir_repair::external::RejectReason>) -> i32 {
    match reason.as_ref().map(|r| r.code.as_str()) {
        None => 0,
        Some("verification_unknown") => EXIT_UNKNOWN,
        Some("unsupported") | Some("verification_unsupported") => EXIT_UNSUPPORTED,
        Some("verification_fail") => EXIT_FAIL,
        _ => EXIT_INVALID,
    }
}

pub fn run_repair_cmd(args: &[String]) {
    let path = args.get(2).unwrap_or_else(|| super::usage());
    let contract_path = args.get(3).unwrap_or_else(|| super::usage());
    let program = parse_program(path);
    let spec = parse_contract(Some(contract_path));

    if let Some(a4) = args.get(4) {
        if !a4.starts_with('-') {
            let mut provider = match FileCandidateProvider::from_file(a4) {
                Ok(fp) => fp,
                Err(e) => {
                    eprintln!("{e}");
                    process::exit(EXIT_USAGE);
                }
            };
            if args.len() > 6 {
                super::usage();
            }
            let budget: usize = match args.get(5) {
                Some(s) => s.parse().unwrap_or_else(|_| {
                    eprintln!("budget must be a non-negative integer");
                    process::exit(EXIT_USAGE);
                }),
                None => 16,
            };
            let report = run_repair(&program, &spec, &mut provider, budget);
            let json = serde_json::json!({
                "outcome": report.outcome,
                "candidates_tried": report.candidates_tried,
                "rounds": report.rounds,
                "accepted_patch": report.accepted,
            });
            println!("{}", serde_json::to_string_pretty(&json).expect("serialize"));
            process::exit(repair_exit(report.outcome));
        }
    }

    let mut strategy = RepairStrategy::Diagnostic;
    let mut config = SearchConfig::default();
    let mut artifact_path: Option<String> = None;
    let mut i = 4usize;
    while i < args.len() {
        match args[i].as_str() {
            "--strategy" => {
                i += 1;
                strategy = strategy_of(args.get(i).map(String::as_str).unwrap_or(""));
            }
            "--candidate-budget" => {
                i += 1;
                config.candidate_budget = parse_usize_arg(args.get(i), "--candidate-budget");
            }
            "--verification-budget" => {
                i += 1;
                config.verification_budget = parse_usize_arg(args.get(i), "--verification-budget");
            }
            "--max-depth" => {
                i += 1;
                config.max_depth = parse_usize_arg(args.get(i), "--max-depth");
            }
            "--max-total-edits" => {
                i += 1;
                config.max_total_edits = parse_usize_arg(args.get(i), "--max-total-edits");
            }
            "--artifact" => {
                i += 1;
                artifact_path = Some(args.get(i).cloned().unwrap_or_else(|| super::usage()));
            }
            _ => super::usage(),
        }
        i += 1;
    }
    config.strategy = strategy;

    let report = run_search(&program, &spec, &config);
    let artifact = report.artifact_with_config(&program, &spec, &config);
    let json = serde_json::to_string_pretty(&artifact).expect("serialize");
    if let Some(path) = &artifact_path {
        if let Err(e) = std::fs::write(path, &json) {
            eprintln!("error writing artifact '{path}': {e}");
            process::exit(EXIT_USAGE);
        }
    }
    println!("{json}");
    process::exit(repair_exit(report.outcome));
}

pub fn run_replay(args: &[String]) {
    let path = args.get(2).unwrap_or_else(|| super::usage());
    let text = read(path);
    let schema = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("schema_version").and_then(|s| s.as_str()).map(String::from));
    let replayed = if schema.as_deref() == Some(EXTERNAL_ARTIFACT_SCHEMA) {
        replay_external_patch(&text)
    } else {
        replay_artifact(&text)
    };
    match replayed {
        Ok(result) => println!("{}", serde_json::to_string_pretty(&result).expect("serialize")),
        Err(e) => {
            eprintln!("artifact replay failed: {e}");
            process::exit(EXIT_INVALID);
        }
    }
}

pub fn run_repair_context(args: &[String]) {
    let model = args.get(2).unwrap_or_else(|| super::usage());
    let contract = args.get(3).unwrap_or_else(|| super::usage());
    let program = parse_program(model);
    let spec = parse_contract(Some(contract));
    let ctx = build_context(&program, &spec);
    let json = serde_json::to_string_pretty(&ctx).expect("serialize");
    write_if_requested(args, &json);
    println!("{json}");
}

pub fn run_evaluate_patch(args: &[String]) {
    let context_path = args.get(2).unwrap_or_else(|| super::usage());
    let candidate_path = args.get(3).unwrap_or_else(|| super::usage());
    let context_text = read(context_path);
    let candidate_text = read(candidate_path);
    match evaluate_patch(&context_text, &candidate_text) {
        Ok(artifact) => {
            let json = serde_json::to_string_pretty(&artifact).expect("serialize");
            write_if_requested(args, &json);
            println!("{json}");
            let code = eval_exit(&artifact.reject_reason);
            if code != 0 {
                process::exit(code);
            }
        }
        Err(e) => {
            eprintln!("evaluate-patch failed: {e}");
            process::exit(EXIT_USAGE);
        }
    }
}

pub fn run_bench(args: &[String]) {
    let mut artifact_path: Option<String> = None;
    let mut i = 2usize;
    while i < args.len() {
        match args[i].as_str() {
            "--artifact" => {
                i += 1;
                artifact_path = Some(args.get(i).cloned().unwrap_or_else(|| super::usage()));
            }
            _ => super::usage(),
        }
        i += 1;
    }
    let records = run_benchmark(&[
        RepairStrategy::Single,
        RepairStrategy::Composite,
        RepairStrategy::Diagnostic,
    ]);
    let json = serde_json::to_string_pretty(&records).expect("serialize");
    if let Some(path) = &artifact_path {
        if let Err(e) = std::fs::write(path, &json) {
            eprintln!("error writing benchmark records '{path}': {e}");
            process::exit(EXIT_USAGE);
        }
    }
    println!("{json}");
    if let Err(e) = check_benchmark() {
        eprintln!("benchmark expectation failed: {e}");
        process::exit(EXIT_FAIL);
    }
}
