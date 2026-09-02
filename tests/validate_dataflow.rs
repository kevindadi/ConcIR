//! E9xx: typed data-flow checks (params, returns, call sites).

use concir::ast::Program;
use concir::validate::validate;

fn wrap(resources: &str, functions: &str) -> concir::diagnostic::ValidationReport {
    wrap_prot(resources, "[]", functions)
}

fn wrap_prot(
    resources: &str,
    protection: &str,
    functions: &str,
) -> concir::diagnostic::ValidationReport {
    let json = format!(
        r#"{{
            "program": "p",
            "modules": [{{
                "name": "main",
                "provides": {{"resources": [], "functions": ["main"]}},
                "requires": {{"resources": [], "functions": []}},
                "resources": {resources},
                "protection": {protection},
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
                {"sid": "s1", "kind": "call", "func": "worker", "args": ["0"]},
                {"sid": "s2", "kind": "return"}
            ]},
            {"name": "worker", "kind": "normal",
             "params": [{"name": "n", "type": "Int", "modeled": true}],
             "body": [
                {"sid": "s1", "kind": "mutex_lock", "resource": "mtx"},
                {"sid": "s2", "kind": "mutex_unlock", "resource": "mtx"},
                {"sid": "s3", "kind": "return"}
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
                {"sid": "s1", "kind": "call", "func": "worker", "args": []},
                {"sid": "s2", "kind": "return"}
            ]},
            {"name": "worker", "kind": "normal",
             "params": [{"name": "n", "type": "Int", "modeled": false}],
             "body": [
                {"sid": "s1", "kind": "branch", "cond": "n > 3", "then": "s2", "else": "s3"},
                {"sid": "s2", "kind": "return"},
                {"sid": "s3", "kind": "return"}
             ]}
        ]"#,
    );
    assert!(
        report.valid,
        "E912 is a warning, got: {:?}",
        report.diagnostics
    );
    assert!(
        codes(&report).contains(&"E912"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn call_capture_into_local_is_valid() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal",
             "locals": [{"name": "out", "type": "Int", "modeled": true}],
             "body": [
                {"sid": "s1", "kind": "call", "func": "worker", "args": ["1"], "dst": "out"},
                {"sid": "s2", "kind": "return"}
             ]},
            {"name": "worker", "kind": "normal",
             "params": [{"name": "n", "type": "Int", "modeled": true}],
             "returns": {"name": "r", "type": "Int", "modeled": true},
             "body": [{"sid": "s1", "kind": "return", "value": "n + 1"}]}
        ]"#,
    );
    assert!(report.valid, "got: {:?}", report.diagnostics);
}

#[test]
fn call_capture_without_modeled_return_is_e923() {
    let report = wrap(
        r#"[{"name": "flag", "kind": "var", "type": "Var", "base": "Int", "init": 0}]"#,
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "kind": "call", "func": "worker", "args": [], "dst": "flag"},
                {"sid": "s2", "kind": "return"}
            ]},
            {"name": "worker", "kind": "normal",
             "body": [{"sid": "s1", "kind": "return"}]}
        ]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E923"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn assign_local_to_resource_is_e936() {
    let report = wrap(
        r#"[{"name": "count", "kind": "var", "type": "Var", "base": "Int", "init": 0}]"#,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "assign_local", "target": "count", "expr": "1"},
            {"sid": "s2", "kind": "return"}
        ]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E936"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn local_colliding_with_resource_is_e914() {
    let report = wrap(
        r#"[{"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}]"#,
        r#"[{"name": "main", "kind": "normal",
            "locals": [{"name": "mtx", "type": "Int", "modeled": false}],
            "body": [{"sid": "s1", "kind": "return"}]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E914"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn duplicate_local_is_e915() {
    let report = wrap(
        "[]",
        r#"[{"name": "main", "kind": "normal",
            "locals": [
                {"name": "tmp", "type": "Int", "modeled": false},
                {"name": "tmp", "type": "Bool", "modeled": false}
            ],
            "body": [{"sid": "s1", "kind": "return"}]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E915"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn switch_on_undefined_name_is_e935() {
    let report = wrap(
        "[]",
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "switch", "var": "missing",
             "cases": {"A": "s2"}, "default": "s2"},
            {"sid": "s2", "kind": "return"}
        ]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E935"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn switch_on_local_enum_is_valid() {
    let report = wrap(
        "[]",
        r#"[{"name": "main", "kind": "normal",
            "locals": [{"name": "st", "type": {"Enum": ["A", "B"]}, "modeled": true, "init": "A"}],
            "body": [
                {"sid": "s1", "kind": "switch", "var": "st",
                 "cases": {"A": "s2", "B": "s3"}, "default": "s2"},
                {"sid": "s2", "kind": "return"},
                {"sid": "s3", "kind": "return"}
            ]}]"#,
    );
    assert!(report.valid, "got: {:?}", report.diagnostics);
}

#[test]
fn scope_of_modeled_param_function_is_e922() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "kind": "scope", "funcs": ["worker"]},
                {"sid": "s2", "kind": "return"}
            ]},
            {"name": "worker", "kind": "normal",
             "params": [{"name": "n", "type": "Int", "modeled": true}],
             "body": [{"sid": "s1", "kind": "return"}]}
        ]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E922"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn spawn_args_arity_mismatch_is_e924() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "kind": "spawn", "func": "worker", "args": ["a", "b"], "handle": "h"},
                {"sid": "s2", "kind": "join", "handle": "h"},
                {"sid": "s3", "kind": "return"}
            ]},
            {"name": "worker", "kind": "normal",
             "params": [{"name": "label", "type": "String", "modeled": false}],
             "body": [{"sid": "s1", "kind": "return"}]}
        ]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E924"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn modeled_local_on_spawn_target_is_e937_warning() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "kind": "scope", "funcs": ["worker"]},
                {"sid": "s2", "kind": "return"}
            ]},
            {"name": "worker", "kind": "normal",
             "locals": [{"name": "tmp", "type": "Int", "modeled": true}],
             "body": [{"sid": "s1", "kind": "return"}]}
        ]"#,
    );
    assert!(report.valid, "warning only, got: {:?}", report.diagnostics);
    assert!(
        codes(&report).contains(&"E937"),
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
                {"sid": "s1", "kind": "call", "func": "worker", "args": ["a", "b"]},
                {"sid": "s2", "kind": "return"}
            ]},
            {"name": "worker", "kind": "normal",
             "params": [{"name": "n", "type": "Int", "modeled": true}],
             "body": [{"sid": "s1", "kind": "return"}]}
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
                {"sid": "s1", "kind": "call", "func": "worker", "args": ["x"], "dst": "mtx"},
                {"sid": "s2", "kind": "return"}
             ]},
            {"name": "worker", "kind": "normal",
             "params": [{"name": "n", "type": "Int", "modeled": true}],
             "returns": {"name": "out", "type": "Int", "modeled": true},
             "body": [{"sid": "s1", "kind": "return", "value": "n + 1"}]}
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
                {"sid": "s1", "kind": "call", "func": "worker", "args": []},
                {"sid": "s2", "kind": "return"}
            ]},
            {"name": "worker", "kind": "normal",
             "returns": {"name": "out", "type": "Int", "modeled": true},
             "body": [{"sid": "s1", "kind": "return"}]}
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
            "body": [{"sid": "s1", "kind": "return"}]}]"#,
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
            {"sid": "s1", "kind": "write_shared", "resource": "count", "expr": "5"},
            {"sid": "s2", "kind": "return"}
        ]}]"#,
    );
    assert!(report.valid, "got: {:?}", report.diagnostics);
}

#[test]
fn bounded_int_init_out_of_range_is_e208() {
    let report = wrap(
        r#"[{"name": "count", "kind": "var", "type": "Var", "base": {"Int": [0, 10]}, "init": 42}]"#,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "return"}
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
            {"sid": "s1", "kind": "write_shared", "resource": "count", "expr": "11"},
            {"sid": "s2", "kind": "return"}
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
                {"sid": "s1", "kind": "atomic_cas", "resource": "flag",
                 "expected": "0", "desired": "1", "dst": "ok"},
                {"sid": "s2", "kind": "return"}
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
                {"sid": "s1", "kind": "atomic_cas", "resource": "flag",
                 "expected": "0", "desired": "1", "dst": "ret"},
                {"sid": "s2", "kind": "branch", "cond": "ret == 0",
                 "then": "s3", "else": "s1"},
                {"sid": "s3", "kind": "return"}
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
            {"sid": "s1", "kind": "return"}
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
            {"sid": "s1", "kind": "return"}
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
                {"sid": "s1", "kind": "channel_recv", "channel": "tx", "dst": "ok"},
                {"sid": "s2", "kind": "return"}
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
                {"sid": "s1", "kind": "select", "branches": [
                    {"guard": {"kind": "channel_recv", "channel": "tx", "dst": "ok"},
                     "target": "s2"}
                ]},
                {"sid": "s2", "kind": "return"}
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
                {"sid": "s1", "kind": "select", "branches": [
                    {"guard": {"kind": "channel_recv", "channel": "tx", "dst": "msg"},
                     "target": "s2"}
                ]},
                {"sid": "s2", "kind": "return"}
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
                {"sid": "s1", "kind": "channel_recv", "channel": "tx", "dst": "_"},
                {"sid": "s2", "kind": "return"}
             ]}
        ]"#,
    );
    assert!(
        report.valid,
        "\"_\" discards the popped payload. got: {:?}",
        report.diagnostics
    );
}

#[test]
fn undefined_name_in_expr_is_e931() {
    let report = wrap(
        r#"[{"name": "count", "kind": "var", "type": "Var", "base": "Int", "init": 0}]"#,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "write_shared", "resource": "count", "expr": "nope + 1"},
            {"sid": "s2", "kind": "return"}
        ]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E931"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn mutex_in_expression_is_e934() {
    let report = wrap(
        r#"[{"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"},
            {"name": "count", "kind": "var", "type": "Var", "base": "Int", "init": 0}]"#,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "write_shared", "resource": "count", "expr": "mtx"},
            {"sid": "s2", "kind": "return"}
        ]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E934"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn positional_struct_literal_is_e931() {
    let report = wrap(
        r#"[{"name": "pt", "kind": "var", "type": "Var", "base": {"Struct": {"x": "Int", "y": "Int"}}, "init": {"x": 0, "y": 0}}]"#,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "write_shared", "resource": "pt", "expr": "{1, 2}"},
            {"sid": "s2", "kind": "return"}
        ]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E931"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn named_struct_literal_is_valid() {
    let report = wrap(
        r#"[{"name": "pt", "kind": "var", "type": "Var", "base": {"Struct": {"x": "Int", "y": "Int"}}, "init": {"x": 0, "y": 0}}]"#,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "write_shared", "resource": "pt", "expr": "{x: 1, y: 2}"},
            {"sid": "s2", "kind": "return"}
        ]}]"#,
    );
    assert!(report.valid, "got: {:?}", report.diagnostics);
}

#[test]
fn struct_field_in_guard_is_valid() {
    let report = wrap(
        r#"[{"name": "pt", "kind": "var", "type": "Var", "base": {"Struct": {"x": "Int", "ready": "Bool"}}, "init": {"x": 0, "ready": false}}]"#,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "branch", "cond": "pt.ready == true", "then": "s2", "else": "s3"},
            {"sid": "s2", "kind": "return"},
            {"sid": "s3", "kind": "return"}
        ]}]"#,
    );
    assert!(report.valid, "got: {:?}", report.diagnostics);
}

#[test]
fn unknown_struct_field_is_e933() {
    let report = wrap(
        r#"[{"name": "pt", "kind": "var", "type": "Var", "base": {"Struct": {"x": "Int"}}, "init": {"x": 0}}]"#,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "branch", "cond": "pt.nope == 0", "then": "s2", "else": "s3"},
            {"sid": "s2", "kind": "return"},
            {"sid": "s3", "kind": "return"}
        ]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E933"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn examples_still_validate() {
    for name in [
        "producer_consumer",
        "async_workers",
        "complex_rwlock",
        "state_machine",
        "with_summary",
    ] {
        let json = std::fs::read_to_string(format!("examples/{name}.json"))
            .unwrap_or_else(|e| panic!("read {name}: {e}"));
        let program: Program =
            serde_json::from_str(&json).unwrap_or_else(|e| panic!("{name}: {e}"));
        let report = validate(&program);
        assert!(
            report.valid,
            "{name} should stay valid after the expression parser. got: {:?}",
            report.diagnostics
        );
    }
}

#[test]
fn spawn_arg_undefined_name_is_e931() {
    let report = wrap(
        "[]",
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "kind": "spawn", "func": "worker", "args": ["nope"], "handle": "h"},
                {"sid": "s2", "kind": "join", "handle": "h"},
                {"sid": "s3", "kind": "return"}
            ]},
            {"name": "worker", "kind": "normal",
             "params": [{"name": "n", "type": "Int", "modeled": false}],
             "body": [
                {"sid": "s1", "kind": "return"}
             ]}
        ]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E931"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn bare_identifier_branch_is_e201() {
    let report = wrap(
        r#"[{"name": "flag", "kind": "var", "type": "Var", "base": "Bool", "init": false}]"#,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "branch", "cond": "flag", "then": "s2", "else": "s3"},
            {"sid": "s2", "kind": "return"},
            {"sid": "s3", "kind": "return"}
        ]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E201"),
        "got: {:?}",
        report.diagnostics
    );
}

fn prot_count_mtx() -> (&'static str, &'static str) {
    (
        r#"[{"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"},
            {"name": "count", "kind": "var", "type": "Var", "base": "Int", "init": 0}]"#,
        r#"[{"var": "count", "lock": "mtx"}]"#,
    )
}

#[test]
fn branch_on_protected_var_without_lock_is_e309() {
    let (resources, protection) = prot_count_mtx();
    let report = wrap_prot(
        resources,
        protection,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "branch", "cond": "count > 0", "then": "s2", "else": "s3"},
            {"sid": "s2", "kind": "return"},
            {"sid": "s3", "kind": "return"}
        ]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E309"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn fqn_in_guard_without_lock_is_e309() {
    let (resources, protection) = prot_count_mtx();
    let report = wrap_prot(
        resources,
        protection,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "branch", "cond": "main::count > 0", "then": "s2", "else": "s3"},
            {"sid": "s2", "kind": "return"},
            {"sid": "s3", "kind": "return"}
        ]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E309"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn branch_on_protected_var_with_lock_is_valid() {
    let (resources, protection) = prot_count_mtx();
    let report = wrap_prot(
        resources,
        protection,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "mutex_lock", "resource": "mtx"},
            {"sid": "s2", "kind": "branch", "cond": "count > 0", "then": "s3", "else": "s3"},
            {"sid": "s3", "kind": "mutex_unlock", "resource": "mtx"},
            {"sid": "s4", "kind": "return"}
        ]}]"#,
    );
    assert!(
        report.valid,
        "holding the lock covers a guard on the Var. got: {:?}",
        report.diagnostics
    );
}

#[test]
fn write_expr_reading_other_protected_var_is_e309() {
    let report = wrap_prot(
        r#"[{"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"},
            {"name": "count", "kind": "var", "type": "Var", "base": "Int", "init": 0},
            {"name": "out", "kind": "var", "type": "Var", "base": "Int", "init": 0}]"#,
        r#"[{"var": "count", "lock": "mtx"}]"#,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "write_shared", "resource": "out", "expr": "count + 1"},
            {"sid": "s2", "kind": "return"}
        ]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E309"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn call_arg_protected_var_without_lock_is_e309() {
    let (resources, protection) = prot_count_mtx();
    let report = wrap_prot(
        resources,
        protection,
        r#"[
            {"name": "main", "kind": "normal", "body": [
                {"sid": "s1", "kind": "call", "func": "worker", "args": ["count"]},
                {"sid": "s2", "kind": "return"}
            ]},
            {"name": "worker", "kind": "normal",
             "params": [{"name": "n", "type": "Int", "modeled": true}],
             "body": [
                {"sid": "s1", "kind": "return"}
             ]}
        ]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E309"),
        "got: {:?}",
        report.diagnostics
    );
}

#[test]
fn switch_on_protected_var_without_lock_is_e309() {
    let (resources, protection) = prot_count_mtx();
    let report = wrap_prot(
        resources,
        protection,
        r#"[{"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "switch", "var": "count",
             "cases": {"0": "s2"}, "default": "s2"},
            {"sid": "s2", "kind": "return"}
        ]}]"#,
    );
    assert!(!report.valid);
    assert!(
        codes(&report).contains(&"E309"),
        "got: {:?}",
        report.diagnostics
    );
}
