use std::process;

use concir_engine::interp::Interpreter;
use concir_sem::outcome::AnalysisBounds;
use concir_sem::program;
use concir_sem::system::TransitionSystem;
use concir_verify::contract::ContractSpec;
use concir_verify::model_check::{verify_program, EngineKind};
use concir_verify::req_check::{check_req_program, ReqProgram};

use super::{flag_value, outcome_exit, parse_contract, parse_program, read, versioned, EXIT_FAIL, EXIT_INVALID, EXIT_UNKNOWN, EXIT_UNSUPPORTED, EXIT_USAGE};

pub fn run_run(args: &[String]) {
    let path = args.get(2).unwrap_or_else(|| super::usage());
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
            let labels: Vec<String> = en.steps.iter().map(|s| s.label.canonical()).collect();
            println!("{}", serde_json::to_string_pretty(&labels).expect("serialize"));
        }
        Err(e) => {
            eprintln!("step error: {e}");
            process::exit(EXIT_INVALID);
        }
    }
}

pub fn run_explore(args: &[String]) {
    let path = args.get(2).unwrap_or_else(|| super::usage());
    let spec = parse_contract(args.get(3));
    let engine = match args.get(4).map(String::as_str) {
        Some("interp") => EngineKind::Interpreter,
        _ => EngineKind::Petri,
    };
    let program = parse_program(path);
    let report = verify_program(&program, &spec, engine);
    println!(
        "{}",
        serde_json::to_string_pretty(&versioned(serde_json::to_value(&report).expect("serialize")))
            .expect("serialize")
    );
    let code = outcome_exit(report.outcome);
    if code != 0 {
        process::exit(code);
    }
}

pub fn run_check_req(args: &[String]) {
    let req_path = args.get(2).unwrap_or_else(|| super::usage());
    let contract_path = flag_value(args, "--contract");
    let out_path = flag_value(args, "--out").or_else(|| flag_value(args, "--output"));
    let req: ReqProgram = match serde_json::from_str(&read(req_path)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("JSON parse error in '{req_path}': {e}");
            process::exit(EXIT_INVALID);
        }
    };
    let contract = contract_path.as_ref().map(|p| {
        serde_json::from_str::<ContractSpec>(&read(p)).unwrap_or_else(|e| {
            eprintln!("contract parse error in '{p}': {e}");
            process::exit(EXIT_INVALID);
        })
    });
    let max_states = contract
        .as_ref()
        .map(|c| c.bounds.max_states)
        .unwrap_or_else(|| AnalysisBounds::default().max_states);
    let report = check_req_program(&req, contract.as_ref(), max_states);
    let json = serde_json::to_string_pretty(&report).expect("serialize");
    if let Some(p) = &out_path {
        if let Err(e) = std::fs::write(p, format!("{json}\n")) {
            eprintln!("error writing check-req report '{p}': {e}");
            process::exit(EXIT_USAGE);
        }
    }
    println!("{json}");
    match report.outcome.as_str() {
        "pass" => {}
        "unknown" => process::exit(EXIT_UNKNOWN),
        "invalid" => process::exit(EXIT_INVALID),
        "unsupported" => process::exit(EXIT_UNSUPPORTED),
        _ => process::exit(EXIT_FAIL),
    }
}
