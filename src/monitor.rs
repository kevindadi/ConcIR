//! Bounded, model-free requirement monitoring over `cir_trace` event streams.
//!
//! `concir-backend monitor` takes a contract, a `resources.json` (rust resource
//! names and kinds), a directory of traces (one JSON-lines event stream per
//! run) and, optionally, a hand-written `mapping.json`. It re-derives a small
//! machine state from each trace (per-thread held resources and channel
//! occupancy) and checks the contract's predicates against the *observed*
//! states only.
//!
//! This is deliberately weaker than `explore`:
//!   * `explore` decides verdicts exhaustively over the model;
//!   * `monitor` observes a finite set of runs and can only ever report
//!     `PASS_bounded`. A property that no run exercises is `not_observed`, a
//!     property over a resource that cannot be aligned is `unmapped`, and a
//!     predicate the trace stream cannot carry is `unsupported`.
//!
//! Required-to-hold predicates (`safety`, `never_holds_all`, `unreachable`)
//! are checked on every observed state; reached-predicates (`reachability`,
//! `always_reachable`, preserved `reachable`) need one observation. Spawned
//! children are counted complete when the trace records their `join` (or an
//! explicit `complete`), because a finished trace implies `main` returned.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct TraceEvent {
    pub tag: String,
    pub op: String,
    pub resource: String,
    pub sid: String,
}

#[derive(Debug, Clone, Default)]
struct MachineState {
    /// contract resource FQN -> set of thread tags currently holding it.
    held: BTreeMap<String, BTreeSet<String>>,
    /// contract resource FQN -> observed channel occupancy (sends - recvs).
    chan: BTreeMap<String, i64>,
    /// every mapped resource that appeared anywhere in the trace.
    seen: BTreeSet<String>,
    completed: BTreeSet<String>,
}

impl MachineState {
    fn holders(&self, resource: &str) -> usize {
        self.held.get(resource).map(|s| s.len()).unwrap_or(0)
    }

    fn thread_holds_all(&self, tag: &str, resources: &[String]) -> bool {
        resources
            .iter()
            .all(|r| self.held.get(r).map(|s| s.contains(tag)).unwrap_or(false))
    }

    fn any_thread_holds_all(&self, resources: &[String]) -> bool {
        self.held
            .iter()
            .filter(|(_, tags)| !tags.is_empty())
            .flat_map(|(_, tags)| tags.iter())
            .any(|tag| self.thread_holds_all(tag, resources))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PropertyResult {
    pub id: String,
    pub kind: String,
    pub source: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub req: Vec<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MonitorReport {
    pub status: String,
    pub bounded: bool,
    pub traces: usize,
    pub events: usize,
    pub resource_map: BTreeMap<String, Option<String>>,
    pub unmapped_resources: Vec<String>,
    pub properties: Vec<PropertyResult>,
}

enum Eval {
    /// Predicate decided on this state.
    Bool(bool),
    /// The trace stream cannot decide this predicate.
    Unsupported(String),
    /// The predicate needs a resource that could not be aligned.
    Unmapped(String),
}

// ─────────────────────────── resource alignment ───────────────────────────

/// Collect every contract resource FQN referenced by the contract predicates.
fn contract_resources(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(r)) = map.get("resource") {
                out.insert(r.clone());
            }
            if let Some(Value::String(f)) = map.get("function") {
                out.insert(f.clone());
            }
            if let Some(Value::Array(items)) = map.get("resources") {
                for item in items {
                    if let Some(s) = item.as_str() {
                        out.insert(s.to_string());
                    }
                }
            }
            for v in map.values() {
                contract_resources(v, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                contract_resources(item, out);
            }
        }
        _ => {}
    }
}

/// Align rust resource names to contract FQNs. Exact identity first, then
/// `main::<name>`, then a `::`-suffix match. A hand-written override map wins.
fn align_resources(
    rust_names: &[String],
    contract: &BTreeSet<String>,
    overrides: &BTreeMap<String, String>,
) -> BTreeMap<String, Option<String>> {
    let mut map = BTreeMap::new();
    for name in rust_names {
        if let Some(target) = overrides.get(name) {
            map.insert(name.clone(), contract.contains(target).then(|| target.clone()));
            continue;
        }
        let mut found = None;
        if contract.contains(name) {
            found = Some(name.clone());
        }
        if found.is_none() {
            let local = format!("main::{name}");
            if contract.contains(&local) {
                found = Some(local);
            }
        }
        if found.is_none() {
            let mut suffix: Vec<String> = contract
                .iter()
                .filter(|fqn| fqn.rsplit("::").next() == Some(name.as_str()))
                .cloned()
                .collect();
            suffix.sort();
            found = suffix.into_iter().next();
        }
        map.insert(name.clone(), found);
    }
    map
}

// ─────────────────────────── predicate evaluation ───────────────────────────

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}

fn number_field(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| x.as_f64())
}

fn map_name(map: &BTreeMap<String, Option<String>>, rust: &str) -> Option<String> {
    map.get(rust).cloned().flatten()
}

fn eval_predicate(
    pred: &Value,
    state: &MachineState,
    resource_map: &BTreeMap<String, Option<String>>,
) -> Eval {
    let kind = match str_field(pred, "kind") {
        Some(k) => k,
        None => return Eval::Unsupported("predicate without kind".into()),
    };
    match kind {
        "true" => Eval::Bool(true),
        "false" => Eval::Bool(false),
        "not" => match pred.get("predicate").or_else(|| pred.get("goal")) {
            Some(inner) => match eval_predicate(inner, state, resource_map) {
                Eval::Bool(b) => Eval::Bool(!b),
                other => other,
            },
            None => Eval::Unsupported("not without operand".into()),
        },
        "and" | "or" => {
            let key = if kind == "and" { "predicates" } else { "predicates" };
            let items = pred
                .get(key)
                .or_else(|| pred.get("goals"))
                .and_then(|x| x.as_array());
            let Some(items) = items else {
                return Eval::Unsupported(format!("{kind} without predicate list"));
            };
            let mut acc = kind == "and";
            for item in items {
                match eval_predicate(item, state, resource_map) {
                    Eval::Bool(b) => {
                        if kind == "and" {
                            acc &= b;
                        } else {
                            acc |= b;
                        }
                    }
                    other => return other,
                }
            }
            Eval::Bool(acc)
        }
        "holds_all" => {
            let resources: Vec<String> = pred
                .get("resources")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            if resources.is_empty() {
                return Eval::Unsupported("holds_all without resources".into());
            }
            for r in &resources {
                if !resource_seen(state, r) {
                    return Eval::Unmapped(format!("resource {r} not observed"));
                }
            }
            Eval::Bool(state.any_thread_holds_all(&resources))
        }
        "never_holds_all" => {
            let resources: Vec<String> = pred
                .get("resources")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            if resources.is_empty() {
                return Eval::Unsupported("never_holds_all without resources".into());
            }
            for r in &resources {
                if !resource_seen(state, r) {
                    return Eval::Unmapped(format!("resource {r} not observed"));
                }
            }
            Eval::Bool(!state.any_thread_holds_all(&resources))
        }
        "mutex_exclusive" => match str_field(pred, "resource") {
            Some(r) => {
                let r = r.to_string();
                if !resource_seen(state, &r) {
                    Eval::Unmapped(format!("resource {r} not observed"))
                } else {
                    Eval::Bool(state.holders(&r) <= 1)
                }
            }
            None => Eval::Unsupported("mutex_exclusive without resource".into()),
        },
        "mutex_free" => match str_field(pred, "resource") {
            Some(r) => {
                let r = r.to_string();
                if !resource_seen(state, &r) {
                    Eval::Unmapped(format!("resource {r} not observed"))
                } else {
                    Eval::Bool(state.holders(&r) == 0)
                }
            }
            None => Eval::Unsupported("mutex_free without resource".into()),
        },
        "mutex_held" => match str_field(pred, "resource") {
            Some(r) => {
                let r = r.to_string();
                if !resource_seen(state, &r) {
                    Eval::Unmapped(format!("resource {r} not observed"))
                } else {
                    Eval::Bool(state.holders(&r) >= 1)
                }
            }
            None => Eval::Unsupported("mutex_held without resource".into()),
        },
        "channel_empty" => match str_field(pred, "resource") {
            Some(r) => {
                let r = r.to_string();
                if !state.chan.contains_key(&r) {
                    Eval::Unmapped(format!("channel {r} not observed"))
                } else {
                    Eval::Bool(state.chan.get(&r).copied().unwrap_or(0).max(0) == 0)
                }
            }
            None => Eval::Unsupported("channel_empty without resource".into()),
        },
        "channel_at_least" => match str_field(pred, "resource") {
            Some(r) => {
                let r = r.to_string();
                let n = number_field(pred, "n").unwrap_or(0.0);
                if !state.chan.contains_key(&r) {
                    Eval::Unmapped(format!("channel {r} not observed"))
                } else {
                    Eval::Bool(state.chan.get(&r).copied().unwrap_or(0) as f64 >= n)
                }
            }
            None => Eval::Unsupported("channel_at_least without resource".into()),
        },
        "function_completed" => match str_field(pred, "function") {
            Some(f) => {
                if f == "main::main" || state.completed.contains(f) {
                    Eval::Bool(true)
                } else {
                    let suffix = f.rsplit("::").next().unwrap_or(f);
                    Eval::Bool(state.completed.contains(suffix))
                }
            }
            None => Eval::Unsupported("function_completed without function".into()),
        },
        "var_eq" | "var_cmp" | "var_ref" => {
            Eval::Unsupported(format!("{kind} needs value events not present in the stream"))
        }
        other => Eval::Unsupported(format!("predicate kind {other} is not supported")),
    }
}

fn resource_seen(state: &MachineState, resource: &str) -> bool {
    state.seen.contains(resource) || state.chan.contains_key(resource)
}

// ─────────────────────────── replay + property checks ───────────────────────────

fn replay(events: &[TraceEvent], map: &BTreeMap<String, Option<String>>) -> Vec<MachineState> {
    let mut state = MachineState::default();
    let mut states = vec![state.clone()];
    for ev in events {
        let mapped = map_name(map, &ev.resource);
        if let Some(name) = &mapped {
            state.seen.insert(name.clone());
        }
        match ev.op.as_str() {
            "mutex_lock" | "sem_acquire" => {
                if let Some(name) = mapped {
                    state.held.entry(name).or_default().insert(ev.tag.clone());
                }
            }
            "mutex_unlock" | "sem_release" => {
                if let Some(name) = mapped {
                    if let Some(tags) = state.held.get_mut(&name) {
                        tags.remove(&ev.tag);
                    }
                }
            }
            "channel_send" => {
                if let Some(name) = mapped {
                    *state.chan.entry(name).or_insert(0) += 1;
                }
            }
            "channel_recv" => {
                if let Some(name) = mapped {
                    *state.chan.entry(name).or_insert(0) -= 1;
                }
            }
            // A finished trace implies `main` returned, so a joined (or still
            // joined) worker has completed. Spawn/join/complete are bound by
            // the runtime resource name; the harness maps them onto the
            // contract's function FQNs.
            "spawn" | "join" | "complete" => {
                if let Some(name) = mapped {
                    state.completed.insert(name);
                }
                if !ev.sid.is_empty() {
                    state.completed.insert(ev.sid.clone());
                }
            }
            _ => {}
        }
        states.push(state.clone());
    }
    states
}

fn predicate_status(
    pred: &Value,
    traces: &[Vec<MachineState>],
    map: &BTreeMap<String, Option<String>>,
    mode: Mode,
) -> (String, Option<String>) {
    // mode: ForAll (must hold in every state of every trace) or Exists
    // (observed in at least one state).
    if traces.is_empty() {
        return ("not_observed".into(), Some("no traces".into()));
    }
    let mut saw_unsupported: Option<String> = None;
    let mut saw_unmapped: Option<String> = None;
    let mut any_true = false;
    for trace in traces {
        for state in trace {
            match eval_predicate(pred, state, map) {
                Eval::Bool(true) => any_true = true,
                Eval::Bool(false) => match mode {
                    Mode::ForAll => return ("FAIL".into(), Some("predicate violated".into())),
                    Mode::Exists => {}
                },
                Eval::Unsupported(msg) => {
                    if saw_unsupported.is_none() {
                        saw_unsupported = Some(msg);
                    }
                }
                Eval::Unmapped(msg) => {
                    if saw_unmapped.is_none() {
                        saw_unmapped = Some(msg);
                    }
                }
            }
        }
    }
    if saw_unmapped.is_some() && !any_true {
        return ("unmapped".into(), saw_unmapped);
    }
    if saw_unsupported.is_some() && !any_true {
        return ("unsupported".into(), saw_unsupported);
    }
    match mode {
        Mode::ForAll => ("PASS_bounded".into(), None),
        Mode::Exists => {
            if any_true {
                ("PASS_bounded".into(), None)
            } else {
                ("not_observed".into(), None)
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    ForAll,
    Exists,
}

fn result_for(
    clause: &Value,
    source: &str,
    traces: &[Vec<MachineState>],
    map: &BTreeMap<String, Option<String>>,
) -> PropertyResult {
    let kind = str_field(clause, "kind").unwrap_or("unknown").to_string();
    let id = clause
        .get("id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| {
            clause
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| kind.clone())
        });
    let req = clause
        .get("req")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let (status, detail) = match kind.as_str() {
        "deadlock_free" => (
            "deferred".to_string(),
            Some("decided by behavior (hang/timeout) and Miri".to_string()),
        ),
        "safety" => match clause.get("invariant") {
            Some(inv) => predicate_status(inv, traces, map, Mode::ForAll),
            None => ("unsupported".into(), Some("safety without invariant".into())),
        },
        "unreachable" => match clause.get("bad") {
            Some(bad) => match predicate_status(bad, traces, map, Mode::ForAll) {
                (s, _) if s == "FAIL" => (
                    "FAIL".to_string(),
                    Some("a bad state was observed".to_string()),
                ),
                (s, d) => (s, d),
            },
            None => ("unsupported".into(), Some("unreachable without bad".into())),
        },
        "reachability" | "always_reachable" => match clause.get("goal") {
            Some(goal) => predicate_status(goal, traces, map, Mode::Exists),
            None => ("unsupported".into(), Some("property without goal".into())),
        },
        "reachable" => match clause.get("goal") {
            Some(goal) => predicate_status(goal, traces, map, Mode::Exists),
            None => ("unsupported".into(), Some("reachable without goal".into())),
        },
        "always" => match clause.get("invariant") {
            Some(inv) => predicate_status(inv, traces, map, Mode::ForAll),
            None => ("unsupported".into(), Some("always without invariant".into())),
        },
        other => ("unsupported".into(), Some(format!("property kind {other}"))),
    };
    PropertyResult {
        id,
        kind,
        source: source.to_string(),
        req,
        status,
        detail,
    }
}

pub fn parse_trace(text: &str) -> Vec<TraceEvent> {
    let mut events = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        events.push(TraceEvent {
            tag: v.get("t").and_then(|x| x.as_str()).unwrap_or("t0").to_string(),
            op: v.get("op").and_then(|x| x.as_str()).unwrap_or("ev").to_string(),
            resource: v.get("r").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            sid: v.get("sid").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        });
    }
    events
}

/// Load every `*.jsonl` / `*.trace` / `*.txt` file in a trace directory (or a
/// single trace file) into per-run event streams, sorted by file name.
pub fn load_traces(dir: &Path) -> Result<Vec<Vec<TraceEvent>>, String> {
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    if dir.is_file() {
        files.push(dir.to_path_buf());
    } else {
        let entries = fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let ok = path.is_file()
                && matches!(path.extension().and_then(|e| e.to_str()),
                            Some("jsonl") | Some("trace") | Some("txt"));
            if ok {
                files.push(path);
            }
        }
    }
    files.sort();
    let mut traces = Vec::new();
    for path in files {
        let text = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        traces.push(parse_trace(&text));
    }
    Ok(traces)
}

/// Rust resource names declared in `resources.json`: `{"resources":[{"name":...,
/// "kind":...}]}` or a bare `{"resources":["m", ...]}`.
pub fn load_resources(value: &Value) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(items) = value.get("resources").and_then(|x| x.as_array()) {
        for item in items {
            if let Some(s) = item.as_str() {
                names.push(s.to_string());
            } else if let Some(s) = item.get("name").and_then(|x| x.as_str()) {
                names.push(s.to_string());
            }
        }
    }
    names
}

pub fn load_overrides(value: &Value) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Some(obj) = value.get("mapping").and_then(|x| x.as_object()) {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                map.insert(k.clone(), s.to_string());
            }
        }
    } else if let Some(obj) = value.as_object() {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                map.insert(k.clone(), s.to_string());
            }
        }
    }
    map
}

/// Derive the rust resource universe from the traces when no `resources.json`
/// is available (bounded fallback).
pub fn resources_from_traces(traces: &[Vec<TraceEvent>]) -> Vec<String> {
    let mut names = BTreeSet::new();
    for trace in traces {
        for ev in trace {
            if !ev.resource.is_empty() && ev.resource != ev.sid {
                names.insert(ev.resource.clone());
            }
        }
    }
    names.into_iter().collect()
}

pub fn monitor(
    contract: &Value,
    rust_names: &[String],
    overrides: &BTreeMap<String, String>,
    traces: &[Vec<TraceEvent>],
) -> MonitorReport {
    let mut contract_set = BTreeSet::new();
    contract_resources(contract, &mut contract_set);
    let resource_map = align_resources(rust_names, &contract_set, overrides);
    let unmapped: Vec<String> = resource_map
        .iter()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.clone())
        .collect();

    let replayed: Vec<Vec<MachineState>> =
        traces.iter().map(|t| replay(t, &resource_map)).collect();

    let mut properties = Vec::new();
    if let Some(props) = contract.get("properties").and_then(|x| x.as_array()) {
        for clause in props {
            properties.push(result_for(clause, "properties", &replayed, &resource_map));
        }
    }
    if let Some(pres) = contract.get("preserved").and_then(|x| x.as_array()) {
        for clause in pres {
            properties.push(result_for(clause, "preserved", &replayed, &resource_map));
        }
    }

    let events: usize = traces.iter().map(|t| t.len()).sum();
    let has_fail = properties.iter().any(|p| p.status == "FAIL");
    MonitorReport {
        status: if has_fail { "fail".into() } else { "ok".into() },
        bounded: true,
        traces: traces.len(),
        events,
        resource_map,
        unmapped_resources: unmapped,
        properties,
    }
}
