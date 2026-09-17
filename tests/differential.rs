//! Differential acceptance (R10).
//!
//! The reference interpreter and the Petri-net executor must expose the same
//! CIR-observable semantics. We compare a **complete** normalization of the
//! state (control positions, call stack, locals, frame handle bindings,
//! concrete wait relations, messages and their values, completion monitors,
//! blocked/finished facts) and the **labeled transition relation** — not just a
//! set of shared-variable snapshots or state counts.
//!
//! Auxiliary net steps (wait-acquire / grant / delivery) are projected onto the
//! blocked thread's CIR statement, which is exactly the interpreter's resume
//! step. Both engines must first be shown complete with no Invalid/Unsupported
//! or boundary events.

use std::collections::{BTreeMap, BTreeSet};

use concir::ast::Program;
use concir::explore;
use concir::interp::state::{BlockReason, MachineState, MutexState as ItMutex, ThreadStatus};
use concir::interp::Interpreter;
use concir::petri::exec::PetriEngine;
use concir::petri::net::{MutexToken, NetState, PlaceKey};
use concir::sem::ids::{FrameId, FunctionId, ResourceId, ThreadId};
use concir::sem::outcome::AnalysisBounds;
use concir::sem::program::{self, ResKind, SemProgram};
use concir::sem::value::Value;

fn bounds() -> AnalysisBounds {
    AnalysisBounds {
        max_threads: 8,
        max_frames_per_thread: 8,
        max_states: 30_000,
        max_depth: 200,
        max_boundary_events: 512,
    }
}

fn lower(src: &str) -> SemProgram {
    let p: Program = serde_json::from_str(src).unwrap();
    program::lower(&p).unwrap()
}

fn fn_key(sp: &SemProgram, f: FunctionId) -> String {
    format!("M{}::{}", sp.function(f).module.index(), sp.function(f).name)
}

fn res_key(sp: &SemProgram, r: ResourceId) -> String {
    let res = sp.resource(r);
    format!("M{}::{}", res.module.index(), res.name)
}

// ── interpreter full projection ─────────────────────────────────────

fn proj_it(sp: &SemProgram, s: &MachineState) -> String {
    let thread_order: Vec<ThreadId> = s.threads.keys().copied().collect();
    let tmap: BTreeMap<ThreadId, usize> = thread_order
        .iter()
        .enumerate()
        .map(|(i, t)| (*t, i))
        .collect();
    let tn = |t: ThreadId| format!("T{}", tmap.get(&t).copied().unwrap_or(999));
    let frame_order: Vec<FrameId> = s.store.frames.keys().copied().collect();
    let fmap: BTreeMap<FrameId, usize> = frame_order
        .iter()
        .enumerate()
        .map(|(i, f)| (*f, i))
        .collect();
    let fnn = |f: FrameId| format!("F{}", fmap.get(&f).copied().unwrap_or(999));

    let mut out = String::new();
    // Shared data, in a stable resource order.
    for r in sp.resources() {
        match r.kind {
            ResKind::Var => {
                if let Some(v) = s.store.vars.get(&r.id) {
                    out.push_str(&format!("var {}={}\n", res_key(sp, r.id), v.canonical()));
                }
            }
            ResKind::Atomic => {
                if let Some(v) = s.store.atomics.get(&r.id) {
                    out.push_str(&format!("atomic {}={}\n", res_key(sp, r.id), v.canonical()));
                }
            }
            ResKind::Mutex => {
                let m = match s.store.mutexes.get(&r.id) {
                    Some(ItMutex::Free) | None => "free".to_string(),
                    Some(ItMutex::Held(t)) => format!("held:{}", tn(*t)),
                };
                out.push_str(&format!("mutex {}={}\n", res_key(sp, r.id), m));
                let mut waiters: Vec<String> = s
                    .threads
                    .iter()
                    .filter(|(_, t)| {
                        matches!(&t.status, ThreadStatus::Blocked(BlockReason::Lock(x)) if x == &r.id)
                    })
                    .map(|(t, _)| tn(*t))
                    .collect();
                waiters.sort();
                out.push_str(&format!("lockwait {}={:?}\n", res_key(sp, r.id), waiters));
            }
            ResKind::Semaphore => {
                out.push_str(&format!(
                    "sem {}={}\n",
                    res_key(sp, r.id),
                    s.store.semaphores.get(&r.id).copied().unwrap_or(0)
                ));
                let mut q: Vec<String> = s
                    .sem_waiters
                    .get(&r.id)
                    .map(|q| q.iter().map(|(t, n)| format!("{}:{}", tn(*t), n)).collect())
                    .unwrap_or_default();
                q.sort();
                out.push_str(&format!("semwait {}={:?}\n", res_key(sp, r.id), q));
            }
            ResKind::Channel => {
                let ch = s.store.channels.get(&r.id).cloned().unwrap_or_default();
                out.push_str(&format!(
                    "chan {} buf={:?}\n",
                    res_key(sp, r.id),
                    ch.buffer.iter().map(Value::canonical).collect::<Vec<_>>()
                ));
                out.push_str(&format!(
                    "sendq {}={:?}\n",
                    res_key(sp, r.id),
                    ch.pending_send
                        .iter()
                        .map(|p| format!("{}:{}", tn(p.thread), p.value.canonical()))
                        .collect::<Vec<_>>()
                ));
                out.push_str(&format!(
                    "recvq {}={:?}\n",
                    res_key(sp, r.id),
                    ch.pending_recv.iter().map(|t| tn(*t)).collect::<Vec<_>>()
                ));
            }
            ResKind::Condvar => {
                let cv = s.store.condvars.get(&r.id).cloned().unwrap_or_default();
                let lock = if cv.waiters.is_empty() {
                    None
                } else {
                    cv.lock.map(|l| res_key(sp, l))
                };
                out.push_str(&format!(
                    "condvar {} waiters={:?} lock={:?}\n",
                    res_key(sp, r.id),
                    cv.waiters.iter().map(|t| tn(*t)).collect::<Vec<_>>(),
                    lock
                ));
            }
            ResKind::RwLock => {}
        }
    }
    // Frames.
    for f in &frame_order {
        let fr = &s.store.frames[f];
        let mut locals: Vec<String> = fr
            .locals
            .iter()
            .map(|(k, v)| format!("{k}={}", v.canonical()))
            .collect();
        locals.sort();
        // Resolve handle ids to child thread ordinals via the owning thread.
        let owner = thread_order
            .iter()
            .find(|t| s.threads[t].stack.contains(&fr.id));
        let mut handles: Vec<usize> = Vec::new();
        if let Some(owner) = owner {
            handles = fr
                .handles
                .values()
                .filter_map(|h| s.threads[owner].handle_children.get(h).map(|c| tmap[c]))
                .collect();
            handles.sort();
        }
        let ret = match &fr.ret {
            Some(r) => format!("ret(pc={},dst={:?})", r.pc_next, r.dst),
            None => "ret(none)".to_string(),
        };
        out.push_str(&format!(
            "frame {} fn={} pc={} locals={:?} handles={:?} {}\n",
            fnn(*f),
            fn_key(sp, fr.function),
            fr.pc,
            locals,
            handles,
            ret
        ));
    }
    // Threads.
    for (tid, t) in &s.threads {
        let stack: Vec<String> = t
            .stack
            .iter()
            .map(|f| format!("{}@{}", fn_key(sp, s.store.frames[f].function), s.store.frames[f].pc))
            .collect();
        let status = match &t.status {
            ThreadStatus::Runnable => "run".to_string(),
            ThreadStatus::Finished => "done".to_string(),
            ThreadStatus::Blocked(b) => format!("blocked:{}", block_it(sp, b)),
        };
        out.push_str(&format!(
            "thread {} entry={} stack={stack:?} {}\n",
            tn(*tid),
            fn_key(sp, t.entry_function),
            status
        ));
    }
    for sc in s.scopes.values() {
        let mut rem: Vec<String> = sc.remaining.iter().map(|t| tn(*t)).collect();
        rem.sort();
        out.push_str(&format!(
            "scope owner={} frame={} remaining={:?}\n",
            tn(sc.owner),
            fnn(sc.owner_frame),
            rem
        ));
    }
    let comp: BTreeMap<String, usize> = s
        .completed_functions
        .iter()
        .map(|(f, n)| (fn_key(sp, *f), *n))
        .collect();
    let mut reached: Vec<String> = s
        .reached
        .iter()
        .map(|(f, sid)| format!("{}@{}", fn_key(sp, *f), sid))
        .collect();
    reached.sort();
    let mut scopes: Vec<String> = s
        .completed_scopes
        .iter()
        .map(|(f, sid)| format!("{}@{}", fn_key(sp, *f), sid))
        .collect();
    scopes.sort();
    out.push_str(&format!(
        "completed={comp:?} scopes={scopes:?} reached={reached:?}\n"
    ));
    out
}

fn block_it(sp: &SemProgram, b: &BlockReason) -> String {
    match b {
        BlockReason::Lock(r) => format!("lock:{}", res_key(sp, *r)),
        BlockReason::ChannelSend(r) => format!("send:{}", res_key(sp, *r)),
        BlockReason::ChannelRecv(r) => format!("recv:{}", res_key(sp, *r)),
        BlockReason::Condvar(c, _l) => format!("condvar:{}", res_key(sp, *c)),
        BlockReason::Semaphore(r) => format!("sem:{}", res_key(sp, *r)),
        BlockReason::Join(_) => "join".to_string(),
        BlockReason::Scope(_) => "scope".to_string(),
    }
}

// ── petri full projection ───────────────────────────────────────────

fn proj_pn(pn: &PetriEngine, sp: &SemProgram, s: &NetState) -> String {
    let thread_order: Vec<ThreadId> = s.store.threads.keys().copied().collect();
    let tmap: BTreeMap<ThreadId, usize> = thread_order
        .iter()
        .enumerate()
        .map(|(i, t)| (*t, i))
        .collect();
    let tn = |t: ThreadId| format!("T{}", tmap.get(&t).copied().unwrap_or(999));
    let frame_order: Vec<FrameId> = s.store.frames.keys().copied().collect();
    let fmap: BTreeMap<FrameId, usize> = frame_order
        .iter()
        .enumerate()
        .map(|(i, f)| (*f, i))
        .collect();
    let fnn = |f: FrameId| format!("F{}", fmap.get(&f).copied().unwrap_or(999));
    let place = |key: &PlaceKey| pn.net.place_of.get(key).copied();
    let toks = |key: &PlaceKey| -> Vec<concir::petri::net::NetToken> {
        place(key)
            .map(|pid| s.place_tokens(pid).to_vec())
            .unwrap_or_default()
    };

    let mut out = String::new();
    for r in sp.resources() {
        match r.kind {
            ResKind::Var => {
                if let Some(pid) = place(&PlaceKey::Var(r.id)) {
                    if let Some(v) = s.read_data(pid) {
                        out.push_str(&format!("var {}={}\n", res_key(sp, r.id), v.canonical()));
                    }
                }
            }
            ResKind::Atomic => {
                if let Some(pid) = place(&PlaceKey::Atomic(r.id)) {
                    if let Some(v) = s.read_data(pid) {
                        out.push_str(&format!("atomic {}={}\n", res_key(sp, r.id), v.canonical()));
                    }
                }
            }
            ResKind::Mutex => {
                let m = place(&PlaceKey::Mutex(r.id))
                    .and_then(|pid| s.read_mutex(pid))
                    .map(|m| match m {
                        MutexToken::Free => "free".to_string(),
                        MutexToken::Held(t) => format!("held:{}", tn(t)),
                    })
                    .unwrap_or_else(|| "free".to_string());
                out.push_str(&format!("mutex {}={}\n", res_key(sp, r.id), m));
                let mut waiters: Vec<String> = toks(&PlaceKey::LockWait(r.id))
                    .iter()
                    .filter_map(|t| t.thread().map(tn))
                    .collect();
                waiters.sort();
                out.push_str(&format!("lockwait {}={:?}\n", res_key(sp, r.id), waiters));
            }
            ResKind::Semaphore => {
                let n = place(&PlaceKey::Semaphore(r.id))
                    .and_then(|pid| s.read_data(pid))
                    .and_then(Value::as_int)
                    .unwrap_or(0);
                out.push_str(&format!("sem {}={}\n", res_key(sp, r.id), n));
                let mut q: Vec<String> = toks(&PlaceKey::SemWait(r.id))
                    .iter()
                    .filter_map(|t| match t {
                        concir::petri::net::NetToken::SemWait { thread, count, .. } => {
                            Some(format!("{}:{}", tn(*thread), count))
                        }
                        _ => None,
                    })
                    .collect();
                q.sort();
                out.push_str(&format!("semwait {}={:?}\n", res_key(sp, r.id), q));
            }
            ResKind::Channel => {
                let buf: Vec<Value> = place(&PlaceKey::Channel(r.id))
                    .and_then(|pid| s.read_data(pid))
                    .and_then(|v| match v {
                        Value::Array(a) => Some(a.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                out.push_str(&format!(
                    "chan {} buf={:?}\n",
                    res_key(sp, r.id),
                    buf.iter().map(Value::canonical).collect::<Vec<_>>()
                ));
                let sendq: Vec<String> = toks(&PlaceKey::ChannelSend(r.id))
                    .iter()
                    .filter_map(|t| match t {
                        concir::petri::net::NetToken::SendWait { thread, value, .. } => {
                            Some(format!("{}:{}", tn(*thread), value.canonical()))
                        }
                        _ => None,
                    })
                    .collect();
                let recvq: Vec<String> = toks(&PlaceKey::ChannelRecv(r.id))
                    .iter()
                    .filter_map(|t| t.thread().map(tn))
                    .collect();
                out.push_str(&format!("sendq {}={:?}\n", res_key(sp, r.id), sendq));
                out.push_str(&format!("recvq {}={:?}\n", res_key(sp, r.id), recvq));
            }
            ResKind::Condvar => {
                let cv_tokens = toks(&PlaceKey::Condvar(r.id));
                let waiters: Vec<String> = cv_tokens
                    .iter()
                    .filter_map(|t| t.thread().map(tn))
                    .collect();
                let lock = cv_tokens.iter().find_map(|t| match t {
                    concir::petri::net::NetToken::CondvarWait { lock, .. } => {
                        Some(res_key(sp, *lock))
                    }
                    _ => None,
                });
                out.push_str(&format!(
                    "condvar {} waiters={:?} lock={:?}\n",
                    res_key(sp, r.id),
                    waiters,
                    lock
                ));
            }
            ResKind::RwLock => {}
        }
    }
    for f in &frame_order {
        let fr = &s.store.frames[f];
        let mut locals: Vec<String> = fr
            .locals
            .iter()
            .map(|(k, v)| format!("{k}={}", v.canonical()))
            .collect();
        locals.sort();
        let owner = thread_order
            .iter()
            .find(|t| s.store.threads[t].stack.contains(&fr.id));
        let mut kids: Vec<usize> = Vec::new();
        if let Some(owner) = owner {
            kids = fr
                .handles
                .values()
                .filter_map(|h| s.store.threads[owner].handle_children.get(h).map(|c| tmap[c]))
                .collect();
            kids.sort();
        }
        let ret = match &fr.ret {
            Some(r) => format!("ret(pc={},dst={:?})", r.pc_next, r.dst),
            None => "ret(none)".to_string(),
        };
        out.push_str(&format!(
            "frame {} fn={} pc={} locals={:?} handles={:?} {}\n",
            fnn(*f),
            fn_key(sp, fr.function),
            fr.pc,
            locals,
            kids,
            ret
        ));
    }
    for (tid, t) in &s.store.threads {
        let stack: Vec<String> = t
            .stack
            .iter()
            .map(|f| format!("{}@{}", fn_key(sp, s.store.frames[f].function), s.store.frames[f].pc))
            .collect();
        let status = if s.store.finished.contains(tid) {
            "done".to_string()
        } else {
            match &t.blocked_at {
                None => "run".to_string(),
                Some(key) => format!("blocked:{}", block_pn(sp, key)),
            }
        };
        out.push_str(&format!(
            "thread {} entry={} stack={stack:?} {}\n",
            tn(*tid),
            fn_key(sp, t.entry_function),
            status
        ));
    }
    for sc in s.store.scopes.values() {
        let mut rem: Vec<String> = sc.remaining.iter().map(|t| tn(*t)).collect();
        rem.sort();
        out.push_str(&format!(
            "scope owner={} frame={} remaining={:?}\n",
            tn(sc.owner),
            fnn(sc.owner_frame),
            rem
        ));
    }
    let comp: BTreeMap<String, usize> = s
        .store
        .completed_functions
        .iter()
        .map(|(f, n)| (fn_key(sp, *f), *n))
        .collect();
    let mut reached: Vec<String> = s
        .store
        .reached
        .iter()
        .map(|(f, sid)| format!("{}@{}", fn_key(sp, *f), sid))
        .collect();
    reached.sort();
    let mut scopes: Vec<String> = s
        .store
        .completed_scopes
        .iter()
        .map(|(f, sid)| format!("{}@{}", fn_key(sp, *f), sid))
        .collect();
    scopes.sort();
    out.push_str(&format!(
        "completed={comp:?} scopes={scopes:?} reached={reached:?}\n"
    ));
    out
}

fn block_pn(sp: &SemProgram, key: &PlaceKey) -> String {
    match key {
        PlaceKey::LockWait(r) => format!("lock:{}", res_key(sp, *r)),
        PlaceKey::ChannelSend(r) => format!("send:{}", res_key(sp, *r)),
        PlaceKey::ChannelRecv(r) => format!("recv:{}", res_key(sp, *r)),
        PlaceKey::Condvar(r) => format!("condvar:{}", res_key(sp, *r)),
        PlaceKey::SemWait(r) => format!("sem:{}", res_key(sp, *r)),
        PlaceKey::JoinWait { .. } => "join".to_string(),
        PlaceKey::ScopeWait { .. } => "scope".to_string(),
        _ => "?".to_string(),
    }
}

// ── exhaustive exploration with edges ───────────────────────────────

struct Explored {
    projections: BTreeSet<String>,
    edges: BTreeSet<(String, String, String)>,
}

fn explore_it(sp: &SemProgram) -> Explored {
    let b = bounds();
    let it = Interpreter::new(sp, b.clone());
    let r = explore::explore(&it, &b);
    assert!(r.boundary_events.is_empty(), "interp boundary: {:?}", r.boundary_events);
    assert!(r.unsupported.is_empty(), "interp unsupported: {:?}", r.unsupported);
    assert!(r.invalid.is_empty(), "interp invalid: {:?}", r.invalid);
    assert!(!r.truncated, "interp search truncated");

    let mut projections = BTreeSet::new();
    let mut edges = BTreeSet::new();
    for (i, s) in r.states.iter().enumerate() {
        let p = proj_it(sp, s);
        projections.insert(p.clone());
        for (t, label) in &r.edges[i] {
            // Reconstruct the step label by replaying? We need source/target
            // labels; `edges` only stores target + label.
            let thread_order: Vec<ThreadId> = s.threads.keys().copied().collect();
            let tmap: BTreeMap<ThreadId, usize> =
                thread_order.iter().enumerate().map(|(k, t)| (*t, k)).collect();
            let frame_order: Vec<FrameId> = s.store.frames.keys().copied().collect();
            let fmap: BTreeMap<FrameId, usize> =
                frame_order.iter().enumerate().map(|(k, f)| (*f, k)).collect();
            let tid = label.thread.map(|x| format!("T{}", tmap[&x])).unwrap_or_default();
            let fid = label.frame.map(|x| format!("F{}", fmap[&x])).unwrap_or_default();
            let sid = label.origin.sid;
            let key = format!(
                "{}#{}#{}#{}",
                fn_key(sp, label.origin.function),
                sid.map(|x| x.to_string()).unwrap_or_else(|| "-".into()),
                tid,
                fid
            );
            edges.insert((p.clone(), key, proj_it(sp, &r.states[*t])));
        }
    }
    Explored { projections, edges }
}

fn explore_pn(sp: &SemProgram) -> Explored {
    let b = bounds();
    let pn = PetriEngine::new(sp, b.clone());
    let r = explore::explore(&pn, &b);
    assert!(r.boundary_events.is_empty(), "petri boundary: {:?}", r.boundary_events);
    assert!(r.unsupported.is_empty(), "petri unsupported: {:?}", r.unsupported);
    assert!(r.invalid.is_empty(), "petri invalid: {:?}", r.invalid);
    assert!(!r.truncated, "petri search truncated");

    let mut projections = BTreeSet::new();
    let mut edges = BTreeSet::new();
    for (i, s) in r.states.iter().enumerate() {
        let p = proj_pn(&pn, sp, s);
        projections.insert(p.clone());
        for (t, label) in &r.edges[i] {
            let thread_order: Vec<ThreadId> = s.store.threads.keys().copied().collect();
            let tmap: BTreeMap<ThreadId, usize> =
                thread_order.iter().enumerate().map(|(k, t)| (*t, k)).collect();
            let frame_order: Vec<FrameId> = s.store.frames.keys().copied().collect();
            let fmap: BTreeMap<FrameId, usize> =
                frame_order.iter().enumerate().map(|(k, f)| (*f, k)).collect();
            let tid = label.thread.map(|x| format!("T{}", tmap[&x])).unwrap_or_default();
            let fid = label.frame.map(|x| format!("F{}", fmap[&x])).unwrap_or_default();
            // Project auxiliary net steps onto the blocked thread's CIR
            // statement (the interpreter's resume step).
            let (function, sid) = match label.origin.sid {
                Some(sid) => (fn_key(sp, label.origin.function), sid.to_string()),
                None => {
                    let frame = label.frame.expect("aux step binds a frame");
                    let fr = s.store.frame(frame);
                    (fn_key(sp, fr.function), fr.pc.to_string())
                }
            };
            let key = format!("{function}#{sid}#{tid}#{fid}");
            edges.insert((p.clone(), key, proj_pn(&pn, sp, &r.states[*t])));
        }
    }
    Explored { projections, edges }
}

fn compare(name: &str, src: &str) {
    let sp = lower(src);
    let it = explore_it(&sp);
    let pn = explore_pn(&sp);
    if it.projections != pn.projections {
        eprintln!(
            "{name}: projection sets differ (interp-only {}, petri-only {})",
            it.projections.difference(&pn.projections).count(),
            pn.projections.difference(&it.projections).count()
        );
        for x in it.projections.difference(&pn.projections).take(2) {
            eprintln!("  I-only:\n{x}");
        }
        for x in pn.projections.difference(&it.projections).take(2) {
            eprintln!("  P-only:\n{x}");
        }
    }
    assert_eq!(it.projections, pn.projections, "state projection mismatch for {name}");
    if it.edges != pn.edges {
        eprintln!(
            "{name}: edge relations differ (interp-only {}, petri-only {})",
            it.edges.difference(&pn.edges).count(),
            pn.edges.difference(&it.edges).count()
        );
        for x in it.edges.difference(&pn.edges).take(2) {
            eprintln!("  I-only edge {:?}", x.1);
        }
        for x in pn.edges.difference(&it.edges).take(2) {
            eprintln!("  P-only edge {:?}", x.1);
        }
    }
    assert_eq!(it.edges, pn.edges, "edge relation mismatch for {name}");
}

// ── corpus ──────────────────────────────────────────────────────────

#[test]
fn differential_matches_producer_consumer() {
    compare(
        "producer_consumer",
        include_str!("../examples/producer_consumer.json"),
    );
}

#[test]
fn differential_matches_state_machine() {
    compare("state_machine", include_str!("../examples/state_machine.json"));
}

#[test]
fn differential_matches_semaphore_and_call() {
    let src = r#"{
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
    compare("semaphore_and_call", src);
}

#[test]
fn differential_matches_buffered_channel_and_rendezvous() {
    let buffered = r#"{
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
    compare("buffered_channel", buffered);

    let rendezvous = r#"{
      "program": "rdv",
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
    compare("rendezvous", rendezvous);
}

#[test]
fn differential_matches_nested_handles() {
    compare(
        "nested_handle_false_pass",
        include_str!("repro_round2/nested_handle_false_pass.json"),
    );
    compare(
        "nested_handle_names",
        include_str!("repro_round2/nested_handle_names.json"),
    );
}

#[test]
fn differential_matches_notify_choice() {
    compare(
        "notify_choice_false_pass",
        include_str!("repro_round2/notify_choice_false_pass.json"),
    );
}

// ── meta-transformations ────────────────────────────────────────────

fn outcome_of(src: &str) -> concir::sem::outcome::Outcome {
    let spec: concir::explore::contract::ContractSpec = serde_json::from_str(
        r#"{"name":"d","properties":[{"kind":"deadlock_free","id":"d"}]}"#,
    )
    .unwrap();
    concir::explore::verify_program(
        &serde_json::from_str(src).unwrap(),
        &spec,
        concir::explore::EngineKind::Petri,
    )
    .outcome
}

#[test]
fn differential_is_invariant_under_declaration_reordering() {
    // Reordering independent function declarations must not change outcomes.
    let a = r#"{
      "program": "reorder",
      "modules": [{"name": "main",
        "resources": [{"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"}],
        "functions": [
          {"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "scope", "funcs": ["f", "g"]},
            {"sid": "s2", "kind": "return"}]},
          {"name": "f", "kind": "normal", "form": "closure", "body": [
            {"sid": "s1", "kind": "mutex_lock", "resource": "m"},
            {"sid": "s2", "kind": "mutex_unlock", "resource": "m"},
            {"sid": "s3", "kind": "return"}]},
          {"name": "g", "kind": "normal", "form": "closure", "body": [
            {"sid": "s1", "kind": "mutex_lock", "resource": "m"},
            {"sid": "s2", "kind": "mutex_unlock", "resource": "m"},
            {"sid": "s3", "kind": "return"}]}
        ]}],
      "entry": "main::main"
    }"#;
    let b = r#"{
      "program": "reorder",
      "modules": [{"name": "main",
        "resources": [{"name": "m", "kind": "sync", "type": "Mutex", "mode": "Sync"}],
        "functions": [
          {"name": "g", "kind": "normal", "form": "closure", "body": [
            {"sid": "s1", "kind": "mutex_lock", "resource": "m"},
            {"sid": "s2", "kind": "mutex_unlock", "resource": "m"},
            {"sid": "s3", "kind": "return"}]},
          {"name": "main", "kind": "normal", "body": [
            {"sid": "s1", "kind": "scope", "funcs": ["f", "g"]},
            {"sid": "s2", "kind": "return"}]},
          {"name": "f", "kind": "normal", "form": "closure", "body": [
            {"sid": "s1", "kind": "mutex_lock", "resource": "m"},
            {"sid": "s2", "kind": "mutex_unlock", "resource": "m"},
            {"sid": "s3", "kind": "return"}]}
        ]}],
      "entry": "main::main"
    }"#;
    assert_eq!(outcome_of(a), outcome_of(b));
}

#[test]
fn differential_catches_shared_error_with_hand_written_expectation() {
    // R1: a correct implementation must find the deadlock. This is a
    // hand-written expectation, not agreement between the two engines.
    let src = include_str!("repro_round2/nested_handle_false_pass.json");
    assert_eq!(outcome_of(src), concir::sem::outcome::Outcome::Fail);
    // R4: choosing the wrong waiter creates a deadlock.
    let src = include_str!("repro_round2/notify_choice_false_pass.json");
    assert_eq!(outcome_of(src), concir::sem::outcome::Outcome::Fail);
}
