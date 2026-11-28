use std::process;

use concir_codegen::CodegenMap;
use concir_sem::outcome::AnalysisBounds;
use concir_sem::program;
use concir_verify::conform::conform_events;
use concir_verify::contract::ContractSpec;
use concir_verify::refinement::design::verify_design_refinement;
use concir_verify::refinement::{verify_refinement_for_module, ImplNet};
use concir_verify::req_check::ReqProgram;

use super::{flag_value, parse_contract_raw, parse_program, parse_program_raw, read, versioned, EXIT_FAIL, EXIT_INVALID, EXIT_UNKNOWN, EXIT_UNSUPPORTED, EXIT_USAGE};

pub fn run_verify_design(args: &[String]) {
    let req_path = flag_value(args, "--req").unwrap_or_else(|| super::usage());
    let design_path = flag_value(args, "--design").unwrap_or_else(|| super::usage());
    let contract_path = flag_value(args, "--contract");
    let out_path = flag_value(args, "--out").or_else(|| flag_value(args, "--output"));

    let req: ReqProgram = match serde_json::from_str(&read(&req_path)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("JSON parse error in '{req_path}': {e}");
            process::exit(EXIT_INVALID);
        }
    };
    let design = parse_program_raw(&design_path, EXIT_INVALID);
    let contract = contract_path.as_ref().map(|p| parse_contract_raw(p, EXIT_INVALID));
    let bounds = contract
        .as_ref()
        .map(|c| AnalysisBounds::from(&c.bounds))
        .unwrap_or_default();
    let report = verify_design_refinement(&req, &design, contract.as_ref(), bounds);
    let json = serde_json::to_string_pretty(&report).expect("serialize");
    if let Some(p) = &out_path {
        if let Err(e) = std::fs::write(p, format!("{json}\n")) {
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

pub fn run_verify_refinement(args: &[String]) {
    let spec_path = flag_value(args, "--spec").unwrap_or_else(|| super::usage());
    let impl_path = flag_value(args, "--impl-net").unwrap_or_else(|| super::usage());
    let contract_path = flag_value(args, "--contract");
    let codegen_map_path = flag_value(args, "--codegen-map");
    let module_filter = flag_value(args, "--module");
    let out_path = flag_value(args, "--out").or_else(|| flag_value(args, "--output"));

    let spec_prog = parse_program(&spec_path);
    let impl_net: ImplNet = match serde_json::from_str(&read(&impl_path)) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("invalid impl-net JSON '{impl_path}': {e}");
            process::exit(EXIT_INVALID);
        }
    };
    let contract: Option<ContractSpec> = contract_path.as_ref().map(|p| parse_contract_raw(p, EXIT_INVALID));
    let codegen_map: Option<CodegenMap> = codegen_map_path.as_ref().map(|p| {
        serde_json::from_str(&read(p)).unwrap_or_else(|e| {
            eprintln!("codegen map parse error in '{p}': {e}");
            process::exit(EXIT_INVALID);
        })
    });

    let report = match verify_refinement_for_module(
        &spec_prog,
        &impl_net,
        contract.as_ref(),
        codegen_map.as_ref(),
        module_filter.as_deref(),
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("verify-refinement error: {e}");
            process::exit(EXIT_INVALID);
        }
    };
    let json = serde_json::to_string_pretty(&report).expect("serialize");
    if let Some(p) = &out_path {
        if let Err(e) = std::fs::write(p, format!("{json}\n")) {
            eprintln!("error writing refinement report '{p}': {e}");
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

pub fn run_conform(args: &[String]) {
    let path = args.get(2).unwrap_or_else(|| super::usage());
    let trace_path = args.get(3).unwrap_or_else(|| super::usage());
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
    let result = conform_events(&sem, &events, lenient_unlock, attempt_events, op_resource);
    println!(
        "{}",
        serde_json::to_string_pretty(&versioned(serde_json::to_value(&result).expect("serialize")))
            .expect("serialize")
    );
    if result.status != "conformant" {
        process::exit(EXIT_FAIL);
    }
}
