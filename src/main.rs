use std::env;
use std::fs;
use std::process;

use cir::ast::Program;
use cir::diagnostic::{Diagnostic, ValidationReport};
use cir::validate;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: cir <file.json>");
        process::exit(2);
    }

    let path = &args[1];
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading '{path}': {e}");
            process::exit(2);
        }
    };

    let report = run(&source);
    let json = serde_json::to_string_pretty(&report).expect("failed to serialize report");
    println!("{json}");

    if !report.valid {
        process::exit(1);
    }
}

fn run(source: &str) -> ValidationReport {
    let program: Program = match serde_json::from_str(source) {
        Ok(p) => p,
        Err(e) => {
            return ValidationReport {
                valid: false,
                diagnostics: vec![Diagnostic::error("E000", format!("JSON parse error: {e}"))],
            };
        }
    };

    validate::validate(&program)
}
