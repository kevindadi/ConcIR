use std::process;

use concir_verify::contract::validate_contract_value;

use super::{flag_value, parse_contract, read, EXIT_FAIL, EXIT_USAGE};

pub fn run_explain_contract(args: &[String]) {
    let path = args.get(2).unwrap_or_else(|| super::usage());
    let out_path = flag_value(args, "--output").or_else(|| flag_value(args, "--out"));
    let spec = parse_contract(Some(path));
    let explanation = spec.explain();
    let json = serde_json::to_string_pretty(&explanation).expect("serialize");
    if let Some(p) = &out_path {
        if let Err(e) = std::fs::write(p, format!("{json}\n")) {
            eprintln!("error writing contract explanation '{p}': {e}");
            process::exit(EXIT_USAGE);
        }
    }
    println!("{json}");
}

pub fn run_validate_contract(args: &[String]) {
    let path = args.get(2).unwrap_or_else(|| super::usage());
    let req_path = flag_value(args, "--requirements").or_else(|| flag_value(args, "--req"));
    let out_path = flag_value(args, "--output").or_else(|| flag_value(args, "--out"));
    let strict_entities = !args.iter().any(|a| a == "--no-strict-entities");
    let require_non_vacuous = !args.iter().any(|a| a == "--no-require-non-vacuous");
    let normalize = args.iter().any(|a| a == "--normalize");

    let contract_val: serde_json::Value = match serde_json::from_str(&read(path)) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("JSON parse error in '{path}': {e}");
            process::exit(EXIT_USAGE);
        }
    };
    let req_val = req_path.as_ref().map(|p| {
        serde_json::from_str::<serde_json::Value>(&read(p)).unwrap_or_else(|e| {
            eprintln!("requirements parse error in '{p}': {e}");
            process::exit(EXIT_USAGE);
        })
    });

    let report = validate_contract_value(
        &contract_val,
        req_val.as_ref(),
        strict_entities,
        require_non_vacuous,
        normalize,
    );
    let json = serde_json::to_string_pretty(&report).expect("serialize");
    if let Some(p) = &out_path {
        if let Err(e) = std::fs::write(p, format!("{json}\n")) {
            eprintln!("error writing contract validation report '{p}': {e}");
            process::exit(EXIT_USAGE);
        }
    }
    println!("{json}");
    if !report.ok {
        process::exit(EXIT_FAIL);
    }
}
