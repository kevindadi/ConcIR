//! Round-5 corrections: D1 (declared return-type domain) and the remaining C1
//! acceptance evidence (identity translation, successor quotient, trace
//! executability).

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use concir::ast::Program;
use concir::explore;
use concir::explore::{verify_program, EngineKind, VerificationReport};
use concir::interp::state::{
    BlockReason, ChannelState, Frame, MachineState, MutexState, RetAddr, ScopeState, ThreadState,
    ThreadStatus,
};
use concir::interp::Interpreter;
use concir::petri::exec::PetriEngine;
use concir::petri::net::{
    MutexToken, NetAlloc, NetFrame, NetRetAddr, NetScope, NetState, NetStore, NetThread, NetToken,
};
use concir::sem::ids::{FrameId, HandleId, ScopeId, ThreadId};
use concir::sem::outcome::{AnalysisBounds, Outcome};
use concir::sem::program;
use concir::sem::system::{Predicate, TransitionSystem};
use concir::sem::value::Value;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("tests/repro_round5/{name}")).expect(name)
}

fn prog(name: &str) -> Program {
    serde_json::from_str(&fixture(name)).unwrap()
}

fn spec(name: &str) -> concir::explore::contract::ContractSpec {
    serde_json::from_str(&fixture(name)).unwrap()
}

fn run(program_name: &str, contract_name: &str, engine: EngineKind) -> VerificationReport {
    verify_program(&prog(program_name), &spec(contract_name), engine)
}

fn both(program_name: &str, contract_name: &str) -> (Outcome, Outcome, bool, bool) {
    let i = run(program_name, contract_name, EngineKind::Interpreter);
    let p = run(program_name, contract_name, EngineKind::Petri);
    (i.outcome, p.outcome, i.complete, p.complete)
}

fn bounds() -> AnalysisBounds {
    AnalysisBounds {
        max_threads: 8,
        max_frames_per_thread: 8,
        max_states: 20_000,
        max_depth: 60,
        max_boundary_events: 256,
    }
}

// ── D1: declared return-type domain ─────────────────────────────────

#[test]
fn d1_out_of_domain_return_is_disabled() {
    for (p, c) in [
        ("r4_return_domain_wide_dst.json", "r4_return_domain_wide_dst_contract.json"),
        ("r4_return_domain_discard.json", "r4_return_domain_discard_contract.json"),
        ("r4_return_domain_entry.json", "r4_return_domain_entry_contract.json"),
    ] {
        let (i, pe, ic, pc) = both(p, c);
        assert_eq!(i, Outcome::Fail, "{p} interpreter must not complete");
        assert_eq!(pe, Outcome::Fail, "{p} petri must not complete");
        assert!(ic && pc, "{p} must be complete");
    }
    let (i, pe, _, _) = both("r4_return_domain_valid.json", "r4_return_domain_valid_contract.json");
    assert_eq!(i, Outcome::Pass);
    assert_eq!(pe, Outcome::Pass);
}

#[test]
fn d1_disabled_return_is_atomic() {
    // The call must not resume the caller: main.s2 is never reached and main
    // never completes, because the callee's return is disabled before any
    // unwind, dst write, or completion.
    for engine in [EngineKind::Interpreter, EngineKind::Petri] {
        let report = verify_program(
            &prog("r4_return_domain_wide_dst.json"),
            &spec("r4_return_domain_wide_dst_contract.json"),
            engine,
        );
        assert_eq!(report.outcome, Outcome::Fail);
        // Reachability of the caller's next statement must also fail.
        let contract = r#"{"name":"c","properties":[{"kind":"reachability","id":"r","goal":{"kind":"statement_reached","function":"main::main","sid":"s2"}}]}"#;
        let rep = verify_program(
            &prog("r4_return_domain_wide_dst.json"),
            &serde_json::from_str(contract).unwrap(),
            engine,
        );
        assert_eq!(rep.outcome, Outcome::Fail, "caller must not resume");
    }
}

#[test]
fn d1_nested_and_composite_returns() {
    // Nested call: mid calls two (bounded return) and discards; two never
    // completes, so mid/main never complete.
    let nested = r#"{
      "program": "nested_return",
      "modules": [{"name": "main",
        "resources": [{"name": "src", "kind": "var", "type": "Var", "base": "Int", "init": 2}],
        "functions": [
          {"name": "main", "kind": "normal", "body": [{"sid":"s1","kind":"call","func":"mid","args":[]},{"sid":"s2","kind":"return"}]},
          {"name": "mid", "kind": "normal", "body": [{"sid":"s1","kind":"call","func":"two","args":[]},{"sid":"s2","kind":"return"}]},
          {"name": "two", "kind": "normal", "returns": {"name":"r","type":{"Int":[0,1]},"modeled":true}, "body": [{"sid":"s1","kind":"return","value":"src"}]}
        ]}],
      "entry": "main::main"
    }"#;
    let goal = r#"{"name":"c","properties":[{"kind":"reachability","id":"done","goal":{"kind":"function_completed","function":"main::two"}}]}"#;
    for engine in [EngineKind::Interpreter, EngineKind::Petri] {
        let rep = verify_program(
            &serde_json::from_str(nested).unwrap(),
            &serde_json::from_str(goal).unwrap(),
            engine,
        );
        assert_eq!(rep.outcome, Outcome::Fail, "nested return must be disabled");
    }

    // Composite return: Struct{n:Int[0,1]} returned from Struct{n:Int}=2.
    let comp = r#"{
      "program": "composite_return",
      "modules": [{"name": "main",
        "resources": [{"name": "src", "kind": "var", "type": "Var", "base": {"Struct": {"n": "Int"}}, "init": {"n": 2}}],
        "functions": [
          {"name": "main", "kind": "normal", "body": [{"sid":"s1","kind":"call","func":"two","args":[]},{"sid":"s2","kind":"return"}]},
          {"name": "two", "kind": "normal", "returns": {"name":"r","type":{"Struct":{"n":{"Int":[0,1]}}},"modeled":true}, "body": [{"sid":"s1","kind":"return","value":"src"}]}
        ]}],
      "entry": "main::main"
    }"#;
    for engine in [EngineKind::Interpreter, EngineKind::Petri] {
        let rep = verify_program(
            &serde_json::from_str(comp).unwrap(),
            &serde_json::from_str(goal).unwrap(),
            engine,
        );
        assert_eq!(rep.outcome, Outcome::Fail, "composite return must be disabled");
    }

    // In domain composite value completes.
    let comp_ok = comp.replace("\"init\": {\"n\": 2}", "\"init\": {\"n\": 1}");
    assert_ne!(comp_ok, comp, "test fixture must contain the literal init");
    for engine in [EngineKind::Interpreter, EngineKind::Petri] {
        let rep = verify_program(
            &serde_json::from_str(&comp_ok).unwrap(),
            &serde_json::from_str(goal).unwrap(),
            engine,
        );
        assert_eq!(rep.outcome, Outcome::Pass, "legal composite return");
    }
}

// ── C1 evidence: identity translation ───────────────────────────────

const K: u64 = 1000;

fn shift_it(s: &MachineState) -> MachineState {
    let mut out = s.clone();
    // frames
    let frames: BTreeMap<FrameId, Frame> = s
        .store
        .frames
        .iter()
        .map(|(id, f)| {
            let mut f = f.clone();
            f.id = FrameId(f.id.0 + K);
            f.handles = f
                .handles
                .iter()
                .map(|(k, h)| (k.clone(), HandleId(h.0 + K)))
                .collect();
            f.ret = f.ret.as_ref().map(|r| RetAddr {
                pc_next: r.pc_next,
                dst: r.dst,
            });
            (FrameId(id.0 + K), f)
        })
        .collect();
    out.store.frames = frames;
    out.store.mutexes = s
        .store
        .mutexes
        .iter()
        .map(|(r, m)| {
            let m = match m {
                MutexState::Free => MutexState::Free,
                MutexState::Held(t) => MutexState::Held(ThreadId(t.0 + K)),
            };
            (*r, m)
        })
        .collect();
    out.store.channels = s
        .store
        .channels
        .iter()
        .map(|(r, c)| {
            let mut c: ChannelState = c.clone();
            for p in c.pending_send.iter_mut() {
                p.thread = ThreadId(p.thread.0 + K);
            }
            c.pending_recv = c
                .pending_recv
                .iter()
                .map(|t| ThreadId(t.0 + K))
                .collect();
            (*r, c)
        })
        .collect();
    out.store.condvars = s
        .store
        .condvars
        .iter()
        .map(|(r, c)| {
            let mut c = c.clone();
            c.waiters = c
                .waiters
                .iter()
                .map(|(t, l)| (ThreadId(t.0 + K), *l))
                .collect();
            (*r, c)
        })
        .collect();
    out.threads = s
        .threads
        .iter()
        .map(|(id, t)| {
            let mut t: ThreadState = t.clone();
            t.id = ThreadId(t.id.0 + K);
            t.stack = t.stack.iter().map(|f| FrameId(f.0 + K)).collect();
            t.handle_children = t
                .handle_children
                .iter()
                .map(|(h, c)| (HandleId(h.0 + K), ThreadId(c.0 + K)))
                .collect();
            t.parent_scope = t.parent_scope.map(|sc| ScopeId(sc.0 + K));
            t.status = match &t.status {
                ThreadStatus::Runnable => ThreadStatus::Runnable,
                ThreadStatus::Finished => ThreadStatus::Finished,
                ThreadStatus::Blocked(b) => ThreadStatus::Blocked(match b {
                    BlockReason::Join(h) => BlockReason::Join(HandleId(h.0 + K)),
                    BlockReason::Scope(sc) => BlockReason::Scope(ScopeId(sc.0 + K)),
                    other => other.clone(),
                }),
            };
            (ThreadId(id.0 + K), t)
        })
        .collect();
    out.sem_waiters = s
        .sem_waiters
        .iter()
        .map(|(r, q)| {
            (
                *r,
                q.iter()
                    .map(|(t, n)| (ThreadId(t.0 + K), *n))
                    .collect(),
            )
        })
        .collect();
    out.scopes = s
        .scopes
        .iter()
        .map(|(id, sc)| {
            let mut sc: ScopeState = sc.clone();
            sc.id = ScopeId(sc.id.0 + K);
            sc.owner = ThreadId(sc.owner.0 + K);
            sc.owner_frame = FrameId(sc.owner_frame.0 + K);
            sc.remaining = sc.remaining.iter().map(|t| ThreadId(t.0 + K)).collect();
            (ScopeId(id.0 + K), sc)
        })
        .collect();
    out.finished = s.finished.iter().map(|t| ThreadId(t.0 + K)).collect();
    out.alloc.next_thread += K;
    out.alloc.next_frame += K;
    out.alloc.next_scope += K;
    out.alloc.next_handle += K;
    out
}

fn shift_pn(s: &NetState) -> NetState {
    let mut out = s.clone();
    out.marking = s
        .marking
        .iter()
        .map(|(pid, toks)| {
            let toks = toks
                .iter()
                .map(|t| match t {
                    NetToken::Control { thread, frame } => NetToken::Control {
                        thread: ThreadId(thread.0 + K),
                        frame: FrameId(frame.0 + K),
                    },
                    NetToken::LockWait { thread, frame } => NetToken::LockWait {
                        thread: ThreadId(thread.0 + K),
                        frame: FrameId(frame.0 + K),
                    },
                    NetToken::CondvarWait {
                        thread,
                        frame,
                        lock,
                    } => NetToken::CondvarWait {
                        thread: ThreadId(thread.0 + K),
                        frame: FrameId(frame.0 + K),
                        lock: *lock,
                    },
                    NetToken::SendWait {
                        thread,
                        frame,
                        value,
                    } => NetToken::SendWait {
                        thread: ThreadId(thread.0 + K),
                        frame: FrameId(frame.0 + K),
                        value: value.clone(),
                    },
                    NetToken::RecvWait { thread, frame } => NetToken::RecvWait {
                        thread: ThreadId(thread.0 + K),
                        frame: FrameId(frame.0 + K),
                    },
                    NetToken::SemWait {
                        thread,
                        frame,
                        count,
                    } => NetToken::SemWait {
                        thread: ThreadId(thread.0 + K),
                        frame: FrameId(frame.0 + K),
                        count: *count,
                    },
                    NetToken::JoinWait { thread, frame } => NetToken::JoinWait {
                        thread: ThreadId(thread.0 + K),
                        frame: FrameId(frame.0 + K),
                    },
                    NetToken::ScopeWait { thread, frame } => NetToken::ScopeWait {
                        thread: ThreadId(thread.0 + K),
                        frame: FrameId(frame.0 + K),
                    },
                    NetToken::Mutex(MutexToken::Free) => NetToken::Mutex(MutexToken::Free),
                    NetToken::Mutex(MutexToken::Held(t)) => {
                        NetToken::Mutex(MutexToken::Held(ThreadId(t.0 + K)))
                    }
                    NetToken::Data(v) => NetToken::Data(v.clone()),
                })
                .collect();
            (*pid, toks)
        })
        .collect();
    let store = NetStore {
        frames: s
            .store
            .frames
            .iter()
            .map(|(id, f)| {
                let mut f: NetFrame = f.clone();
                f.id = FrameId(f.id.0 + K);
                f.handles = f
                    .handles
                    .iter()
                    .map(|(k, h)| (k.clone(), HandleId(h.0 + K)))
                    .collect();
                f.ret = f.ret.as_ref().map(|r| NetRetAddr {
                    pc_next: r.pc_next,
                    dst: r.dst,
                });
                (FrameId(id.0 + K), f)
            })
            .collect(),
        threads: s
            .store
            .threads
            .iter()
            .map(|(id, t)| {
                let mut t: NetThread = t.clone();
                t.stack = t.stack.iter().map(|f| FrameId(f.0 + K)).collect();
                t.handle_children = t
                    .handle_children
                    .iter()
                    .map(|(h, c)| (HandleId(h.0 + K), ThreadId(c.0 + K)))
                    .collect();
                t.parent_scope = t.parent_scope.map(|sc| ScopeId(sc.0 + K));
                (ThreadId(id.0 + K), t)
            })
            .collect(),
        scopes: s
            .store
            .scopes
            .iter()
            .map(|(id, sc)| {
                let mut sc: NetScope = sc.clone();
                sc.id = ScopeId(sc.id.0 + K);
                sc.owner = ThreadId(sc.owner.0 + K);
                sc.owner_frame = FrameId(sc.owner_frame.0 + K);
                sc.remaining = sc.remaining.iter().map(|t| ThreadId(t.0 + K)).collect();
                (ScopeId(id.0 + K), sc)
            })
            .collect(),
        finished: s
            .store
            .finished
            .iter()
            .map(|t| ThreadId(t.0 + K))
            .collect(),
        completed_functions: s.store.completed_functions.clone(),
        completed_scopes: s.store.completed_scopes.clone(),
        reached: s.store.reached.clone(),
        alloc: NetAlloc {
            next_thread: s.store.alloc.next_thread + K,
            next_frame: s.store.alloc.next_frame + K,
            next_scope: s.store.alloc.next_scope + K,
            next_handle: s.store.alloc.next_handle + K,
        },
    };
    out.store = store;
    out
}

fn origin_key(o: &concir::sem::outcome::TransitionOrigin) -> String {
    format!("{:?}|{}|{:?}|{:?}", o.module, o.function, o.sid, o.phase)
}

fn it_actions(it: &Interpreter, s: &MachineState) -> BTreeSet<(String, String)> {
    it.successors(s)
        .unwrap()
        .steps
        .iter()
        .map(|st| (origin_key(&st.label.origin), it.state_key(&st.state)))
        .collect()
}

fn pn_actions(pn: &PetriEngine, s: &NetState) -> BTreeSet<(String, String)> {
    pn.successors(s)
        .unwrap()
        .steps
        .iter()
        .map(|st| (origin_key(&st.label.origin), pn.state_key(&st.state)))
        .collect()
}

#[test]
fn c1_identity_translation_preserves_key_predicates_and_successors() {
    // A program with several live threads/frames/handles so identity
    // translation touches every reference class.
    let src = r#"{
      "program": "identity",
      "modules": [{"name": "main",
        "resources": [{"name": "x", "kind": "var", "type": "Var", "base": "Int", "init": 0}],
        "functions": [
          {"name": "main", "kind": "normal", "body": [{"sid":"s1","kind":"scope","funcs":["f","g"]},{"sid":"s2","kind":"return"}]},
          {"name": "f", "kind": "normal", "form": "closure", "body": [
            {"sid":"s1","kind":"spawn","func":"k","handle":"h"},
            {"sid":"s2","kind":"join","handle":"h"},
            {"sid":"s3","kind":"write_shared","resource":"x","expr":"x + 1"},
            {"sid":"s4","kind":"return"}]},
          {"name": "g", "kind": "normal", "form": "closure", "body": [
            {"sid":"s1","kind":"spawn","func":"k","handle":"h"},
            {"sid":"s2","kind":"join","handle":"h"},
            {"sid":"s3","kind":"write_shared","resource":"x","expr":"x + 10"},
            {"sid":"s4","kind":"return"}]},
          {"name": "k", "kind": "normal", "form": "closure", "body": [{"sid":"s1","kind":"return"}]}
        ]}],
      "entry": "main::main"
    }"#;
    let sp = program::lower(&serde_json::from_str(src).unwrap()).unwrap();
    let b = bounds();

    let it = Interpreter::new(&sp, b.clone());
    let r = explore::explore(&it, &b);
    // Pick a state with at least two threads and a non-empty handle table.
    let sample = r
        .states
        .iter()
        .find(|s| s.threads.len() >= 2 && s.store.frames.values().any(|f| !f.handles.is_empty()))
        .expect("a multi-thread state with handles");
    let shifted = shift_it(sample);
    assert_eq!(
        it.state_key(sample),
        it.state_key(&shifted),
        "identity translation must not change the semantic key"
    );
    let preds = [
        Predicate::FunctionCompleted {
            func: sp.function_by_name("main::k").unwrap(),
        },
        Predicate::StatementReached {
            func: sp.function_by_name("main::f").unwrap(),
            sid: 2,
        },
        Predicate::VarEq {
            resource: sp
                .resolve_resource(concir::sem::ids::ModuleId(0), "x")
                .unwrap(),
            value: Value::Int(1),
        },
    ];
    for p in &preds {
        assert_eq!(
            it.satisfied(sample, p),
            it.satisfied(&shifted, p),
            "predicate truth must agree"
        );
    }
    assert_eq!(it_actions(&it, sample), it_actions(&it, &shifted));

    let pn = PetriEngine::new(&sp, b.clone());
    let rp = explore::explore(&pn, &b);
    let sample = rp
        .states
        .iter()
        .find(|s| s.store.threads.len() >= 2)
        .expect("a multi-thread net state");
    let shifted = shift_pn(sample);
    assert_eq!(pn.state_key(sample), pn.state_key(&shifted));
    for p in &preds {
        assert_eq!(pn.satisfied(sample, p), pn.satisfied(&shifted, p));
    }
    assert_eq!(pn_actions(&pn, sample), pn_actions(&pn, &shifted));
}

#[test]
fn c1_quotient_edges_are_executable() {
    // Every stored quotient edge must be realizable by an enabled step of its
    // source whose successor has the stored target's key.
    let corpus = [
        include_str!("../examples/producer_consumer.json"),
        include_str!("../examples/state_machine.json"),
        include_str!("../tests/repro_round4/r3_canonical_collision_A.json"),
    ];
    for src in corpus {
        let sp = program::lower(&serde_json::from_str::<Program>(src).unwrap()).unwrap();
        let b = bounds();
        let it = Interpreter::new(&sp, b.clone());
        let r = explore::explore(&it, &b);
        for (i, edges) in r.edges.iter().enumerate() {
            let en = it.successors(&r.states[i]).unwrap();
            for (t, label) in edges {
                let target_key = it.state_key(&r.states[*t]);
                let ok = en.steps.iter().any(|st| {
                    origin_key(&st.label.origin) == origin_key(&label.origin)
                        && it.state_key(&st.state) == target_key
                });
                assert!(ok, "edge from state {i} is not executable");
            }
        }

        let pn = PetriEngine::new(&sp, b.clone());
        let r = explore::explore(&pn, &b);
        for (i, edges) in r.edges.iter().enumerate() {
            let en = pn.successors(&r.states[i]).unwrap();
            for (t, label) in edges {
                let target_key = pn.state_key(&r.states[*t]);
                let ok = en.steps.iter().any(|st| {
                    origin_key(&st.label.origin) == origin_key(&label.origin)
                        && pn.state_key(&st.state) == target_key
                });
                assert!(ok, "net edge from state {i} is not executable");
            }
        }
    }
}

#[test]
fn c1_raw_oracle_successor_quotient() {
    // Bounded raw State Eq/Hash BFS: states merged into one key must agree on
    // predicate truth and on the set of (action, successor-key) pairs.
    let src = include_str!("../tests/repro_round4/r3_canonical_collision_A.json");
    let sp = program::lower(&serde_json::from_str::<Program>(src).unwrap()).unwrap();
    let it = Interpreter::new(&sp, bounds());
    let k = sp.function_by_name("main::w1").unwrap();
    let pred = Predicate::StatementReached { func: k, sid: 1 };

    let mut seen: HashSet<MachineState> = HashSet::new();
    let mut groups: BTreeMap<String, Vec<MachineState>> = BTreeMap::new();
    let mut q: VecDeque<MachineState> = VecDeque::from([it.initial().unwrap()]);
    while let Some(s) = q.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        if seen.len() > 4096 {
            break;
        }
        groups.entry(it.state_key(&s)).or_default().push(s.clone());
        for st in it.successors(&s).unwrap().steps {
            q.push_back(st.state);
        }
    }
    for (key, members) in &groups {
        if members.len() < 2 {
            continue;
        }
        let truth = it.satisfied(&members[0], &pred);
        let actions = it_actions(&it, &members[0]);
        for m in &members[1..] {
            assert_eq!(it.satisfied(m, &pred), truth, "key {key} predicate mismatch");
            assert_eq!(it_actions(&it, m), actions, "key {key} successor mismatch");
        }
    }
}
