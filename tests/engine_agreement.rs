//! The Petri net and the reference interpreter must agree on every model.
//!
//! External benchmark trees are supplied through `CONCIR_ENGINE_AGREEMENT_DIRS`
//! (colon-separated); when unset, only the repository examples/tests are
//! checked. Any disagreement fails with the case name.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use concir::ast::Program;
use concir::explore::contract::ContractSpec;
use concir::explore::{verify_program, EngineKind};

fn collect_json(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name == "target" || name == ".git" || name.starts_with('.') {
                continue;
            }
            collect_json(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("json") {
            out.push(path);
        }
    }
}

fn contract_for(program: &Path) -> ContractSpec {
    let dir = program.parent().unwrap_or(Path::new("."));
    let stem = program
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .trim_end_matches(".json");
    let explicit = [
        dir.join(format!("{stem}_contract.json")),
        dir.join("contract.json"),
    ]
    .into_iter()
    .find(|p| p.is_file());
    if let Some(path) = explicit {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(spec) = serde_json::from_str::<ContractSpec>(&text) {
                return spec;
            }
        }
    }
    serde_json::from_str(r#"{"name":"engine-agreement","properties":[]}"#)
        .expect("empty contract parses")
}

fn root_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples")];
    let mut tests = Vec::new();
    collect_json(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests"),
        &mut tests,
    );
    if let Ok(extra) = std::env::var("CONCIR_ENGINE_AGREEMENT_DIRS") {
        for part in extra.split(':').filter(|p| !p.is_empty()) {
            dirs.push(PathBuf::from(part));
        }
    }
    dirs
}

#[test]
fn petri_and_interp_agree_on_all_models() {
    let mut files = Vec::new();
    for dir in root_dirs() {
        collect_json(&dir, &mut files);
    }
    let mut disagreements = Vec::new();
    let mut checked = 0usize;
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for path in files {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if name.ends_with("_contract.json") || !seen.insert(path.display().to_string()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(program) = serde_json::from_str::<Program>(&text) else {
            continue; // not a program (patch, artifact, contract, ...)
        };
        let spec = contract_for(&path);
        let petri = verify_program(&program, &spec, EngineKind::Petri);
        let interp = verify_program(&program, &spec, EngineKind::Interpreter);
        checked += 1;
        if petri.outcome != interp.outcome || petri.complete != interp.complete {
            disagreements.push(format!(
                "{}: petri={:?}/{} interp={:?}/{}",
                path.display(),
                petri.outcome,
                petri.complete,
                interp.outcome,
                interp.complete
            ));
        }
    }
    assert!(checked > 0, "no programs found to compare");
    assert!(
        disagreements.is_empty(),
        "petri/interp disagreements:\n{}",
        disagreements.join("\n")
    );
}
