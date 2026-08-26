//! E9xx: typed data-flow checks (params, returns, call sites).

use concir::ast::Program;
use concir::validate::validate;

fn wrap(resources: &str, functions: &str) -> concir::diagnostic::ValidationReport {
    let json = format!(
        r#"{{
            "program": "p",
            "modules": [{{
                "name": "main",
                "provides": {{"resources": [], "functions": ["main"]}},
                "requires": {{"resources": [], "functions": []}},
                "resources": {resources},
                "protection": [],
                "functions": {functions}
            }}],
            "entry": "main::main"
        }}"#
    );
    let program: Program = serde_json::from_str(&json).expect("test CIR must parse");
    validate(&program)
}

fn codes(report: &concir::diagnostic::ValidationReport) -> Vec<&str> {
    report.diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn modeled_param_referenced_in_guard_is_valid() {
    let report = wrap(
        r#"[{"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}]"#,
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "statements": [{"kind": "call", "func": "worker", "args": ["n"]}], "terminator": {"kind": "goto", "target": "s2"}},
                {"sid": "s2", "terminator": {"kind": "return"}}
            ]},
            {"name": "worker", "kind": "normal",
             "params": [{"name": "n", "type": "Int", "modeled": true}],
             "body": [
                {"sid": "s1", "statements": [{"kind": "mutex_lock", "resource": "mtx"}], "terminator": {"kind": "goto", "target": "s2"}},
                {"sid": "s2", "statements": [{"kind": "mutex_unlock", "resource": "mtx"}], "terminator": {"kind": "goto", "target": "s3"}},
                {"sid": "s3", "terminator": {"kind": "return"}}
             ]}
        ]"#,
    );
    assert!(report.valid, "got: {:?}", report.diagnostics);
}

#[test]
fn unmodeled_param_referenced_in_body_is_e912() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "statements": [{"kind": "call", "func": "worker", "args": []}], "terminator": {"kind": "goto", "target": "s2"}},
                {"sid": "s2", "terminator": {"kind": "return"}}
            ]},
            {"name": "worker", "kind": "normal",
             "params": [{"name": "n", "type": "Int", "modeled": false}],
             "body": [
                {"sid": "s1", "statements": [{"kind": "nop"}],
                 "terminator": {"kind": "branch", "cond": "n > 3", "then": "s2", "else": "s3"}},
                {"sid": "s2", "terminator": {"kind": "return"}},
                {"sid": "s3", "terminator": {"kind": "return"}}
             ]}
        ]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E912"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn call_argument_arity_mismatch_is_e920() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "statements": [{"kind": "call", "func": "worker", "args": ["a", "b"]}], "terminator": {"kind": "goto", "target": "s2"}},
                {"sid": "s2", "terminator": {"kind": "return"}}
            ]},
            {"name": "worker", "kind": "normal",
             "params": [{"name": "n", "type": "Int", "modeled": true}],
             "body": [{"sid": "s1", "terminator": {"kind": "return"}}]}
        ]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E920"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn call_capture_into_non_var_resource_is_e921() {
    let report = wrap(
        r#"[{"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}]"#,
        r#"[
            {"name": "main", "kind": "normal",
             "params": [{"name": "x", "type": "Int", "modeled": true}],
             "body": [
                {"sid": "s1", "statements": [{"kind": "call", "func": "worker", "args": ["x"], "dst": "mtx"}], "terminator": {"kind": "goto", "target": "s2"}},
                {"sid": "s2", "terminator": {"kind": "return"}}
             ]},
            {"name": "worker", "kind": "normal",
             "params": [{"name": "n", "type": "Int", "modeled": true}],
             "returns": {"name": "out", "type": "Int", "modeled": true},
             "body": [{"sid": "s1", "terminator": {"kind": "return", "value": "n + 1"}}]}
        ]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E921"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn modeled_return_with_bare_return_is_e913_warning() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "statements": [{"kind": "call", "func": "worker", "args": []}], "terminator": {"kind": "goto", "target": "s2"}},
                {"sid": "s2", "terminator": {"kind": "return"}}
            ]},
            {"name": "worker", "kind": "normal",
             "returns": {"name": "out", "type": "Int", "modeled": true},
             "body": [{"sid": "s1", "terminator": {"kind": "return"}}]}
        ]"#,
    );
    assert!(report.valid, "warning only, got: {:?}", report.diagnostics);
    assert!(
        codes(&report).contains(&"E913"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn param_colliding_with_resource_is_e910() {
    let report = wrap(
        r#"[{"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}]"#,
        r#"[{"name": "main", "kind": "normal",
            "params": [{"name": "mtx", "type": "Int", "modeled": true}],
            "body": [{"sid": "s1", "terminator": {"kind": "return"}}]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E910"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn bounded_int_resource_is_valid() {
    let report = wrap(
        r#"[{"name": "count", "kind": "var", "type": "Var", "base": {"Int": [0, 10]}, "init": 3}]"#,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "statements": [{"kind": "write_shared", "resource": "count", "expr": "5"}],
             "terminator": {"kind": "goto", "target": "s2"}},
            {"sid": "s2", "terminator": {"kind": "return"}}
        ]}]"#,
    );
    assert!(report.valid, "got: {:?}", report.diagnostics);
}

#[test]
fn bounded_int_init_out_of_range_is_e208() {
    let report = wrap(
        r#"[{"name": "count", "kind": "var", "type": "Var", "base": {"Int": [0, 10]}, "init": 42}]"#,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "terminator": {"kind": "return"}}
        ]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E208"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn bounded_int_write_literal_out_of_range_is_e203() {
    let report = wrap(
        r#"[{"name": "count", "kind": "var", "type": "Var", "base": {"Int": [0, 10]}, "init": 0}]"#,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "statements": [{"kind": "write_shared", "resource": "count", "expr": "11"}],
             "terminator": {"kind": "goto", "target": "s2"}},
            {"sid": "s2", "terminator": {"kind": "return"}}
        ]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E203"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn bounded_int_lo_gt_hi_is_a_parse_error() {
    let json = r#"{
        "program": "p",
        "modules": [{
            "name": "main",
            "resources": [{"name": "count", "kind": "var", "type": "Var", "base": {"Int": [10, 0]}, "init": 0}],
            "functions": []
        }],
        "entry": "main::main"
    }"#;
    let result: Result<Program, _> = serde_json::from_str(json);
    assert!(result.is_err(), "lo > hi must be rejected at parse");
}

#[test]
fn atomic_cas_dst_bool_on_int_atomic_is_e205() {
    let report = wrap(
        r#"[{"name": "flag", "kind": "var", "type": "Atomic", "base": "Int", "init": 0}]"#,
        r#"[
            {"name": "main", "kind": "normal",
             "locals": [{"name": "ok", "type": "Bool", "modeled": true}],
             "body": [
                {"sid": "s1",
                 "statements": [{"kind": "atomic_cas", "resource": "flag",
                                 "expected": "0", "desired": "1", "dst": "ok"}],
                 "terminator": {"kind": "return"}}
             ]}
        ]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E205"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn atomic_cas_dst_old_value_same_base_is_valid() {
    let report = wrap(
        r#"[{"name": "flag", "kind": "var", "type": "Atomic", "base": "Int", "init": 0}]"#,
        r#"[
            {"name": "main", "kind": "normal",
             "locals": [{"name": "ret", "type": "Int", "modeled": true}],
             "body": [
                {"sid": "s1",
                 "statements": [{"kind": "atomic_cas", "resource": "flag",
                                 "expected": "0", "desired": "1", "dst": "ret"}],
                 "terminator": {"kind": "branch", "cond": "ret == 0",
                                "then": "s2", "else": "s1"}},
                {"sid": "s2", "terminator": {"kind": "return"}}
             ]}
        ]"#,
    );
    assert!(
        report.valid,
        "dst holds the old Int value; success is ret == expected. got: {:?}",
        report.diagnostics
    );
}

#[test]
fn channel_missing_capacity_is_e001() {
    let report = wrap(
        r#"[{"name": "tx", "kind": "sync", "type": "Channel", "mode": "Sync", "base": "Int"}]"#,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "terminator": {"kind": "return"}}
        ]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E001"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn channel_negative_capacity_is_e001() {
    let report = wrap(
        r#"[{"name": "tx", "kind": "sync", "type": "Channel", "mode": "Sync", "base": "Int", "capacity": -1}]"#,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "terminator": {"kind": "return"}}
        ]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E001"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn channel_recv_dst_type_mismatch_is_e206() {
    let report = wrap(
        r#"[{"name": "tx", "kind": "sync", "type": "Channel", "mode": "Sync", "base": "Int", "capacity": 4}]"#,
        r#"[
            {"name": "main", "kind": "normal",
             "locals": [{"name": "ok", "type": "Bool", "modeled": true}],
             "body": [
                {"sid": "s1",
                 "statements": [{"kind": "channel_recv", "channel": "tx", "dst": "ok"}],
                 "terminator": {"kind": "return"}}
             ]}
        ]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E206"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn select_channel_recv_dst_type_mismatch_is_e206() {
    let report = wrap(
        r#"[{"name": "tx", "kind": "sync", "type": "Channel", "mode": "Sync", "base": "Int", "capacity": 4}]"#,
        r#"[
            {"name": "main", "kind": "normal",
             "locals": [{"name": "ok", "type": "Bool", "modeled": true}],
             "body": [
                {"sid": "s1",
                 "terminator": {"kind": "select", "branches": [
                    {"guard": {"kind": "channel_recv", "channel": "tx", "dst": "ok"},
                     "target": "s2"}
                 ]}},
                {"sid": "s2", "terminator": {"kind": "return"}}
             ]}
        ]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E206"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn select_channel_recv_dst_matching_base_is_valid() {
    let report = wrap(
        r#"[{"name": "tx", "kind": "sync", "type": "Channel", "mode": "Sync", "base": "Int", "capacity": 4}]"#,
        r#"[
            {"name": "main", "kind": "normal",
             "locals": [{"name": "msg", "type": "Int", "modeled": true}],
             "body": [
                {"sid": "s1",
                 "terminator": {"kind": "select", "branches": [
                    {"guard": {"kind": "channel_recv", "channel": "tx", "dst": "msg"},
                     "target": "s2"}
                 ]}},
                {"sid": "s2", "terminator": {"kind": "return"}}
             ]}
        ]"#,
    );
    assert!(
        report.valid,
        "select guard dst holds the popped Channel payload. got: {:?}",
        report.diagnostics
    );
}

#[test]
fn channel_recv_discard_underscore_is_valid() {
    let report = wrap(
        r#"[{"name": "tx", "kind": "sync", "type": "Channel", "mode": "Sync", "base": "Int", "capacity": 0}]"#,
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1",
                 "statements": [{"kind": "channel_recv", "channel": "tx", "dst": "_"}],
                 "terminator": {"kind": "return"}}
             ]}
        ]"#,
    );
    assert!(
        report.valid,
        "\"_\" discards the popped payload. got: {:?}",
        report.diagnostics
    );
}
