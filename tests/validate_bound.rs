//! Function `bound`: role multiplicity for concurrent entries.

use concir::ast::Program;
use concir::validate::validate;

fn codes(report: &concir::diagnostic::ValidationReport) -> Vec<&str> {
    report.diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn bound_on_scope_target_is_valid() {
    let json = r#"{
        "program": "p",
        "modules": [{
            "name": "main",
            "provides": { "functions": ["main", "worker"] },
            "functions": [
                {
                    "name": "main",
                    "kind": "normal",
                    "body": [
                        { "sid": "s1", "kind": "scope", "funcs": ["worker"] },
                        { "sid": "s2", "kind": "return" }
                    ]
                },
                {
                    "name": "worker",
                    "kind": "normal",
                    "form": "closure",
                    "bound": 4,
                    "body": [{ "sid": "s1", "kind": "return" }]
                }
            ]
        }],
        "entry": "main::main"
    }"#;
    let program: Program = serde_json::from_str(json).unwrap();
    let report = validate(&program);
    assert!(report.valid, "got: {:?}", report.diagnostics);
    assert_eq!(program.modules[0].functions[1].bound, Some(4));
}

#[test]
fn bound_zero_is_e960() {
    let json = r#"{
        "program": "p",
        "modules": [{
            "name": "main",
            "provides": { "functions": ["main"] },
            "functions": [{
                "name": "main",
                "kind": "normal",
                "bound": 0,
                "body": [{ "sid": "s1", "kind": "return" }]
            }]
        }],
        "entry": "main::main"
    }"#;
    let program: Program = serde_json::from_str(json).unwrap();
    let report = validate(&program);
    assert!(codes(&report).contains(&"E960"));
}

#[test]
fn bound_on_sequential_only_is_e961() {
    let json = r#"{
        "program": "p",
        "modules": [{
            "name": "main",
            "provides": { "functions": ["main"] },
            "functions": [{
                "name": "main",
                "kind": "normal",
                "bound": 2,
                "body": [{ "sid": "s1", "kind": "return" }]
            }]
        }],
        "entry": "main::main"
    }"#;
    let program: Program = serde_json::from_str(json).unwrap();
    let report = validate(&program);
    assert!(codes(&report).contains(&"E961"));
    assert!(report.valid, "E961 is a warning");
}
