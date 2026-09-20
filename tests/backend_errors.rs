//! Malformed CIR must lower to an `E\d{3}` error, never panic.

use concir::ast::Program;
use concir::sem::program;

const BAD_FUNCTION_NAME: &str = r#"{
  "program": "bad-name",
  "version": "3.5.0",
  "entry": "main::main",
  "modules": [{
    "name": "main",
    "resources": [],
    "protection": [],
    "functions": [
      {"name": "main", "kind": "normal", "body": [{"sid": "s1", "kind": "return"}]},
      {"name": "A::b", "kind": "normal", "body": [{"sid": "s1", "kind": "return"}]}
    ]
  }]
}"#;

#[test]
fn function_name_with_fqn_separator_is_an_error_not_a_panic() {
    let program: Program = serde_json::from_str(BAD_FUNCTION_NAME).unwrap();
    let err = program::lower(&program).expect_err("must be rejected");
    let text = format!("{err:?}");
    assert!(text.contains("E102"), "expected E102, got: {text}");
}
