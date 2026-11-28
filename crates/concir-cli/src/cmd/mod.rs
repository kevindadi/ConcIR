pub mod check;
pub mod codegen;
pub mod contract;
pub mod refine;
pub mod repair;
pub mod verify;

use std::env;
use std::fs;
use std::process;

use concir_ir::ast::Program;
use concir_ir::hash;
use concir_sem::outcome::Outcome;
use concir_verify::contract::ContractSpec;

pub const EXIT_FAIL: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_UNKNOWN: i32 = 3;
pub const EXIT_INVALID: i32 = 4;
pub const EXIT_UNSUPPORTED: i32 = 5;

pub fn usage() -> ! {
    eprintln!(
        "usage:\n  \
         concir-backend check   <program.json>\n  \
         concir-backend explore <program.json> [contract.json] [interp|petri]\n  \
         concir-backend run     <program.json>\n  \
         concir-backend repair  <program.json> <contract.json> [patches.json] [budget]\n  \
         concir-backend repair  <program.json> <contract.json> --strategy a|b|c [flags]\n  \
         concir-backend replay  <artifact.json>\n  \
         concir-backend bench   [--artifact out.json]\n  \
         concir-backend support <program.json>\n  \
         concir-backend schema\n  \
         concir-backend lower   <program.json> [--out <lowered.json>] [--trace-map <map.json>]\n  \
         concir-backend summarize <program.json> [--module <name>] [--out <summary.json>]\n  \
         concir-backend check-req <req_ir.json> [--contract <contract.json>] [--out <report.json>]\n  \
         concir-backend verify-design --req <req_ir.json> --design <program.json> [--contract <contract.json>] [--out <report.json>]\n  \
         concir-backend verify-refinement --spec <program.json> --impl-net <impl_net.json> [--contract <contract.json>] [--codegen-map <codegen.json>] [--module <name>] [--out <report.json>]\n  \
         concir-backend explain-contract <contract.json> [--output <file>]\n  \
         concir-backend validate-contract <contract.json> [--requirements <req.json>] [--no-strict-entities] [--no-require-non-vacuous] [--normalize] [--output <file>]\n  \
         concir-backend export-cvn <program.json> [--output <file>]\n  \
         concir-backend codegen <program.json> --out <dir>\n  \
         concir-backend conform <program.json> <trace.jsonl> [--lenient-unlock] [--attempt-events] [--op-resource]\n  \
         concir-backend repair-context <program.json> <contract.json> [--artifact out.json]\n  \
         concir-backend evaluate-patch <context.json> <candidate.json> [--artifact out.json]\n\n  \
         flags for --strategy: --candidate-budget N --verification-budget N\n  \
         --max-depth N --max-total-edits N --artifact out.json\n\n  \
         exit codes: 0 pass/repaired, 1 fail, 2 usage, 3 unknown, 4 invalid, 5 unsupported"
    );
    process::exit(EXIT_USAGE);
}

pub fn read(path: &str) -> String {
    match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading '{path}': {e}");
            process::exit(EXIT_USAGE);
        }
    }
}

pub fn versioned(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "binary_sha256".into(),
            serde_json::Value::String(hash::binary_sha256()),
        );
        obj.insert(
            "git_rev".into(),
            serde_json::Value::String(hash::git_rev().to_string()),
        );
    }
    value
}

pub fn parse_program(path: &str) -> Program {
    let mut p: Program = match serde_json::from_str(&read(path)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("JSON parse error in '{path}': {e}");
            process::exit(EXIT_USAGE);
        }
    };
    if p.has_structured_bodies() {
        if let Err(e) = p.ensure_lowered() {
            eprintln!("SCIR lowering error in '{path}': {e}");
            process::exit(EXIT_INVALID);
        }
    }
    p
}

pub fn parse_program_raw(path: &str, parse_err_exit: i32) -> Program {
    match serde_json::from_str(&read(path)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("JSON parse error in '{path}': {e}");
            process::exit(parse_err_exit);
        }
    }
}

pub fn parse_contract(path: Option<&String>) -> ContractSpec {
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

pub fn parse_contract_raw(path: &str, parse_err_exit: i32) -> ContractSpec {
    match serde_json::from_str(&read(path)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("contract parse error in '{path}': {e}");
            process::exit(parse_err_exit);
        }
    }
}

pub fn outcome_exit(outcome: Outcome) -> i32 {
    match outcome {
        Outcome::Pass => 0,
        Outcome::Fail => EXIT_FAIL,
        Outcome::Unknown => EXIT_UNKNOWN,
        Outcome::Invalid => EXIT_INVALID,
        Outcome::Unsupported => EXIT_UNSUPPORTED,
    }
}

pub fn parse_usize_arg(s: Option<&String>, flag: &str) -> usize {
    match s.and_then(|v| v.parse::<usize>().ok()) {
        Some(n) => n,
        None => {
            eprintln!("{flag} requires a non-negative integer");
            process::exit(EXIT_USAGE);
        }
    }
}

pub fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

pub fn write_if_requested(args: &[String], json: &str) {
    if let Some(path) = flag_value(args, "--artifact") {
        if let Err(e) = fs::write(&path, json) {
            eprintln!("error writing artifact '{path}': {e}");
            process::exit(EXIT_USAGE);
        }
    }
}

pub fn dispatch() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        usage();
    }
    match args[1].as_str() {
        "schema" => check::run_schema(),
        "lower" => check::run_lower(&args),
        "summarize" => check::run_summarize(&args),
        "check" => check::run_check(&args),
        "support" => check::run_support(&args),
        "run" => verify::run_run(&args),
        "explore" => verify::run_explore(&args),
        "check-req" => verify::run_check_req(&args),
        "verify-design" => refine::run_verify_design(&args),
        "verify-refinement" => refine::run_verify_refinement(&args),
        "conform" => refine::run_conform(&args),
        "explain-contract" => contract::run_explain_contract(&args),
        "validate-contract" => contract::run_validate_contract(&args),
        "export-cvn" => codegen::run_export_cvn(&args),
        "codegen" => codegen::run_codegen(&args),
        "repair" => repair::run_repair_cmd(&args),
        "replay" => repair::run_replay(&args),
        "repair-context" => repair::run_repair_context(&args),
        "evaluate-patch" => repair::run_evaluate_patch(&args),
        "bench" => repair::run_bench(&args),
        _ => usage(),
    }
}
