//! Bounded monitor: model-free requirement checking over cir_trace streams.

use std::collections::BTreeMap;

use concir::monitor::{monitor, parse_trace};
use serde_json::json;

fn resources() -> Vec<String> {
    vec!["a".to_string(), "b".to_string(), "m".to_string(), "ch".to_string()]
}

fn map_of() -> BTreeMap<String, String> {
    BTreeMap::new()
}

fn status(report: &concir::monitor::MonitorReport, id: &str) -> String {
    report
        .properties
        .iter()
        .find(|p| p.id == id)
        .unwrap_or_else(|| panic!("no property {id}"))
        .status
        .clone()
}

const FIXED_ABBA: &str = r#"{"t":"t1","sid":"L1","op":"mutex_lock","r":"a"}
{"t":"t1","sid":"L2","op":"mutex_lock","r":"b"}
{"t":"t1","sid":"L3","op":"mutex_unlock","r":"b"}
{"t":"t1","sid":"L4","op":"mutex_unlock","r":"a"}
{"t":"t2","sid":"L1","op":"mutex_lock","r":"a"}
{"t":"t2","sid":"L2","op":"mutex_lock","r":"b"}
{"t":"t2","sid":"L3","op":"mutex_unlock","r":"b"}
{"t":"t2","sid":"L4","op":"mutex_unlock","r":"a"}
"#;

#[test]
fn holds_all_is_observed_on_a_conformant_trace() {
    let contract = json!({
        "name": "abba",
        "properties": [{"kind": "deadlock_free", "id": "no-deadlock", "req": ["R5"]}],
        "preserved": [
            {"kind": "reachable", "description": "t1 holds both",
             "goal": {"kind": "holds_all", "function": "main::t1",
                      "resources": ["main::a", "main::b"]}, "req": ["R3"]},
            {"kind": "reachable", "description": "t2 completes",
             "goal": {"kind": "function_completed", "function": "main::t2"}, "req": ["R6"]}
        ]
    });
    let traces = vec![parse_trace(FIXED_ABBA)];
    let report = monitor(&contract, &["a".to_string(), "b".to_string()], &map_of(), &traces);
    assert_eq!(report.status, "ok");
    assert!(report.bounded);
    assert_eq!(status(&report, "no-deadlock"), "deferred");
    assert_eq!(status(&report, "t1 holds both"), "PASS_bounded");
    assert_eq!(status(&report, "t2 completes"), "not_observed");
    assert!(report.unmapped_resources.is_empty());
    // requirement tags flow through
    let holds = report.properties.iter().find(|p| p.id == "t1 holds both").unwrap();
    assert_eq!(holds.req, vec!["R3".to_string()]);
}

#[test]
fn mutex_exclusive_fails_when_two_threads_hold_the_same_lock() {
    let contract = json!({
        "properties": [{"kind": "safety", "id": "excl",
                        "invariant": {"kind": "mutex_exclusive", "resource": "main::m"}}]
    });
    let trace = r#"{"t":"t0","op":"mutex_lock","r":"m"}
{"t":"t1","op":"mutex_lock","r":"m"}
"#;
    let report = monitor(&contract, &resources(), &map_of(), &[parse_trace(trace)]);
    assert_eq!(report.status, "fail");
    assert_eq!(status(&report, "excl"), "FAIL");
}

#[test]
fn never_holds_all_fails_when_a_thread_holds_both() {
    let contract = json!({
        "properties": [{"kind": "safety", "id": "nh",
                        "invariant": {"kind": "never_holds_all", "function": "main::t1",
                                      "resources": ["main::a", "main::b"]}}]
    });
    let report = monitor(&contract, &resources(), &map_of(), &[parse_trace(FIXED_ABBA)]);
    assert_eq!(report.status, "fail");
    assert_eq!(status(&report, "nh"), "FAIL");
}

#[test]
fn unmapped_resource_is_reported_not_failed() {
    let contract = json!({
        "properties": [{"kind": "safety", "id": "excl",
                        "invariant": {"kind": "mutex_exclusive", "resource": "main::zzz"}}]
    });
    let report = monitor(&contract, &resources(), &map_of(), &[parse_trace(FIXED_ABBA)]);
    assert_eq!(status(&report, "excl"), "unmapped");
    assert_eq!(report.status, "ok");
}

#[test]
fn unresolved_value_predicate_is_unsupported() {
    let contract = json!({
        "properties": [{"kind": "reachability", "id": "flag",
                        "goal": {"kind": "var_eq", "resource": "main::ready", "value": true}}]
    });
    let report = monitor(&contract, &resources(), &map_of(), &[parse_trace(FIXED_ABBA)]);
    assert_eq!(status(&report, "flag"), "unsupported");
}

#[test]
fn no_traces_yields_not_observed() {
    let contract = json!({
        "properties": [{"kind": "reachability", "id": "r",
                        "goal": {"kind": "holds_all", "function": "main::t1",
                                 "resources": ["main::a"]}}]
    });
    let report = monitor(&contract, &resources(), &map_of(), &[]);
    assert_eq!(status(&report, "r"), "not_observed");
}

#[test]
fn channel_empty_fails_after_an_undrained_send() {
    let contract = json!({
        "properties": [{"kind": "safety", "id": "drained",
                        "invariant": {"kind": "channel_empty", "resource": "main::ch"}}]
    });
    let trace = r#"{"t":"t0","op":"channel_send","r":"ch"}
"#;
    let report = monitor(&contract, &resources(), &map_of(), &[parse_trace(trace)]);
    assert_eq!(status(&report, "drained"), "FAIL");
}

#[test]
fn hand_written_mapping_resolves_a_renamed_resource() {
    let contract = json!({
        "properties": [{"kind": "safety", "id": "excl",
                        "invariant": {"kind": "mutex_exclusive", "resource": "main::a"}}]
    });
    let mut overrides = BTreeMap::new();
    overrides.insert("mtx_a".to_string(), "main::a".to_string());
    let trace = r#"{"t":"t0","op":"mutex_lock","r":"mtx_a"}
"#;
    let report = monitor(&contract, &["mtx_a".to_string()], &overrides, &[parse_trace(trace)]);
    assert_eq!(status(&report, "excl"), "PASS_bounded");
    assert_eq!(report.resource_map.get("mtx_a"), Some(&Some("main::a".to_string())));
}
