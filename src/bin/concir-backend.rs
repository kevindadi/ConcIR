//! `concir-backend`: run the non-LLM backend.
//!
//! Subcommands:
//!   check   <program.json>                         static validation (same as `cir`)
//!   explore <program.json> [contract.json] [interp|petri]   checked verification
//!   run     <program.json>                         list enabled steps from the initial state
//!   repair  <program.json> <contract.json> [patches.json] [budget]
//!   repair  <program.json> <contract.json> --strategy a|b|c [--candidate-budget N]
//!                                                   [--verification-budget N]
//!                                                   [--max-depth N] [--max-total-edits N]
//!                                                   [--artifact out.json]
//!   replay  <artifact.json>                        rebuild nodes and re-verify the export
//!   bench   [--artifact out.json]                  development benchmark records
//!   support <program.json>                         print the supportability report
//!
//! Exit codes (documented, stable):
//!   0 PASS / repaired / already satisfied
//!   1 FAIL / no acceptable candidate / budget exhausted
//!   2 usage / input error
//!   3 UNKNOWN
//!   4 INVALID (static, semantic, or configuration)
//!   5 UNSUPPORTED

use std::env;
use std::fs;
use std::process;

use concir::ast::Program;
use concir::explore::contract::ContractSpec;
use concir::explore::{verify_program, EngineKind};
use concir::interp::Interpreter;
use concir::repair::benchmark::{check_benchmark, run_benchmark};
use concir::repair::candidates::FileCandidateProvider;
use concir::repair::external::{build_context, evaluate_patch, replay_external_patch, ARTIFACT_SCHEMA as EXTERNAL_ARTIFACT_SCHEMA};
use concir::repair::search::{replay_artifact, run_search, RepairStrategy, SearchConfig};
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
         concir-backend export-cvn <program.json> [--output <file>]\n  \
         concir-backend codegen <program.json> --out <dir>\n  \
         concir-backend conform <program.json> <trace.jsonl> [--lenient-unlock] [--attempt-events] [--op-resource]\n  \
         concir-backend monitor --contract <contract.json> [--resources <resources.json>] --traces <dir> [--mapping <mapping.json>]\n  \
         concir-backend repair-context <program.json> <contract.json> [--artifact out.json]\n  \
         concir-backend evaluate-patch <context.json> <candidate.json> [--artifact out.json]\n\n\
         flags for --strategy: --candidate-budget N --verification-budget N\n  \
         --max-depth N --max-total-edits N --artifact out.json\n\n\
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

/// Add `binary_sha256` and `git_rev` to a JSON object output so experiment
/// records are bound to the producing binary.
fn versioned(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "binary_sha256".into(),
            serde_json::Value::String(concir::hash::binary_sha256()),
        );
        obj.insert(
            "git_rev".into(),
            serde_json::Value::String(concir::hash::git_rev().to_string()),
        );
    }
    value
}

fn parse_program(path: &str) -> Program {
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

fn parse_program_raw(path: &str, parse_err_exit: i32) -> Program {
    match serde_json::from_str(&read(path)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("JSON parse error in '{path}': {e}");
            process::exit(parse_err_exit);
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

fn repair_exit(outcome: RepairOutcome) -> i32 {
    match outcome {
        RepairOutcome::Repaired | RepairOutcome::AlreadySatisfied => 0,
        RepairOutcome::NoAcceptableCandidate | RepairOutcome::BudgetExhausted => EXIT_FAIL,
        RepairOutcome::AnalysisUnknown => EXIT_UNKNOWN,
        RepairOutcome::Invalid | RepairOutcome::InvalidConfig => EXIT_INVALID,
        RepairOutcome::Unsupported => EXIT_UNSUPPORTED,
    }
}

fn engine_kind(s: Option<&String>) -> EngineKind {
    match s.map(String::as_str) {
        Some("interp") => EngineKind::Interpreter,
        _ => EngineKind::Petri,
    }
}

fn strategy_of(s: &str) -> RepairStrategy {
    match s {
        "a" => RepairStrategy::Single,
        "b" => RepairStrategy::Composite,
        "c" => RepairStrategy::Diagnostic,
        _ => usage(),
    }
}

fn parse_usize_arg(s: Option<&String>, flag: &str) -> usize {
    match s.and_then(|v| v.parse::<usize>().ok()) {
        Some(n) => n,
        None => {
            eprintln!("{flag} requires a non-negative integer");
            process::exit(EXIT_USAGE);
        }
    }
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn write_if_requested(args: &[String], json: &str) {
    if let Some(path) = flag_value(args, "--artifact") {
        if let Err(e) = fs::write(&path, json) {
            eprintln!("error writing artifact '{path}': {e}");
            process::exit(EXIT_USAGE);
        }
    }
}

fn eval_exit(reason: &Option<concir::repair::external::RejectReason>) -> i32 {
    match reason.as_ref().map(|r| r.code.as_str()) {
        None => 0,
        Some("verification_unknown") => EXIT_UNKNOWN,
        Some("unsupported") | Some("verification_unsupported") => EXIT_UNSUPPORTED,
        Some("verification_fail") => EXIT_FAIL,
        _ => EXIT_INVALID,
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        usage();
    }
    match args[1].as_str() {
        "schema" => {
            let value = concir::schema::schema();
            println!("{}", serde_json::to_string_pretty(&value).expect("serialize"));
        }
        "lower" => {
            let path = args.get(2).unwrap_or_else(|| usage());
            let out_path = flag_value(&args, "--out");
            let trace_map_path = flag_value(&args, "--trace-map");
            let mut program = parse_program_raw(path, EXIT_USAGE);
            let trace_map = match program.ensure_lowered() {
                Ok(tm) => tm,
                Err(e) => {
                    eprintln!("SCIR lowering error: {e}");
                    process::exit(EXIT_INVALID);
                }
            };
            let lowered_json = serde_json::to_string_pretty(&program).expect("serialize");
            if let Some(p) = &out_path {
                if let Err(e) = fs::write(p, format!("{lowered_json}\n")) {
                    eprintln!("error writing lowered program '{p}': {e}");
                    process::exit(EXIT_USAGE);
                }
            }
            if let Some(p) = &trace_map_path {
                let tm_json = serde_json::to_string_pretty(&trace_map).expect("serialize");
                if let Err(e) = fs::write(p, format!("{tm_json}\n")) {
                    eprintln!("error writing trace map '{p}': {e}");
                    process::exit(EXIT_USAGE);
                }
            }
            println!("{lowered_json}");
        }
        "summarize" => {
            let path = args.get(2).unwrap_or_else(|| usage());
            let module_filter = flag_value(&args, "--module");
            let out_path = flag_value(&args, "--out");
            let mut program = parse_program_raw(path, EXIT_INVALID);
            if let Err(e) = program.ensure_lowered() {
                eprintln!("SCIR lowering error: {e}");
                process::exit(EXIT_INVALID);
            }
            let static_report = validate::validate(&program);
            if !static_report.valid {
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&static_report).expect("serialize")
                );
                process::exit(EXIT_INVALID);
            }
            let summary =
                match concir::summarize::summarize_program(&program, module_filter.as_deref()) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("summarize error: {e}");
                        process::exit(EXIT_INVALID);
                    }
                };
            let json = serde_json::to_string_pretty(&summary).expect("serialize");
            if let Some(p) = &out_path {
                if let Err(e) = fs::write(p, format!("{json}\n")) {
                    eprintln!("error writing summary '{p}': {e}");
                    process::exit(EXIT_USAGE);
                }
            }
            println!("{json}");
            if summary.cross_module_lock_cycle {
                process::exit(EXIT_FAIL);
            }
        }
        "verify-refinement" => {
            let spec_path = flag_value(&args, "--spec").unwrap_or_else(|| usage());
            let impl_net_path = flag_value(&args, "--impl-net").unwrap_or_else(|| usage());
            let contract_path = flag_value(&args, "--contract");
            let codegen_map_path = flag_value(&args, "--codegen-map");
            let module_filter = flag_value(&args, "--module");
            let out_path = flag_value(&args, "--out").or_else(|| flag_value(&args, "--output"));

            let mut program = parse_program_raw(&spec_path, EXIT_INVALID);
            if let Err(e) = program.ensure_lowered() {
                eprintln!("SCIR lowering error: {e}");
                process::exit(EXIT_INVALID);
            }
            let impl_net: concir::refinement::ImplNet =
                match serde_json::from_str(&read(&impl_net_path)) {
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("JSON parse error in '{impl_net_path}': {e}");
                        process::exit(EXIT_INVALID);
                    }
                };
            let contract = contract_path.as_ref().map(|p| {
                match serde_json::from_str::<ContractSpec>(&read(p)) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("contract parse error in '{p}': {e}");
                        process::exit(EXIT_INVALID);
                    }
                }
            });
            let codegen_map = codegen_map_path.as_ref().map(|p| {
                match serde_json::from_str::<concir::codegen::CodegenMap>(&read(p)) {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("codegen map parse error in '{p}': {e}");
                        process::exit(EXIT_INVALID);
                    }
                }
            });

            let report = match concir::refinement::verify_refinement_for_module(
                &program,
                &impl_net,
                contract.as_ref(),
                codegen_map.as_ref(),
                module_filter.as_deref(),
            ) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("refinement invalid spec: {e}");
                    process::exit(EXIT_INVALID);
                }
            };
            let json = serde_json::to_string_pretty(&report).expect("serialize");
            if let Some(p) = &out_path {
                if let Err(e) = fs::write(p, format!("{json}\n")) {
                    eprintln!("error writing refinement report '{p}': {e}");
                    process::exit(EXIT_USAGE);
                }
            }
            println!("{json}");
            match report.outcome.as_str() {
                "pass" => {}
                "unknown" => process::exit(EXIT_UNKNOWN),
                _ => process::exit(EXIT_FAIL),
            }
        }
        "check-req" => {
            let req_path = args.get(2).unwrap_or_else(|| usage());
            let contract_path = flag_value(&args, "--contract");
            let out_path = flag_value(&args, "--out").or_else(|| flag_value(&args, "--output"));
            let req: concir::req_ir::ReqProgram = match serde_json::from_str(&read(req_path)) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("JSON parse error in '{req_path}': {e}");
                    process::exit(EXIT_INVALID);
                }
            };
            let contract = contract_path.as_ref().map(|p| {
                match serde_json::from_str::<ContractSpec>(&read(p)) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("contract parse error in '{p}': {e}");
                        process::exit(EXIT_INVALID);
                    }
                }
            });
            let max_states = contract
                .as_ref()
                .map(|c| c.bounds.max_states)
                .unwrap_or_else(|| AnalysisBounds::default().max_states);
            let report = concir::req_ir::check_req_program(&req, contract.as_ref(), max_states);
            let json = serde_json::to_string_pretty(&report).expect("serialize");
            if let Some(p) = &out_path {
                if let Err(e) = fs::write(p, format!("{json}\n")) {
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
        "verify-design" => {
            let req_path = flag_value(&args, "--req").unwrap_or_else(|| usage());
            let design_path = flag_value(&args, "--design").unwrap_or_else(|| usage());
            let contract_path = flag_value(&args, "--contract");
            let out_path = flag_value(&args, "--out").or_else(|| flag_value(&args, "--output"));

            let req: concir::req_ir::ReqProgram = match serde_json::from_str(&read(&req_path)) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("JSON parse error in '{req_path}': {e}");
                    process::exit(EXIT_INVALID);
                }
            };
            let design = parse_program_raw(&design_path, EXIT_INVALID);
            let contract = contract_path.as_ref().map(|p| {
                match serde_json::from_str::<ContractSpec>(&read(p)) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("contract parse error in '{p}': {e}");
                        process::exit(EXIT_INVALID);
                    }
                }
            });
            let bounds = contract
                .as_ref()
                .map(|c| AnalysisBounds::from(&c.bounds))
                .unwrap_or_default();
            let report = concir::design_refinement::verify_design_refinement(
                &req,
                &design,
                contract.as_ref(),
                bounds,
            );
            let json = serde_json::to_string_pretty(&report).expect("serialize");
            if let Some(p) = &out_path {
                if let Err(e) = fs::write(p, format!("{json}\n")) {
                    eprintln!("error writing verify-design report '{p}': {e}");
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
        "explain-contract" => {
            let path = args.get(2).unwrap_or_else(|| usage());
            let out_path = flag_value(&args, "--output").or_else(|| flag_value(&args, "--out"));
            let spec = parse_contract(Some(path));
            let explanation = spec.explain();
            let json = serde_json::to_string_pretty(&explanation).expect("serialize");
            if let Some(p) = &out_path {
                if let Err(e) = fs::write(p, format!("{json}\n")) {
                    eprintln!("error writing contract explanation '{p}': {e}");
                    process::exit(EXIT_USAGE);
                }
            }
            println!("{json}");
        }
        "export-cvn" => {
            let path = args.get(2).unwrap_or_else(|| usage());
            let out_path = flag_value(&args, "--output").or_else(|| flag_value(&args, "--out"));
            let program = parse_program(path);
            let lowered_json = serde_json::to_string(&program).expect("serialize");
            let lowered = match unipn_transfer::cvn_from_concir_json(&lowered_json) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("export-cvn error: {e}");
                    process::exit(EXIT_UNSUPPORTED);
                }
            };
            let degraded: Vec<serde_json::Value> = lowered
                .report
                .degraded
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "scope": d.scope,
                        "sid": d.sid,
                        "role": d.role,
                        "source": d.source,
                        "message": d.to_string(),
                    })
                })
                .collect();
            let out = serde_json::json!({
                "schema": "concir-cvn-export/1",
                "program": program.program,
                "net": lowered.net,
                "initial": lowered.initial,
                "report": {
                    "degraded": degraded,
                    "defaulted_rwlock_readers": lowered.report.defaulted_rwlock_readers,
                    "unmodeled_slots": lowered.report.unmodeled_slots,
                }
            });
            let json = serde_json::to_string_pretty(&out).expect("serialize");
            if let Some(p) = &out_path {
                if let Err(e) = fs::write(p, format!("{json}\n")) {
                    eprintln!("error writing CVN export '{p}': {e}");
                    process::exit(EXIT_USAGE);
                }
            }
            println!("{json}");
        }
        "codegen" => {
            let path = args.get(2).unwrap_or_else(|| usage());
            let out = flag_value(&args, "--out")
                .or_else(|| args.get(3).cloned())
                .unwrap_or_else(|| usage());
            let program = parse_program(path);
            let sem = match program::lower(&program) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("cannot lower program: {e}");
                    process::exit(EXIT_INVALID);
                }
            };
            match concir::codegen::generate(&sem) {
                Ok(generated) => {
                    if let Err(e) =
                        concir::codegen::write_project(std::path::Path::new(&out), &generated)
                    {
                        eprintln!("cannot write codegen output: {e}");
                        process::exit(EXIT_USAGE);
                    }
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&versioned(
                            serde_json::to_value(&generated.map).expect("serialize")
                        ))
                        .expect("serialize")
                    );
                }
                Err(e) => {
                    eprintln!("codegen unsupported: {e}");
                    process::exit(EXIT_UNSUPPORTED);
                }
            }
        }
        "conform" => {
            let path = args.get(2).unwrap_or_else(|| usage());
            let trace_path = args.get(3).unwrap_or_else(|| usage());
            let lenient_unlock = args.iter().any(|a| a == "--lenient-unlock");
            let attempt_events = args.iter().any(|a| a == "--attempt-events");
            let op_resource = args.iter().any(|a| a == "--op-resource");
            let program = parse_program(path);
            let sem = match program::lower(&program) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("cannot lower program: {e}");
                    process::exit(EXIT_INVALID);
                }
            };
            let mut events: Vec<(String, String, String, String)> = Vec::new();
            for line in read(trace_path).lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let v: serde_json::Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("bad trace line: {e}");
                        process::exit(EXIT_USAGE);
                    }
                };
                let t = v.get("t").and_then(|x| x.as_str()).unwrap_or("");
                let sid = v.get("sid").and_then(|x| x.as_str()).unwrap_or("");
                let op = v.get("op").and_then(|x| x.as_str()).unwrap_or("");
                let res = v.get("r").and_then(|x| x.as_str()).unwrap_or("");
                events.push((t.to_string(), sid.to_string(), op.to_string(), res.to_string()));
            }
            let result = concir::conform::conform_events(&sem, &events, lenient_unlock,
                                                         attempt_events, op_resource);
            println!(
                "{}",
                serde_json::to_string_pretty(&versioned(
                    serde_json::to_value(&result).expect("serialize")
                ))
                .expect("serialize")
            );
            if result.status != "conformant" {
                process::exit(EXIT_FAIL);
            }
        }
        "monitor" => {
            let contract_path = flag_value(&args, "--contract").unwrap_or_else(|| usage());
            let traces_path = flag_value(&args, "--traces").unwrap_or_else(|| usage());
            let contract: serde_json::Value = match serde_json::from_str(&read(&contract_path)) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("contract parse error: {e}");
                    process::exit(EXIT_USAGE);
                }
            };
            let parse_value = |path: &str| -> serde_json::Value {
                match serde_json::from_str(&read(path)) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("JSON parse error in '{path}': {e}");
                        process::exit(EXIT_USAGE);
                    }
                }
            };
            let mut rust_names = match flag_value(&args, "--resources") {
                Some(p) => concir::monitor::load_resources(&parse_value(&p)),
                None => Vec::new(),
            };
            let overrides = match flag_value(&args, "--mapping") {
                Some(p) => concir::monitor::load_overrides(&parse_value(&p)),
                None => std::collections::BTreeMap::new(),
            };
            let traces = match concir::monitor::load_traces(std::path::Path::new(&traces_path)) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("cannot load traces: {e}");
                    process::exit(EXIT_USAGE);
                }
            };
            if rust_names.is_empty() {
                rust_names = concir::monitor::resources_from_traces(&traces);
            }
            let report = concir::monitor::monitor(&contract, &rust_names, &overrides, &traces);
            println!(
                "{}",
                serde_json::to_string_pretty(&versioned(
                    serde_json::to_value(&report).expect("serialize")
                ))
                .expect("serialize")
            );
            if report.status == "fail" {
                process::exit(EXIT_FAIL);
            }
        }
        "check" => {
            let path = args.get(2).unwrap_or_else(|| usage());
            let program = parse_program_raw(path, EXIT_USAGE);
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
                serde_json::to_string_pretty(&versioned(
                    serde_json::to_value(&report).expect("serialize")
                ))
                .expect("serialize")
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

            // Legacy positional form: repair model contract patches.json [budget].
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
                        usage();
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
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json).expect("serialize")
                    );
                    process::exit(repair_exit(report.outcome));
                }
            }

            // Flag form.
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
                        config.candidate_budget =
                            parse_usize_arg(args.get(i), "--candidate-budget");
                    }
                    "--verification-budget" => {
                        i += 1;
                        config.verification_budget =
                            parse_usize_arg(args.get(i), "--verification-budget");
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
                        artifact_path = Some(args.get(i).cloned().unwrap_or_else(|| usage()));
                    }
                    _ => usage(),
                }
                i += 1;
            }
            config.strategy = strategy;

            let report = run_search(&program, &spec, &config);
            let artifact = report.artifact_with_config(&program, &spec, &config);
            let json = serde_json::to_string_pretty(&artifact).expect("serialize");
            if let Some(path) = &artifact_path {
                if let Err(e) = fs::write(path, &json) {
                    eprintln!("error writing artifact '{path}': {e}");
                    process::exit(EXIT_USAGE);
                }
            }
            println!("{json}");
            process::exit(repair_exit(report.outcome));
        }
        "replay" => {
            let path = args.get(2).unwrap_or_else(|| usage());
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
                Ok(result) => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&result).expect("serialize")
                    );
                }
                Err(e) => {
                    eprintln!("artifact replay failed: {e}");
                    process::exit(EXIT_INVALID);
                }
            }
        }
        "repair-context" => {
            let model = args.get(2).unwrap_or_else(|| usage());
            let contract = args.get(3).unwrap_or_else(|| usage());
            let program = parse_program(model);
            let spec = parse_contract(Some(contract));
            let ctx = build_context(&program, &spec);
            let json = serde_json::to_string_pretty(&ctx).expect("serialize");
            write_if_requested(&args, &json);
            println!("{json}");
        }
        "evaluate-patch" => {
            let context_path = args.get(2).unwrap_or_else(|| usage());
            let candidate_path = args.get(3).unwrap_or_else(|| usage());
            let context_text = read(context_path);
            let candidate_text = read(candidate_path);
            match evaluate_patch(&context_text, &candidate_text) {
                Ok(artifact) => {
                    let json = serde_json::to_string_pretty(&artifact).expect("serialize");
                    write_if_requested(&args, &json);
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
        "bench" => {
            let mut artifact_path: Option<String> = None;
            let mut i = 2usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--artifact" => {
                        i += 1;
                        artifact_path = Some(args.get(i).cloned().unwrap_or_else(|| usage()));
                    }
                    _ => usage(),
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
                if let Err(e) = fs::write(path, &json) {
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
        _ => usage(),
    }
}
