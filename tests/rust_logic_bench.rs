//! Oracle check for the Rust concurrency-logic benchmark.
//!
//! Every reference lowering must PASS its frozen contract, and every bundled
//! buggy lowering must FAIL. The prompts are what a model sees; these programs
//! are the hand-lowered protocols (explicit unlock at guard drop).

use std::fs;
use std::path::PathBuf;

use concir::ast::Program;
use concir::explore::contract::ContractSpec;
use concir::explore::{verify_program, EngineKind};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Task {
    id: String,
    reference_outcome: String,
    bug_outcome: String,
}

#[test]
fn rust_logic_oracle_matches_manifest() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("benchmark/rust-logic");
    let manifest: Vec<Task> =
        serde_json::from_str(&fs::read_to_string(root.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest.len(), 30, "expected 30 tasks");

    let mut failures = Vec::new();
    for task in &manifest {
        let dir = root.join("tasks").join(&task.id);
        let spec: ContractSpec =
            serde_json::from_str(&fs::read_to_string(dir.join("contract.json")).unwrap()).unwrap();
        for (which, expected) in [("reference", &task.reference_outcome), ("bug", &task.bug_outcome)]
        {
            let program: Program = serde_json::from_str(
                &fs::read_to_string(dir.join(format!("{which}.json"))).unwrap(),
            )
            .unwrap();
            let report = verify_program(&program, &spec, EngineKind::Petri);
            let got = report.outcome.as_str();
            if got != expected {
                failures.push(format!(
                    "{} {which}: got {got}, expected {expected}; states={} complete={} invalid={:?} unsupported={:?}",
                    task.id,
                    report.states_explored,
                    report.complete,
                    report.invalid,
                    report.unsupported
                ));
            }
        }
    }

    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
