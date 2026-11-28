use std::process;

use concir_ir::{schema, summarize, validate};
use concir_sem::program;

use super::{flag_value, parse_program, parse_program_raw, EXIT_INVALID, EXIT_UNSUPPORTED, EXIT_USAGE};

pub fn run_schema() {
    let value = schema::schema();
    println!("{}", serde_json::to_string_pretty(&value).expect("serialize"));
}

pub fn run_lower(args: &[String]) {
    let path = args.get(2).unwrap_or_else(|| super::usage());
    let out_path = flag_value(args, "--out");
    let trace_map_path = flag_value(args, "--trace-map");
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
        if let Err(e) = std::fs::write(p, format!("{lowered_json}\n")) {
            eprintln!("error writing lowered program '{p}': {e}");
            process::exit(EXIT_USAGE);
        }
    }
    if let Some(p) = &trace_map_path {
        let tm_json = serde_json::to_string_pretty(&trace_map).expect("serialize");
        if let Err(e) = std::fs::write(p, format!("{tm_json}\n")) {
            eprintln!("error writing trace map '{p}': {e}");
            process::exit(EXIT_USAGE);
        }
    }
    if let Some(tm_only) = flag_value(args, "--print-trace-map") {
        let _ = tm_only;
    }
    println!("{lowered_json}");
}

pub fn run_summarize(args: &[String]) {
    let path = args.get(2).unwrap_or_else(|| super::usage());
    let module_filter = flag_value(args, "--module");
    let out_path = flag_value(args, "--out");
    let program = parse_program(path);
    let report = match summarize::summarize_program(&program, module_filter.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("summarize error: {e}");
            process::exit(EXIT_INVALID);
        }
    };
    let json = serde_json::to_string_pretty(&report).expect("serialize");
    if let Some(p) = &out_path {
        if let Err(e) = std::fs::write(p, format!("{json}\n")) {
            eprintln!("error writing summary '{p}': {e}");
            process::exit(EXIT_USAGE);
        }
    }
    println!("{json}");
    if report.cross_module_lock_cycle {
        process::exit(super::EXIT_FAIL);
    }
}

pub fn run_check(args: &[String]) {
    let path = args.get(2).unwrap_or_else(|| super::usage());
    let program = parse_program_raw(path, EXIT_USAGE);
    let report = validate::validate(&program);
    println!("{}", serde_json::to_string_pretty(&report).expect("serialize"));
    if !report.valid {
        process::exit(EXIT_INVALID);
    }
}

pub fn run_support(args: &[String]) {
    let path = args.get(2).unwrap_or_else(|| super::usage());
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
