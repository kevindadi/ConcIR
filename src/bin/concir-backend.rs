//! `concir-backend`: run the non-LLM backend.
//!
//! Subcommands:
//!   check   <program.json>                         static validation (same as `cir`)
//!   explore <program.json> [contract.json] [interp|petri]   checked verification
//!   run     <program.json>                         list enabled steps from the initial state
//!   repair  <program.json> <contract.json> [patches.json] [budget]
//!   support <program.json>                         print the supportability report
//!
//! Exit codes (documented, stable):
//!   0 PASS / repaired
//!   1 FAIL
//!   2 usage / input error
//!   3 UNKNOWN
//!   4 INVALID (static or semantic)
//!   5 UNSUPPORTED

use std::env;
use std::fs;
use std::process;

use concir::ast::Program;
use concir::explore::contract::ContractSpec;
use concir::explore::{verify_program, EngineKind};
use concir::interp::Interpreter;
use concir::repair::candidates::{CandidateProvider, FileCandidateProvider, LockOrderEnumerator};
use concir::repair::{run_repair, RepairOutcome};
use concir::sem::outcome::{AnalysisBounds, Outcome};
use concir::sem::program;
use concir::sem::system::TransitionSystem;
use concir::validate;

const EXIT_FAIL: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_UNKNOWN: i32 = 3;
const EXIT_INVALID: i32 = 4;
const EXIT_UNSUPPORTED: i32 = 5;

fn usage() -> ! {
    eprintln!(
        "usage:\n  \
         concir-backend check   <program.json>\n  \
         concir-backend explore <program.json> [contract.json] [interp|petri]\n  \
         concir-backend run     <program.json>\n  \
         concir-backend repair  <program.json> <contract.json> [patches.json] [budget]\n  \
         concir-backend support <program.json>\n\n\
         exit codes: 0 pass/repaired, 1 fail, 2 usage, 3 unknown, 4 invalid, 5 unsupported"
    );
    process::exit(EXIT_USAGE);
}

fn read(path: &str) -> String {
    match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading '{path}': {e}");
            process::exit(EXIT_USAGE);
        }
    }
}

fn parse_program(path: &str) -> Program {
    match serde_json::from_str(&read(path)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("JSON parse error in '{path}': {e}");
            process::exit(EXIT_USAGE);
        }
    }
}

fn parse_contract(path: Option<&String>) -> ContractSpec {
    let text = match path {
        Some(p) => read(p),
        None => {
            r#"{ "name": "default", "properties": [ { "kind": "deadlock_free", "id": "no-deadlock" } ] }"#
                .to_string()
        }
    };
    match serde_json::from_str(&text) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("contract parse error: {e}");
            process::exit(EXIT_USAGE);
        }
    }
}

fn outcome_exit(outcome: Outcome) -> i32 {
    match outcome {
        Outcome::Pass => 0,
        Outcome::Fail => EXIT_FAIL,
        Outcome::Unknown => EXIT_UNKNOWN,
        Outcome::Invalid => EXIT_INVALID,
        Outcome::Unsupported => EXIT_UNSUPPORTED,
    }
}

fn engine_kind(s: Option<&String>) -> EngineKind {
    match s.map(String::as_str) {
        Some("interp") => EngineKind::Interpreter,
        _ => EngineKind::Petri,
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        usage();
    }
    match args[1].as_str() {
        "check" => {
            let path = args.get(2).unwrap_or_else(|| usage());
            let program = parse_program(path);
            let report = validate::validate(&program);
            println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("serialize")
            );
            if !report.valid {
                process::exit(EXIT_INVALID);
            }
        }
        "support" => {
            let path = args.get(2).unwrap_or_else(|| usage());
            let program = parse_program(path);
            let sem = match program::lower(&program) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("cannot lower program: {e}");
                    process::exit(EXIT_INVALID);
                }
            };
            let unsupported = sem.unsupported();
            let out = serde_json::json!({
                "supported": unsupported.is_empty(),
                "unsupported": unsupported,
            });
            println!("{}", serde_json::to_string_pretty(&out).expect("serialize"));
            if !unsupported.is_empty() {
                process::exit(EXIT_UNSUPPORTED);
            }
        }
        "run" => {
            let path = args.get(2).unwrap_or_else(|| usage());
            let program = parse_program(path);
            let sem = match program::lower(&program) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("cannot lower program: {e}");
                    process::exit(EXIT_INVALID);
                }
            };
            let it = Interpreter::new(&sem, AnalysisBounds::default());
            let init = match it.initial() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("initial state error: {e}");
                    process::exit(EXIT_INVALID);
                }
            };
            match it.successors(&init) {
                Ok(en) => {
                    let labels: Vec<String> =
                        en.steps.iter().map(|s| s.label.canonical()).collect();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&labels).expect("serialize")
                    );
                }
                Err(e) => {
                    eprintln!("step error: {e}");
                    process::exit(EXIT_INVALID);
                }
            }
        }
        "explore" => {
            let path = args.get(2).unwrap_or_else(|| usage());
            let spec = parse_contract(args.get(3));
            let engine = engine_kind(args.get(4));
            let program = parse_program(path);
            let report = verify_program(&program, &spec, engine);
            println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("serialize")
            );
            let code = outcome_exit(report.outcome);
            if code != 0 {
                process::exit(code);
            }
        }
        "repair" => {
            let path = args.get(2).unwrap_or_else(|| usage());
            let contract_path = args.get(3).unwrap_or_else(|| usage());
            let program = parse_program(path);
            let spec = parse_contract(Some(contract_path));
            let provider: Box<dyn CandidateProvider> = if let Some(p) = args.get(4) {
                match FileCandidateProvider::from_file(p) {
                    Ok(fp) => Box::new(fp),
                    Err(e) => {
                        eprintln!("{e}");
                        process::exit(EXIT_USAGE);
                    }
                }
            } else {
                Box::new(LockOrderEnumerator::new(&program, &spec.allowed_scope))
            };
            let budget: usize = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(16);
            let mut provider = provider;
            let report = run_repair(&program, &spec, provider.as_mut(), budget);
            let json = serde_json::json!({
                "outcome": report.outcome,
                "candidates_tried": report.candidates_tried,
                "rounds": report.rounds,
                "accepted_patch": report.accepted,
            });
            println!("{}", serde_json::to_string_pretty(&json).expect("serialize"));
            match report.outcome {
                RepairOutcome::Repaired => {}
                RepairOutcome::AnalysisUnknown => process::exit(EXIT_UNKNOWN),
                _ => process::exit(EXIT_FAIL),
            }
        }
        _ => usage(),
    }
}
