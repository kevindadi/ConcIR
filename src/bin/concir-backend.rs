//! `concir-backend`: run the non-LLM backend.
//!
//! Subcommands:
//!   check   <program.json>                         static validation (same as `cir`)
//!   explore <program.json> [contract.json]         verify properties, print report
//!   run     <program.json>                         list enabled steps from the initial state
//!   repair  <program.json> <contract.json> [patches.json] [budget]
//!   support <program.json>                         print the supportability report

use std::env;
use std::fs;
use std::process;

use concir::ast::Program;
use concir::explore::contract::ContractSpec;
use concir::explore::verify;
use concir::interp::Interpreter;
use concir::petri::PetriEngine;
use concir::repair::candidates::{CandidateProvider, FileCandidateProvider, LockOrderEnumerator};
use concir::repair::run_repair;
use concir::sem::outcome::AnalysisBounds;
use concir::sem::program;
use concir::sem::system::TransitionSystem;
use concir::validate;

fn usage() -> ! {
    eprintln!(
        "usage:\n  \
         concir-backend check   <program.json>\n  \
         concir-backend explore <program.json> [contract.json] [interp|petri]\n  \
         concir-backend run     <program.json>\n  \
         concir-backend repair  <program.json> <contract.json> [patches.json] [budget]\n  \
         concir-backend support <program.json>"
    );
    process::exit(2);
}

fn read(path: &str) -> String {
    match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading '{path}': {e}");
            process::exit(2);
        }
    }
}

fn parse_program(path: &str) -> Program {
    match serde_json::from_str(&read(path)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("JSON parse error in '{path}': {e}");
            process::exit(2);
        }
    }
}

fn default_contract() -> String {
    r#"{ "name": "default", "properties": [ { "kind": "deadlock_free", "id": "no-deadlock" } ] }"#
        .to_string()
}

fn resolve_contract(program: &Program, source: Option<&str>) -> (ContractSpec, concir::explore::contract::VerificationContract) {
    let text = match source {
        Some(p) => read(p),
        None => default_contract(),
    };
    let spec: ContractSpec = match serde_json::from_str(&text) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("contract parse error: {e}");
            process::exit(2);
        }
    };
    let sem = match program::lower(program) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot lower program: {e}");
            process::exit(1);
        }
    };
    let contract = match spec.resolve(&sem) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("contract resolution error: {e}");
            process::exit(2);
        }
    };
    (spec, contract)
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
                process::exit(1);
            }
        }
        "support" => {
            let path = args.get(2).unwrap_or_else(|| usage());
            let program = parse_program(path);
            let sem = match program::lower(&program) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("cannot lower program: {e}");
                    process::exit(1);
                }
            };
            let unsupported = sem.unsupported();
            let out = serde_json::json!({
                "supported": unsupported.is_empty(),
                "unsupported": unsupported,
            });
            println!("{}", serde_json::to_string_pretty(&out).expect("serialize"));
            if !unsupported.is_empty() {
                process::exit(1);
            }
        }
        "run" => {
            let path = args.get(2).unwrap_or_else(|| usage());
            let program = parse_program(path);
            let sem = match program::lower(&program) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("cannot lower program: {e}");
                    process::exit(1);
                }
            };
            let it = Interpreter::new(&sem, AnalysisBounds::default());
            let init = match it.initial() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("initial state error: {e}");
                    process::exit(1);
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
                    process::exit(1);
                }
            }
        }
        "explore" => {
            let path = args.get(2).unwrap_or_else(|| usage());
            let contract_path = args.get(3).map(String::as_str);
            let engine = args.get(4).map(String::as_str).unwrap_or("petri");
            let program = parse_program(path);
            let (_, contract) = resolve_contract(&program, contract_path);
            let sem = program::lower(&program).unwrap();
            let report = match engine {
                "interp" => {
                    let e = Interpreter::new(&sem, contract.bounds.clone());
                    verify(&e, &contract)
                }
                _ => {
                    let e = PetriEngine::new(&sem, contract.bounds.clone());
                    verify(&e, &contract)
                }
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("serialize")
            );
            if report.outcome == concir::sem::outcome::Outcome::Fail {
                process::exit(1);
            }
        }
        "repair" => {
            let path = args.get(2).unwrap_or_else(|| usage());
            let contract_path = args.get(3);
            let program = parse_program(path);
            let (_, contract) = resolve_contract(&program, contract_path.map(String::as_str));
            let sem = program::lower(&program).unwrap();
            let provider: Box<dyn CandidateProvider> = if let Some(p) = args.get(4) {
                match FileCandidateProvider::from_file(p) {
                    Ok(fp) => Box::new(fp),
                    Err(e) => {
                        eprintln!("{e}");
                        process::exit(2);
                    }
                }
            } else {
                Box::new(LockOrderEnumerator::new(&program, &contract.allowed_scope))
            };
            let budget: usize = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(16);
            let mut provider = provider;
            let report = run_repair(&program, &contract, provider.as_mut(), budget);
            let _ = sem;
            let json = serde_json::json!({
                "outcome": report.outcome,
                "candidates_tried": report.candidates_tried,
                "rounds": report.rounds,
                "accepted_patch": report.accepted,
            });
            println!("{}", serde_json::to_string_pretty(&json).expect("serialize"));
            if report.outcome != concir::repair::RepairOutcome::Repaired {
                process::exit(1);
            }
        }
        _ => usage(),
    }
}
