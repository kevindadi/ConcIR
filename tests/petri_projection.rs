//! Differential check: the reference interpreter and the Petri-net executor
//! must expose the same CIR-observable semantics on small bounded programs.
//!
//! Auxiliary net steps are projected away by comparing a *semantic projection*
//! of the complete state (shared data, mutex ownership, wait queues, thread
//! control positions, locals, completion facts), not raw token placements.

use std::collections::HashSet;

use concir::ast::Program;
use concir::explore;
use concir::interp::state::{BlockReason, MachineState, MutexState as ItMutex, ThreadStatus};
use concir::interp::Interpreter;
use concir::petri::exec::PetriEngine;
use concir::petri::net::{MutexToken, NetState, PlaceKey};
use concir::sem::outcome::AnalysisBounds;
use concir::sem::program::{self, ResKind, SemProgram};
use concir::sem::value::Value;

fn lower(src: &str) -> SemProgram {
    let p: Program = serde_json::from_str(src).unwrap();
    program::lower(&p).unwrap()
}

fn bounds() -> AnalysisBounds {
    AnalysisBounds {
        max_threads: 8,
        max_frames_per_thread: 8,
        max_states: 100_000,
        max_depth: 200,
        max_boundary_events: 256,
    }
}

// ── interpreter projection ──────────────────────────────────────────

fn project_it(sp: &SemProgram, s: &MachineState) -> String {
    let mut out = String::new();
    out.push_str(&shared_it(sp, s));
    for f in s.store.frames.values() {
        out.push_str(&frame_line(f.id.0, f.function.0, f.pc, &f.locals));
    }
    for (tid, t) in &s.threads {
        let stack: Vec<String> = t
            .stack
            .iter()
            .map(|f| {
                let fr = &s.store.frames[f];
                format!("{}:{}", fr.function.0, fr.pc)
            })
            .collect();
        let status = match &t.status {
            ThreadStatus::Runnable => "run".to_string(),
            ThreadStatus::Finished => "done".to_string(),
            ThreadStatus::Blocked(b) => format!("blocked:{}", block_kind_it(b)),
        };
        out.push_str(&format!(
            "thread {} entry={} stack=[{}] {}\n",
            tid.0, t.entry_function.0, stack.join(","), status
        ));
    }
    out.push_str(&format!(
        "completed={:?} reached={:?}\n",
        s.completed_functions, s.reached
    ));
    out
}

fn shared_it(sp: &SemProgram, s: &MachineState) -> String {
    let mut out = String::new();
    for r in sp.resources() {
        match r.kind {
            ResKind::Var => {
                if let Some(v) = s.store.vars.get(&r.id) {
                    out.push_str(&format!("var r{}={}\n", r.id.0, v.canonical()));
                }
            }
            ResKind::Atomic => {
                if let Some(v) = s.store.atomics.get(&r.id) {
                    out.push_str(&format!("atomic r{}={}\n", r.id.0, v.canonical()));
                }
            }
            ResKind::Mutex => {
                let m = s
                    .store
                    .mutexes
                    .get(&r.id)
                    .map(|m| match m {
                        ItMutex::Free => "free".to_string(),
                        ItMutex::Held(t) => format!("held:{}", t.0),
                    })
                    .unwrap_or_default();
                out.push_str(&format!("mutex r{}={}\n", r.id.0, m));
                out.push_str(&format!(
                    "lockq r{}={:?}\n",
                    r.id.0,
                    s.mutex_waiters
                        .get(&r.id)
                        .map(|q| q.iter().map(|t| t.0).collect::<Vec<_>>())
                        .unwrap_or_default()
                ));
            }
            ResKind::Semaphore => {
                out.push_str(&format!(
                    "sem r{}={}\n",
                    r.id.0,
                    s.store.semaphores.get(&r.id).copied().unwrap_or(0)
                ));
                out.push_str(&format!(
                    "semq r{}={:?}\n",
                    r.id.0,
                    s.sem_waiters
                        .get(&r.id)
                        .map(|q| q.iter().map(|(t, n)| (t.0, *n)).collect::<Vec<_>>())
                        .unwrap_or_default()
                ));
            }
            ResKind::Channel => {
                out.push_str(&format!(
                    "chan r{} buf={:?}\n",
                    r.id.0,
                    s.store
                        .channels
                        .get(&r.id)
                        .map(|c| c.buffer.iter().map(Value::canonical).collect::<Vec<_>>())
                        .unwrap_or_default()
                ));
                out.push_str(&format!(
                    "sendq r{}={:?}\n",
                    r.id.0,
                    s.store
                        .channels
                        .get(&r.id)
                        .map(|c| c.pending_send.iter().map(|p| p.thread.0).collect::<Vec<_>>())
                        .unwrap_or_default()
                ));
                out.push_str(&format!(
                    "recvq r{}={:?}\n",
                    r.id.0,
                    s.store
                        .channels
                        .get(&r.id)
                        .map(|c| c.pending_recv.iter().map(|t| t.0).collect::<Vec<_>>())
                        .unwrap_or_default()
                ));
            }
            ResKind::Condvar => {
                out.push_str(&format!(
                    "condvar r{} waiters={:?}\n",
                    r.id.0,
                    s.store
                        .condvars
                        .get(&r.id)
                        .map(|c| c.waiters.iter().map(|t| t.0).collect::<Vec<_>>())
                        .unwrap_or_default()
                ));
            }
            ResKind::RwLock => {}
        }
    }
    out
}

fn frame_line(id: u64, function: u32, pc: usize, locals: &std::collections::BTreeMap<usize, Value>) -> String {
    let locals: Vec<String> = locals
        .iter()
        .map(|(k, v)| format!("{k}={}", v.canonical()))
        .collect();
    format!(
        "frame f{id} fn={function} pc={pc} locals={{{}}}\n",
        locals.join(",")
    )
}

fn block_kind_it(b: &BlockReason) -> &'static str {
    match b {
        BlockReason::Lock(_) => "lock",
        BlockReason::ChannelSend(_) => "send",
        BlockReason::ChannelRecv(_) => "recv",
        BlockReason::Condvar(_, _) => "condvar",
        BlockReason::Semaphore(_) => "sem",
        BlockReason::Join(_) => "join",
        BlockReason::Scope(_) => "scope",
    }
}

// ── petri projection ────────────────────────────────────────────────

fn project_pn(pn: &PetriEngine, sp: &SemProgram, s: &NetState) -> String {
    let mut out = String::new();
    out.push_str(&shared_pn(pn, sp, s));
    for f in s.store.frames.values() {
        out.push_str(&frame_line(f.id.0, f.function.0, f.pc, &f.locals));
    }
    for (tid, t) in &s.store.threads {
        let stack: Vec<String> = t
            .stack
            .iter()
            .map(|f| {
                let fr = s.store.frame(*f);
                format!("{}:{}", fr.function.0, fr.pc)
            })
            .collect();
        let status = match &t.blocked_at {
            None if s.store.finished.contains(tid) => "done".to_string(),
            None => "run".to_string(),
            Some(key) => format!("blocked:{}", block_kind_key(key)),
        };
        out.push_str(&format!(
            "thread {} entry={} stack=[{}] {}\n",
            tid.0, t.entry_function.0, stack.join(","), status
        ));
    }
    out.push_str(&format!(
        "completed={:?} reached={:?}\n",
        s.store.completed_functions, s.store.reached
    ));
    out
}

fn shared_pn(pn: &PetriEngine, sp: &SemProgram, s: &NetState) -> String {
    let mut out = String::new();
    let tokens = |key: &PlaceKey| -> Vec<concir::petri::net::NetToken> {
        pn.net
            .place_of
            .get(key)
            .map(|pid| s.place_tokens(*pid).to_vec())
            .unwrap_or_default()
    };
    for r in sp.resources() {
        match r.kind {
            ResKind::Var => {
                let pid = pn.net.place_of.get(&PlaceKey::Var(r.id));
                if let Some(pid) = pid {
                    if let Some(v) = s.read_data(*pid) {
                        out.push_str(&format!("var r{}={}\n", r.id.0, v.canonical()));
                    }
                }
            }
            ResKind::Atomic => {
                let pid = pn.net.place_of.get(&PlaceKey::Atomic(r.id));
                if let Some(pid) = pid {
                    if let Some(v) = s.read_data(*pid) {
                        out.push_str(&format!("atomic r{}={}\n", r.id.0, v.canonical()));
                    }
                }
            }
            ResKind::Mutex => {
                let m = pn
                    .net
                    .place_of
                    .get(&PlaceKey::Mutex(r.id))
                    .and_then(|pid| s.read_mutex(*pid))
                    .map(|m| match m {
                        MutexToken::Free => "free".to_string(),
                        MutexToken::Held(t) => format!("held:{}", t.0),
                    })
                    .unwrap_or_default();
                out.push_str(&format!("mutex r{}={}\n", r.id.0, m));
                let q: Vec<u64> = tokens(&PlaceKey::LockWait(r.id))
                    .iter()
                    .filter_map(|t| t.thread().map(|x| x.0))
                    .collect();
                out.push_str(&format!("lockq r{}={:?}\n", r.id.0, q));
            }
            ResKind::Semaphore => {
                let n = pn
                    .net
                    .place_of
                    .get(&PlaceKey::Semaphore(r.id))
                    .and_then(|pid| s.read_data(*pid))
                    .and_then(Value::as_int)
                    .unwrap_or(0);
                out.push_str(&format!("sem r{}={}\n", r.id.0, n));
                let q: Vec<(u64, i64)> = tokens(&PlaceKey::SemWait(r.id))
                    .iter()
                    .filter_map(|t| match t {
                        concir::petri::net::NetToken::SemWait { thread, count, .. } => {
                            Some((thread.0, *count))
                        }
                        _ => None,
                    })
                    .collect();
                out.push_str(&format!("semq r{}={:?}\n", r.id.0, q));
            }
            ResKind::Channel => {
                let buf: Vec<Value> = pn
                    .net
                    .place_of
                    .get(&PlaceKey::Channel(r.id))
                    .and_then(|pid| s.read_data(*pid))
                    .and_then(|v| match v {
                        Value::Array(a) => Some(a.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                out.push_str(&format!(
                    "chan r{} buf={:?}\n",
                    r.id.0,
                    buf.iter().map(Value::canonical).collect::<Vec<_>>()
                ));
                let sendq: Vec<u64> = tokens(&PlaceKey::ChannelSend(r.id))
                    .iter()
                    .filter_map(|t| t.thread().map(|x| x.0))
                    .collect();
                let recvq: Vec<u64> = tokens(&PlaceKey::ChannelRecv(r.id))
                    .iter()
                    .filter_map(|t| t.thread().map(|x| x.0))
                    .collect();
                out.push_str(&format!("sendq r{}={:?}\n", r.id.0, sendq));
                out.push_str(&format!("recvq r{}={:?}\n", r.id.0, recvq));
            }
            ResKind::Condvar => {
                let q: Vec<u64> = tokens(&PlaceKey::Condvar(r.id))
                    .iter()
                    .filter_map(|t| t.thread().map(|x| x.0))
                    .collect();
                out.push_str(&format!("condvar r{} waiters={:?}\n", r.id.0, q));
            }
            ResKind::RwLock => {}
        }
    }
    out
}

fn block_kind_key(key: &PlaceKey) -> &'static str {
    match key {
        PlaceKey::LockWait(_) => "lock",
        PlaceKey::ChannelSend(_) => "send",
        PlaceKey::ChannelRecv(_) => "recv",
        PlaceKey::Condvar(_) => "condvar",
        PlaceKey::SemWait(_) => "sem",
        PlaceKey::JoinWait { .. } => "join",
        PlaceKey::ScopeWait { .. } => "scope",
        _ => "?",
    }
}

fn it_set(sp: &SemProgram) -> HashSet<String> {
    let b = bounds();
    let it = Interpreter::new(sp, b.clone());
    let r = explore::explore(&it, &b);
    r.states.iter().map(|s| shared_it(sp, s)).collect()
}

fn pn_set(sp: &SemProgram) -> HashSet<String> {
    let b = bounds();
    let pn = PetriEngine::new(sp, b.clone());
    let r = explore::explore(&pn, &b);
    r.states.iter().map(|s| shared_pn(&pn, sp, s)).collect()
}

fn compare(name: &str, src: &str) {
    let sp = lower(src);
    let a = it_set(&sp);
    let b = pn_set(&sp);
    if a != b {
        eprintln!(
            "{name}: shared interp-only {} states, petri-only {} states",
            a.difference(&b).count(),
            b.difference(&a).count()
        );
        for x in a.difference(&b).take(2) {
            eprintln!("  I: {x}");
        }
        for x in b.difference(&a).take(2) {
            eprintln!("  P: {x}");
        }
    }
    assert_eq!(a, b, "shared-state projection mismatch for {name}");
}

#[test]
fn projection_matches_on_examples() {
    compare(
        "producer_consumer",
        include_str!("../examples/producer_consumer.json"),
    );
    compare("state_machine", include_str!("../examples/state_machine.json"));
}

const BUFFERED_CHANNEL: &str = r#"{
  "program": "buffered",
  "modules": [{
    "name": "main",
    "resources": [{"name": "ch", "kind": "sync", "type": "Channel", "mode": "Sync", "base": "Int", "capacity": 2}],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "scope", "funcs": ["p", "c"]},
        {"sid": "s2", "kind": "return"}
      ]},
      {"name": "p", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "channel_send", "channel": "ch", "value": "1"},
        {"sid": "s2", "kind": "channel_send", "channel": "ch", "value": "2"},
        {"sid": "s3", "kind": "channel_send", "channel": "ch", "value": "3"},
        {"sid": "s4", "kind": "return"}
      ]},
      {"name": "c", "kind": "normal", "form": "closure",
       "locals": [{"name": "msg", "type": "Int"}],
       "body": [
        {"sid": "s1", "kind": "channel_recv", "channel": "ch", "dst": "msg"},
        {"sid": "s2", "kind": "channel_recv", "channel": "ch", "dst": "msg"},
        {"sid": "s3", "kind": "channel_recv", "channel": "ch", "dst": "msg"},
        {"sid": "s4", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

const RENDEZVOUS: &str = r#"{
  "program": "rendezvous",
  "modules": [{
    "name": "main",
    "resources": [{"name": "ch", "kind": "sync", "type": "Channel", "mode": "Sync", "base": "Int", "capacity": 0}],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "scope", "funcs": ["p", "c"]},
        {"sid": "s2", "kind": "return"}
      ]},
      {"name": "p", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "channel_send", "channel": "ch", "value": "7"},
        {"sid": "s2", "kind": "return"}
      ]},
      {"name": "c", "kind": "normal", "form": "closure",
       "locals": [{"name": "msg", "type": "Int"}],
       "body": [
        {"sid": "s1", "kind": "channel_recv", "channel": "ch", "dst": "msg"},
        {"sid": "s2", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

const SEMAPHORE_AND_CALL: &str = r#"{
  "program": "sem_call",
  "modules": [{
    "name": "main",
    "resources": [
      {"name": "sem", "kind": "sync", "type": "Semaphore", "mode": "Sync", "count": 2},
      {"name": "x", "kind": "var", "type": "Var", "base": "Int", "init": 0}
    ],
    "functions": [
      {"name": "main", "kind": "normal", "body": [
        {"sid": "s1", "kind": "spawn", "func": "worker", "handle": "h1"},
        {"sid": "s2", "kind": "spawn", "func": "worker", "handle": "h2"},
        {"sid": "s3", "kind": "join", "handle": "h1"},
        {"sid": "s4", "kind": "join", "handle": "h2"},
        {"sid": "s5", "kind": "call", "func": "bump", "args": ["1"]},
        {"sid": "s6", "kind": "return"}
      ]},
      {"name": "worker", "kind": "normal", "form": "closure", "body": [
        {"sid": "s1", "kind": "semaphore_acquire", "resource": "sem"},
        {"sid": "s2", "kind": "write_shared", "resource": "x", "expr": "x + 10"},
        {"sid": "s3", "kind": "semaphore_release", "resource": "sem"},
        {"sid": "s4", "kind": "return"}
      ]},
      {"name": "bump", "kind": "normal",
       "params": [{"name": "n", "type": "Int", "modeled": true}],
       "locals": [{"name": "tmp", "type": "Int"}],
       "body": [
        {"sid": "s1", "kind": "assign_local", "target": "tmp", "expr": "n + 1"},
        {"sid": "s2", "kind": "write_shared", "resource": "x", "expr": "x + tmp"},
        {"sid": "s3", "kind": "return"}
      ]}
    ]
  }],
  "entry": "main::main"
}"#;

#[test]
fn projection_matches_channels_semaphores_calls() {
    compare("buffered_channel", BUFFERED_CHANNEL);
    compare("rendezvous", RENDEZVOUS);
    compare("semaphore_and_call", SEMAPHORE_AND_CALL);
}

