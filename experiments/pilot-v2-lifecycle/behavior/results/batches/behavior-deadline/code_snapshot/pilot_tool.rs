//! Small pilot tool: normalize CIR JSON to the exact serde form the backend
//! embeds, and build/validate a *legal* repair witness from adjacent
//! mutex-lock swaps.
//!
//! This is an experiment tool that reuses the public library API only. It does
//! not change core semantics. Two subcommands:
//!
//!   pilot_tool normalize <program.json|contract.json> --kind program|contract
//!   pilot_tool witness <program.json> <contract.json> --engine petri|interp \
//!       --swap module:function:sidA:sidB [--swap ...]
//!
//! `normalize` lets the runner bind an artifact's embedded program/contract to
//! the external input files by comparing the backend's own serialization.
//!
//! `witness` applies the given adjacent swaps in order, records the same
//! `AppliedEdit` chain the search would produce (original function hash, parent
//! and child program fingerprints, permission check), and verifies the final
//! program against the supplied frozen contract.

use std::process;

use concir::ast::Program;
use concir::explore::contract::ContractSpec;
use concir::explore::{verify_program, EngineKind};
use concir::repair::patch::{self, CirPatch, PatchChange, SourceRelation};
use concir::repair::search::{program_fingerprint, AppliedEdit};
use concir::validate;

fn usage() -> ! {
    eprintln!(
        "usage:\n  pilot_tool normalize <file.json> --kind program|contract\n  \
         pilot_tool witness <program.json> <contract.json> [--engine petri|interp] \
         [--swap module:function:sidA:sidB ...]"
    );
    process::exit(2);
}

fn read(path: &str) -> String {
    match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("read {path}: {e}");
            process::exit(2);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        usage();
    }
    match args[1].as_str() {
        "normalize" => {
            let path = args.get(2).unwrap_or_else(|| usage());
            let kind = args
                .iter()
                .position(|a| a == "--kind")
                .and_then(|i| args.get(i + 1))
                .map(String::as_str)
                .unwrap_or_else(|| usage());
            let text = read(path);
            let out = match kind {
                "program" => match serde_json::from_str::<Program>(&text) {
                    Ok(v) => serde_json::to_string(&v).unwrap(),
                    Err(e) => {
                        eprintln!("parse program: {e}");
                        process::exit(2);
                    }
                },
                "contract" => match serde_json::from_str::<ContractSpec>(&text) {
                    Ok(v) => serde_json::to_string(&v).unwrap(),
                    Err(e) => {
                        eprintln!("parse contract: {e}");
                        process::exit(2);
                    }
                },
                _ => usage(),
            };
            println!("{out}");
        }
        "witness" => {
            let prog_path = args.get(2).unwrap_or_else(|| usage());
            let spec_path = args.get(3).unwrap_or_else(|| usage());
            let mut engine = EngineKind::Petri;
            let mut swaps: Vec<(String, String, String, String)> = Vec::new();
            let mut i = 4;
            while i < args.len() {
                match args[i].as_str() {
                    "--engine" => {
                        i += 1;
                        engine = match args.get(i).map(String::as_str) {
                            Some("interp") => EngineKind::Interpreter,
                            _ => EngineKind::Petri,
                        };
                    }
                    "--swap" => {
                        i += 1;
                        let raw = args.get(i).cloned().unwrap_or_else(|| usage());
                        let parts: Vec<&str> = raw.split(':').collect();
                        if parts.len() != 4 {
                            eprintln!("--swap expects module:function:sidA:sidB");
                            process::exit(2);
                        }
                        swaps.push((
                            parts[0].into(),
                            parts[1].into(),
                            parts[2].into(),
                            parts[3].into(),
                        ));
                    }
                    _ => usage(),
                }
                i += 1;
            }
            let program: Program = serde_json::from_str(&read(prog_path)).unwrap_or_else(|e| {
                eprintln!("parse program: {e}");
                process::exit(2);
            });
            let spec: ContractSpec = serde_json::from_str(&read(spec_path)).unwrap_or_else(|e| {
                eprintln!("parse contract: {e}");
                process::exit(2);
            });

            let mut current = program;
            let mut chain: Vec<AppliedEdit> = Vec::new();
            let mut permissions: Vec<serde_json::Value> = Vec::new();
            let mut error: Option<String> = None;
            for (module, function, a, b) in &swaps {
                let original_hash = match patch::function_hash(&current, module, function) {
                    Ok(h) => h,
                    Err(e) => {
                        error = Some(format!("function_hash {module}::{function}: {e}"));
                        break;
                    }
                };
                let patch = CirPatch {
                    id: format!("witness:{module}:{function}:{a}-{b}"),
                    module: module.clone(),
                    function: function.clone(),
                    original_hash,
                    changes: vec![PatchChange::SwapStatements { a: a.clone(), b: b.clone() }],
                    provenance: vec![SourceRelation {
                        description: "legal adjacent mutex-lock swap (pilot witness)".into(),
                    }],
                };
                let allowed = patch::check_allowed(&spec.allowed_scope, &patch);
                let allowed_ok = allowed.is_ok();
                permissions.push(serde_json::json!({
                    "module": module, "function": function,
                    "allowed": allowed_ok,
                    "allowed_error": allowed.err().map(|e| e.to_string()),
                }));
                if allowed_ok {
                    // permission evidence only; application re-checks the hash.
                }
                let (next, _diff) = match patch::apply(&current, &patch) {
                    Ok(v) => v,
                    Err(e) => {
                        error = Some(format!("apply {module}::{function}: {e}"));
                        break;
                    }
                };
                chain.push(AppliedEdit {
                    module: module.clone(),
                    function: function.clone(),
                    changes: patch.changes.clone(),
                    provenance: patch.provenance.clone(),
                    original_function_hash: patch.original_hash.clone(),
                    parent_fingerprint: program_fingerprint(&current),
                    program_fingerprint: program_fingerprint(&next),
                });
                current = next;
            }

            let static_ok = validate::validate(&current).valid;
            let report = verify_program(&current, &spec, engine);
            let out = serde_json::json!({
                "error": error,
                "chain": chain,
                "permissions": permissions,
                "static_valid": static_ok,
                "final_outcome": report.outcome,
                "final_complete": report.complete,
                "final_states": report.states_explored,
                "program": current,
            });
            println!("{}", serde_json::to_string(&out).unwrap());
        }
        _ => usage(),
    }
}
