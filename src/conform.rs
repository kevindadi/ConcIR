//! Trace conformance: replay a `cir_trace` event stream against the reference
//! interpreter and check that every observed CIR concurrency statement is a
//! step the model could actually take at that point.
//!
//! Observable statements are the concurrency operations that `codegen`
//! instruments with `cir_trace::ev(tag, sid)`; only their `Statement` phase is
//! matched to an event. Everything else (control flow, data statements, and the
//! internal `Register`/`Rendezvous`/`Reacquire`/`Wake`/`Complete` phases) may be
//! taken silently while searching for the next event.
//!
//! A conformant trace proves: every observed execution of the generated code is
//! an execution of the verified model. It does not prove the code is correct.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::Serialize;

use crate::codegen::{child_tags, event_at_attempt, is_observable};
use crate::interp::exec::Interpreter;
use crate::interp::state::MachineState;
use crate::sem::ids::ThreadId;
use crate::sem::outcome::{AnalysisBounds, Phase};
use crate::sem::program::{SemOp, SemProgram};
use crate::sem::system::TransitionSystem;

const MAX_VISITED: usize = 200_000;

#[derive(Debug, Clone, Serialize)]
pub struct Coverage {
    pub sids_seen: usize,
    pub sids_total: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Conformance {
    pub status: String,
    pub events: usize,
    pub event_index: Option<usize>,
    pub expected: Vec<String>,
    pub got: Option<String>,
    pub coverage: Coverage,
    pub detail: Option<String>,
    /// In `--op-resource` mode: main-thread events after all spawned threads have
    /// joined (e.g. reading state to print the terminal line). Not a violation.
    pub post_join_main_ops: Vec<String>,
    /// Channel operations observed through a wrapper mutex (ignored as events).
    pub wrapper_ops: Vec<String>,
}

fn op_of(
    program: &SemProgram,
    function: crate::sem::ids::FunctionId,
    idx: usize,
) -> Option<&SemOp> {
    program.function(function).body.get(idx).map(|s| &s.op)
}

fn sid_of(
    program: &SemProgram,
    function: crate::sem::ids::FunctionId,
    idx: usize,
) -> Option<String> {
    program
        .function(function)
        .body
        .get(idx)
        .map(|s| s.sid.clone())
}

/// All observable `module::sid` strings in the program (for coverage).
fn observable_sids(program: &SemProgram) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for f in program.functions() {
        for s in &f.body {
            if is_observable(&s.op) {
                out.insert(format!("{}::{}::{}", program.module_name(f.module), f.name, s.sid));
            }
        }
    }
    out
}

fn thread_runnable(state: &MachineState, tid: ThreadId) -> bool {
    matches!(
        state.threads.get(&tid).map(|x| &x.status),
        Some(crate::interp::state::ThreadStatus::Runnable)
    )
}

fn thread_advanced(state: &MachineState, step: &crate::sem::system::Step<MachineState>) -> bool {
    let Some(tid) = step.label.thread else {
        return false;
    };
    match (state.pc_of(tid), step.state.pc_of(tid)) {
        (Some(before), Some(after)) => after > before,
        (Some(_), None) => true,
        _ => false,
    }
}

/// True when this step is the model's rendering of an observable event.
fn event_capable(
    program: &SemProgram,
    state: &MachineState,
    step: &crate::sem::system::Step<MachineState>,
    attempt_all: bool,
    silent_spawn: bool,
) -> bool {
    let Some(idx) = step.label.origin.sid else {
        return false;
    };
    let Some(op) = op_of(program, step.label.origin.function, idx) else {
        return false;
    };
    if !is_observable(op) || !matches!(step.label.origin.phase, Phase::Statement) {
        return false;
    }
    if silent_spawn
        && matches!(op, SemOp::Spawn { .. } | SemOp::Scope { .. } | SemOp::Join { .. })
    {
        return false;
    }
    if attempt_all {
        return step
            .label
            .thread
            .map(|t| thread_runnable(state, t))
            .unwrap_or(false);
    }
    if event_at_attempt(op) {
        step.label
            .thread
            .map(|t| thread_runnable(state, t))
            .unwrap_or(false)
    } else {
        thread_advanced(state, step)
    }
}

/// In extraction mode the annotated Rust has no explicit unlock event (Rust
/// guards drop implicitly), so a model `mutex_unlock` statement may be taken
/// silently. Codegen mode never sets this, so the strict rule still applies.
fn lenient_skippable(
    program: &SemProgram,
    state: &MachineState,
    step: &crate::sem::system::Step<MachineState>,
    attempt_all: bool,
    silent_spawn: bool,
) -> bool {
    let Some(idx) = step.label.origin.sid else {
        return false;
    };
    let Some(op) = op_of(program, step.label.origin.function, idx) else {
        return false;
    };
    matches!(step.label.origin.phase, Phase::Statement)
        && matches!(op, SemOp::MutexUnlock { .. })
        && event_capable(program, state, step, attempt_all, silent_spawn)
}

fn new_tags_for_step(
    program: &SemProgram,
    before: &MachineState,
    step: &crate::sem::system::Step<MachineState>,
) -> Vec<(String, ThreadId)> {
    let Some(idx) = step.label.origin.sid else {
        return Vec::new();
    };
    let f = program.function(step.label.origin.function);
    let Some(sid) = f.body.get(idx).map(|s| s.sid.clone()) else {
        return Vec::new();
    };
    let tags = child_tags(f, &sid);
    if tags.is_empty() {
        return Vec::new();
    }
    let before_ids: BTreeSet<ThreadId> = before.threads.keys().copied().collect();
    let after_ids: BTreeSet<ThreadId> = step.state.threads.keys().copied().collect();
    after_ids
        .difference(&before_ids)
        .copied()
        .enumerate()
        .filter_map(|(i, tid)| tags.get(i).cloned().map(|t| (t, tid)))
        .collect()
}

/// What an event is matched against: a specific `sid` (codegen mode) or an
/// `(op, resource)` pair (free-Rust mode, where the instrumenter's sids are
/// synthetic and cannot be compared to model sids).
#[derive(Clone, Debug)]
enum Target {
    Sid(String),
    OpRes(String, String),
}

/// Resource-name alignment for free-Rust traces: the model uses FQNs
/// (`main::m`) while the instrumented program uses a variable name (`m`).
fn resource_matches(model: &str, event: &str) -> bool {
    if event.is_empty() {
        return true;
    }
    if model == event {
        return true;
    }
    let short = |s: &str| s.rsplit("::").next().unwrap_or(s).to_string();
    short(model) == short(event)
}

fn statement_matches(
    program: &SemProgram,
    f: crate::sem::ids::FunctionId,
    idx: usize,
    target: &Target,
) -> bool {
    match target {
        Target::Sid(s) => sid_of(program, f, idx).as_deref() == Some(s.as_str()),
        Target::OpRes(op, res) => match observable_sig(program, f, idx) {
            Some((mop, mres)) => {
                mop == *op
                    && (matches!(op.as_str(), "spawn" | "scope" | "join")
                        || resource_matches(&mres, res))
            }
            None => false,
        },
    }
}

fn target_label(program: &SemProgram, f: crate::sem::ids::FunctionId, idx: usize,
                target: &Target) -> Option<String> {
    match target {
        Target::Sid(_) => sid_of(program, f, idx),
        Target::OpRes(..) => observable_sig(program, f, idx)
            .map(|(op, res)| format!("{op}:{res}")),
    }
}

/// One event step: from every state reachable by silent steps (carrying tag
/// maps), take every binding of the target completing step.
fn advance(
    program: &SemProgram,
    it: &Interpreter,
    start: &MachineState,
    start_tags: &BTreeMap<String, ThreadId>,
    tid: ThreadId,
    target: &Target,
    lenient: bool,
    attempt_all: bool,
    silent_spawn: bool,
) -> Result<Vec<(MachineState, BTreeMap<String, ThreadId>, crate::sem::ids::FunctionId)>, Vec<String>> {
    let mut closure: Vec<(MachineState, BTreeMap<String, ThreadId>)> =
        vec![(start.clone(), start_tags.clone())];
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(it.state_key(start));
    let mut qi = 0;
    while qi < closure.len() {
        if visited.len() > MAX_VISITED {
            break;
        }
        let (state, map) = closure[qi].clone();
        qi += 1;
        let enabled = match it.successors(&state) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for step in &enabled.steps {
            if event_capable(program, &state, step, attempt_all, silent_spawn)
                && !(lenient && lenient_skippable(program, &state, step, attempt_all, silent_spawn))
            {
                continue;
            }
            let mut nm = map.clone();
            for (t, id) in new_tags_for_step(program, &state, step) {
                nm.insert(t, id);
            }
            let key = it.state_key(&step.state);
            if visited.insert(key) {
                closure.push((step.state.clone(), nm));
            }
        }
    }

    let mut out: Vec<(MachineState, BTreeMap<String, ThreadId>, crate::sem::ids::FunctionId)> = Vec::new();
    let mut seen_states: HashSet<String> = HashSet::new();
    for (state, map) in &closure {
        let enabled = match it.successors(state) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for step in &enabled.steps {
            if !event_capable(program, state, step, attempt_all, silent_spawn)
                || step.label.thread != Some(tid)
            {
                continue;
            }
            let Some(idx) = step.label.origin.sid else {
                continue;
            };
            if !statement_matches(program, step.label.origin.function, idx, target) {
                continue;
            }
            let mut nm = map.clone();
            for (t, id) in new_tags_for_step(program, state, step) {
                nm.insert(t, id);
            }
            let key = it.state_key(&step.state);
            if seen_states.insert(key) {
                out.push((step.state.clone(), nm, step.label.origin.function));
            }
        }
    }
    if out.is_empty() {
        let mut expected = Vec::new();
        for (state, _) in &closure {
            if let Ok(enabled) = it.successors(state) {
                for step in &enabled.steps {
                    if event_capable(program, state, step, attempt_all, silent_spawn)
                        && step.label.thread == Some(tid)
                    {
                        if let Some(i) = step.label.origin.sid {
                            if let Some(s) = target_label(program, step.label.origin.function, i, target) {
                                if !expected.contains(&s) {
                                    expected.push(s);
                                }
                            }
                        }
                    }
                }
            }
        }
        return Err(expected);
    }
    Ok(out)
}

fn silent_expand(
    program: &SemProgram,
    it: &Interpreter,
    frontier: &[(MachineState, BTreeMap<String, ThreadId>)],
    lenient: bool,
    attempt_all: bool,
    silent_spawn: bool,
) -> Vec<(MachineState, BTreeMap<String, ThreadId>)> {
    let mut out: Vec<(MachineState, BTreeMap<String, ThreadId>)> = frontier.to_vec();
    let mut visited: HashSet<String> = HashSet::new();
    for (state, _) in frontier {
        visited.insert(it.state_key(state));
    }
    let mut qi = 0;
    while qi < out.len() {
        if visited.len() > MAX_VISITED {
            break;
        }
        let (state, map) = out[qi].clone();
        qi += 1;
        let enabled = match it.successors(&state) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for step in &enabled.steps {
            if event_capable(program, &state, step, attempt_all, silent_spawn)
                && !(lenient && lenient_skippable(program, &state, step, attempt_all, silent_spawn))
            {
                continue;
            }
            let mut nm = map.clone();
            for (t, id) in new_tags_for_step(program, &state, step) {
                nm.insert(t, id);
            }
            let key = it.state_key(&step.state);
            if visited.insert(key) {
                out.push((step.state.clone(), nm));
            }
        }
    }
    out
}

/// Runtime op name for an observable statement (matches `cir_trace` v2 events).
fn op_name(op: &SemOp) -> &'static str {
    match op {
        SemOp::MutexLock { .. } => "mutex_lock",
        SemOp::MutexUnlock { .. } => "mutex_unlock",
        SemOp::CondvarWait { .. } => "condvar_wait",
        SemOp::CondvarNotify { .. } => "condvar_notify",
        SemOp::CondvarNotifyAll { .. } => "condvar_notify_all",
        SemOp::SemaphoreAcquire { .. } => "sem_acquire",
        SemOp::SemaphoreRelease { .. } => "sem_release",
        SemOp::ChannelSend { .. } => "channel_send",
        SemOp::ChannelRecv { .. } => "channel_recv",
        SemOp::Spawn { .. } => "spawn",
        SemOp::Scope { .. } => "scope",
        SemOp::Join { .. } => "join",
        _ => "other",
    }
}

/// (op, resource) signature of an observable statement, for v2 op/resource
/// binding. Spawn uses the child function FQN as the resource.
fn observable_sig(program: &SemProgram, f: crate::sem::ids::FunctionId, idx: usize)
    -> Option<(String, String)> {
    let fun = program.function(f);
    let op = fun.body.get(idx)?.op.clone();
    if !is_observable(&op) {
        return None;
    }
    let resource = match &op {
        SemOp::MutexLock { resource } | SemOp::MutexUnlock { resource }
        | SemOp::CondvarWait { condvar: resource, .. }
        | SemOp::CondvarNotify { condvar: resource }
        | SemOp::CondvarNotifyAll { condvar: resource }
        | SemOp::SemaphoreAcquire { resource, .. }
        | SemOp::SemaphoreRelease { resource, .. }
        | SemOp::ChannelSend { channel: resource, .. }
        | SemOp::ChannelRecv { channel: resource, .. } => {
            let res = program.resource(*resource);
            crate::fqn::fqn(program.module_name(res.module), &res.name)
        }
        SemOp::Spawn { func, .. } => {
            let cf = program.function(*func);
            crate::fqn::fqn(program.module_name(cf.module), &cf.name)
        }
        _ => String::new(),
    };
    Some((op_name(&op).to_string(), resource))
}

pub fn conform(program: &SemProgram, trace: &[(String, String)]) -> Conformance {
    conform_options(program, trace, false, false)
}

/// `lenient_unlock` is the extraction-mode relaxation: the model may take a
/// `mutex_unlock` statement silently (no `cir_trace` event), matching Rust's
/// implicit guard drop. It is off for codegen-mode conformance.
pub fn conform_options(
    program: &SemProgram,
    trace: &[(String, String)],
    lenient_unlock: bool,
    attempt_events: bool,
) -> Conformance {
    let full: Vec<(String, String, String, String)> = trace
        .iter()
        .map(|(t, s)| (t.clone(), s.clone(), String::new(), String::new()))
        .collect();
    conform_events(program, &full, lenient_unlock, attempt_events, false)
}

/// v2: each event carries (tag, sid, op, resource); op/resource are checked
/// against the model statement when present. With `op_resource`, the event is
/// matched to a model statement by `(op, resource)` only; this is the mode for
/// LLM-written Rust, whose instrumented sids are synthetic.
pub fn conform_events(
    program: &SemProgram,
    trace: &[(String, String, String, String)],
    lenient_unlock: bool,
    attempt_events: bool,
    op_resource: bool,
) -> Conformance {
    let total = observable_sids(program).len();
    let mut post_join_main_ops: Vec<String> = Vec::new();
    let mut wrapper_ops: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let it = Interpreter::new(program, AnalysisBounds::default());
    let initial = match it.initial() {
        Ok(s) => s,
        Err(e) => {
            return Conformance {
                status: "error".into(),
                events: trace.len(),
                post_join_main_ops: post_join_main_ops.clone(),
                wrapper_ops: wrapper_ops.clone(),
                event_index: None,
                expected: vec![],
                got: None,
                coverage: Coverage {
                    sids_seen: 0,
                    sids_total: total,
                },
                detail: Some(format!("initial state: {e}")),
            };
        }
    };
    let mut tags0: BTreeMap<String, ThreadId> = BTreeMap::new();
    tags0.insert("t0".into(), ThreadId(0));
    let mut frontier: Vec<(MachineState, BTreeMap<String, ThreadId>)> = vec![(initial, tags0)];

    for (k, (tag, sid, op, resource)) in trace.iter().enumerate() {
        frontier = silent_expand(program, &it, &frontier, lenient_unlock, attempt_events, op_resource);
        let target = if op_resource {
            Target::OpRes(op.clone(), resource.clone())
        } else {
            Target::Sid(sid.clone())
        };
        let known = program.functions().iter().any(|f| {
            f.body
                .iter()
                .enumerate()
                .any(|(i, s)| is_observable(&s.op) && statement_matches(program, f.id, i, &target))
        });
        if known && !op.is_empty() && !op_resource {
            // v2 op/resource binding: the event's operation must match the model.
            let mut sigs: Vec<(String, String)> = Vec::new();
            for f in program.functions() {
                for (i, st) in f.body.iter().enumerate() {
                    if st.sid == *sid {
                        if let Some(sig) = observable_sig(program, f.id, i) {
                            sigs.push(sig);
                        }
                    }
                }
            }
            let op_ok = sigs.iter().any(|(o, _)| o == op);
            let res_ok = sigs.iter().any(|(o, r)| o == op && (resource.is_empty() || r == resource));
            if !op_ok || !res_ok {
                let kind = if op_ok { "resource" } else { "order" };
                return Conformance {
                    status: "violation".into(),
                    events: trace.len(),
                    post_join_main_ops: post_join_main_ops.clone(),
                    wrapper_ops: wrapper_ops.clone(),
                    event_index: Some(k),
                    expected: sigs.iter().map(|(o, r)| format!("{o}:{r}")).collect(),
                    got: Some(format!("{op}:{resource}")),
                    coverage: Coverage { sids_seen: seen.len(), sids_total: total },
                    detail: Some(format!(
                        "kind={kind}: event {op} on {resource:?} does not match sid {sid}")),
                };
            }
        }
        if !known {
            return Conformance {
                status: if op_resource { "violation".into() } else { "unknown_sid".into() },
                events: trace.len(),
                post_join_main_ops: post_join_main_ops.clone(),
                wrapper_ops: wrapper_ops.clone(),
                event_index: Some(k),
                expected: vec![],
                got: Some(if op_resource {
                    format!("{op}:{resource}")
                } else {
                    sid.clone()
                }),
                coverage: Coverage {
                    sids_seen: seen.len(),
                    sids_total: total,
                },
                detail: Some(if op_resource {
                    format!("no model statement matches {op} on {resource:?} (thread {tag})")
                } else {
                    format!("thread {tag} emitted unknown sid {sid}")
                }),
            };
        }
        if op_resource {
            // Free-Rust mode: the instrumenter's tags do not match the model's
            // child tags, so we check that the observed (op, resource) sequence
            // is a valid interleaving of the model's observable steps, across
            // threads.
            let mut next: Vec<(MachineState, BTreeMap<String, ThreadId>)> = Vec::new();
            let mut expected: Vec<String> = Vec::new();
            let mut found = false;
            for (state, map) in &frontier {
                let Ok(enabled) = it.successors(state) else { continue };
                for step in &enabled.steps {
                    if !event_capable(program, state, step, attempt_events, true) {
                        continue;
                    }
                    let Some(idx) = step.label.origin.sid else { continue };
                    if statement_matches(program, step.label.origin.function, idx, &target) {
                        found = true;
                        let mut nm = map.clone();
                        for (t, id) in new_tags_for_step(program, state, step) {
                            nm.insert(t, id);
                        }
                        next.push((step.state.clone(), nm));
                    } else if let Some(lbl) =
                        target_label(program, step.label.origin.function, idx, &target)
                    {
                        if !expected.contains(&lbl) {
                            expected.push(lbl);
                        }
                    }
                }
            }
            if !found {
                // Post-join main-thread work (reading state to print the terminal
                // line) is allowed once every spawned thread has finished; keep
                // only the finished-children frontier and record the op.
                let main_only: Vec<(MachineState, BTreeMap<String, ThreadId>)> = frontier
                    .iter()
                    .filter(|(st, _)| st.threads.len() <= 1)
                    .cloned()
                    .collect();
                if !main_only.is_empty() {
                    post_join_main_ops.push(format!("{op}:{resource}"));
                    frontier = main_only;
                    continue;
                }
                return Conformance {
                    status: "violation".into(),
                    events: trace.len(),
                    post_join_main_ops: post_join_main_ops.clone(),
                    wrapper_ops: wrapper_ops.clone(),
                    event_index: Some(k),
                    expected: expected.into_iter().take(6).collect(),
                    got: Some(format!("{op}:{resource}")),
                    coverage: Coverage { sids_seen: seen.len(), sids_total: total },
                    detail: Some(format!(
                        "no enabled model step matches {op} on {resource:?} at event {k}")),
                };
            }
            let mut seen_states: HashSet<String> = HashSet::new();
            frontier = next
                .into_iter()
                .filter(|(st, _)| seen_states.insert(it.state_key(st)))
                .collect();
            continue;
        }
        let mut next: Vec<(MachineState, BTreeMap<String, ThreadId>)> = Vec::new();
        let mut matched_functions: BTreeSet<crate::sem::ids::FunctionId> = BTreeSet::new();
        let mut expected: Vec<String> = Vec::new();
        let mut tag_known = false;
        for (state, map) in &frontier {
            let Some(tid) = map.get(tag).copied() else {
                continue;
            };
            tag_known = true;
            match advance(program, &it, state, map, tid, &target, lenient_unlock, attempt_events, op_resource) {
                Ok(results) => {
                    for (s, m, f) in results {
                        matched_functions.insert(f);
                        next.push((s, m));
                    }
                }
                Err(exp) => {
                    for e in exp {
                        if !expected.contains(&e) {
                            expected.push(e);
                        }
                    }
                }
            }
        }
        if !tag_known {
            return Conformance {
                status: "violation".into(),
                events: trace.len(),
                post_join_main_ops: post_join_main_ops.clone(),
                wrapper_ops: wrapper_ops.clone(),
                event_index: Some(k),
                expected: vec![],
                got: Some(sid.clone()),
                coverage: Coverage {
                    sids_seen: seen.len(),
                    sids_total: total,
                },
                detail: Some(format!(
                    "event for unknown thread tag {tag} before it was spawned"
                )),
            };
        }
        if next.is_empty() {
            let dump = if std::env::var("CONFORM_DEBUG").is_ok() {
                frontier
                    .first()
                    .map(|(st, _)| it.canonical(st))
                    .unwrap_or_default()
                    .chars()
                    .take(1500)
                    .collect::<String>()
            } else {
                String::new()
            };
            return Conformance {
                status: "violation".into(),
                events: trace.len(),
                post_join_main_ops: post_join_main_ops.clone(),
                wrapper_ops: wrapper_ops.clone(),
                event_index: Some(k),
                expected,
                got: Some(sid.clone()),
                coverage: Coverage {
                    sids_seen: seen.len(),
                    sids_total: total,
                },
                detail: Some(format!(
                    "no model path matches event {k} ({tag} {sid}){dump}"
                )),
            };
        }
        let mut seen_states: HashSet<String> = HashSet::new();
        frontier = next
            .into_iter()
            .filter(|(st, _)| seen_states.insert(it.state_key(st)))
            .collect();
        if !op_resource {
            for function in &matched_functions {
                let f = program.function(*function);
                seen.insert(format!("{}::{}::{}", program.module_name(f.module), f.name, sid));
            }
        }
    }
    Conformance {
        status: "conformant".into(),
        events: trace.len(),
        post_join_main_ops: post_join_main_ops.clone(),
        wrapper_ops: wrapper_ops.clone(),
        event_index: None,
        expected: vec![],
        got: None,
        coverage: Coverage {
            sids_seen: seen.len(),
            sids_total: total,
        },
        detail: None,
    }
}
