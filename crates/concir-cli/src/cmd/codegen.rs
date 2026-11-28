use std::process;

use concir_sem::program;

use super::{flag_value, parse_program, versioned, EXIT_INVALID, EXIT_UNSUPPORTED, EXIT_USAGE};

pub fn run_export_cvn(args: &[String]) {
    let path = args.get(2).unwrap_or_else(|| super::usage());
    let out_path = flag_value(args, "--output").or_else(|| flag_value(args, "--out"));
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
        if let Err(e) = std::fs::write(p, format!("{json}\n")) {
            eprintln!("error writing CVN export '{p}': {e}");
            process::exit(EXIT_USAGE);
        }
    }
    println!("{json}");
}

pub fn run_codegen(args: &[String]) {
    let path = args.get(2).unwrap_or_else(|| super::usage());
    let out = flag_value(args, "--out")
        .or_else(|| args.get(3).cloned())
        .unwrap_or_else(|| super::usage());
    let program = parse_program(path);
    let sem = match program::lower(&program) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot lower program: {e}");
            process::exit(EXIT_INVALID);
        }
    };
    match concir_codegen::generate(&sem) {
        Ok(generated) => {
            if let Err(e) = concir_codegen::write_project(std::path::Path::new(&out), &generated) {
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
