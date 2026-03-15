use std::env;
use std::fs;
use std::process;

use ceir::diagnostic::ValidationReport;
use ceir::lexer::Lexer;
use ceir::parser::Parser;
use ceir::validate;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: ceir <file.cir>");
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
    let mut lexer = Lexer::new(source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(e) => {
            let (line, col) = e.span.line_col(source);
            return ValidationReport {
                valid: false,
                diagnostics: vec![ceir::diagnostic::Diagnostic::error(
                    "E000",
                    format!("lex error at {line}:{col}: {}", e.message),
                )
                .with_span(e.span, source)],
            };
        }
    };

    let mut parser = Parser::new(tokens);
    let program = match parser.parse_program() {
        Ok(p) => p,
        Err(e) => {
            let (line, col) = e.span.line_col(source);
            return ValidationReport {
                valid: false,
                diagnostics: vec![ceir::diagnostic::Diagnostic::error(
                    "E000",
                    format!("parse error at {line}:{col}: {}", e.message),
                )
                .with_span(e.span, source)],
            };
        }
    };

    validate::validate(&program, source)
}
