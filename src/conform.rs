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
                out.insert(format!("{}::{}", program.module_name(f.module), s.sid));
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
    if event_at_attempt(op) {
        step.label
            .thread
            .map(|t| thread_runnable(state, t))
            .unwrap_or(false)
    } else {
        thread_advanced(state, step)
    }
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

/// One event step: from every state reachable by silent steps (carrying tag
/// maps), take every binding of the target `(tid, sid)` completing step.
fn advance(
    program: &SemProgram,
    it: &Interpreter,
    start: &MachineState,
    start_tags: &BTreeMap<String, ThreadId>,
    tid: ThreadId,
    sid: &str,
) -> Result<Vec<(MachineState, BTreeMap<String, ThreadId>)>, Vec<String>> {
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
            if event_capable(program, &state, step) {
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

    let mut out: Vec<(MachineState, BTreeMap<String, ThreadId>)> = Vec::new();
    let mut seen_states: HashSet<String> = HashSet::new();
    for (state, map) in &closure {
        let enabled = match it.successors(state) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for step in &enabled.steps {
            if !event_capable(program, state, step) || step.label.thread != Some(tid) {
                continue;
            }
            let Some(idx) = step.label.origin.sid else {
                continue;
            };
            if sid_of(program, step.label.origin.function, idx).as_deref() != Some(sid) {
                continue;
            }
            let mut nm = map.clone();
            for (t, id) in new_tags_for_step(program, state, step) {
                nm.insert(t, id);
            }
            let key = it.state_key(&step.state);
            if seen_states.insert(key) {
                out.push((step.state.clone(), nm));
            }
        }
    }
    if out.is_empty() {
        let mut expected = Vec::new();
        for (state, _) in &closure {
            if let Ok(enabled) = it.successors(state) {
                for step in &enabled.steps {
                    if event_capable(program, state, step) && step.label.thread == Some(tid) {
                        if let Some(i) = step.label.origin.sid {
                            if let Some(s) = sid_of(program, step.label.origin.function, i) {
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
            if event_capable(program, &state, step) {
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

pub fn conform(program: &SemProgram, trace: &[(String, String)]) -> Conformance {
    let total = observable_sids(program).len();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let it = Interpreter::new(program, AnalysisBounds::default());
    let initial = match it.initial() {
        Ok(s) => s,
        Err(e) => {
            return Conformance {
                status: "error".into(),
                events: trace.len(),
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

    for (k, (tag, sid)) in trace.iter().enumerate() {
        frontier = silent_expand(program, &it, &frontier);
        let known = program
            .functions()
            .iter()
            .any(|f| f.body.iter().any(|s| is_observable(&s.op) && s.sid == *sid));
        if !known {
            return Conformance {
                status: "unknown_sid".into(),
                events: trace.len(),
                event_index: Some(k),
                expected: vec![],
                got: Some(sid.clone()),
                coverage: Coverage {
                    sids_seen: seen.len(),
                    sids_total: total,
                },
                detail: Some(format!("thread {tag} emitted unknown sid {sid}")),
            };
        }
        let mut next: Vec<(MachineState, BTreeMap<String, ThreadId>)> = Vec::new();
        let mut expected: Vec<String> = Vec::new();
        let mut tag_known = false;
        for (state, map) in &frontier {
            let Some(tid) = map.get(tag).copied() else {
                continue;
            };
            tag_known = true;
            match advance(program, &it, state, map, tid, sid) {
                Ok(results) => next.extend(results),
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
        for s in observable_sids(program) {
            if s.ends_with(&format!("::{sid}")) {
                seen.insert(s);
            }
        }
    }
    Conformance {
        status: "conformant".into(),
        events: trace.len(),
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
