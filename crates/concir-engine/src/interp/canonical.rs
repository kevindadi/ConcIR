use concir_sem::eval::eval;
use concir_sem::ids::{FrameId, FunctionId, HandleId, ResourceId, ScopeId, SlotRef, ThreadId};
use concir_sem::monitor::MonitorConfig;
use concir_sem::outcome::{
    AnalysisBounds, BackendError, BackendResult, BoundaryEvent, BoundaryKind, Phase, StepLabel,
    TransitionOrigin,
};
use concir_sem::program::{SemOp, SemProgram};
use concir_sem::system::{
    compare_values, BlockKind, BlockedRecord, Enabled, Predicate, Step, TransitionSystem,
};
use concir_sem::value::{within_type, Value};

use super::state::{
    BlockReason, FrameView, MachineState, MutexState, PendingSend, RetAddr, RwLockState,
    ScopeState, ThreadState, ThreadStatus,
};


// ───────────────────────── Canonical rendering ─────────────────────────

pub(super) fn render_state(
    program: &SemProgram,
    state: &MachineState,
    enc: &dyn Fn(&Value) -> String,
) -> String {
    let _ = program;
    let thread_order: Vec<ThreadId> = state.threads.keys().copied().collect();
    let frame_order: Vec<FrameId> = state.store.frames.keys().copied().collect();
    let scope_order: Vec<ScopeId> = state.scopes.keys().copied().collect();
    let mut handle_ids: Vec<HandleId> = state
        .store
        .frames
        .values()
        .flat_map(|f| f.handles.values().copied())
        .chain(
            state
                .threads
                .values()
                .flat_map(|t| t.handle_children.keys().copied()),
        )
        .collect();
    handle_ids.sort();
    handle_ids.dedup();

    let tname = |t: ThreadId| -> String {
        thread_order
            .iter()
            .position(|x| *x == t)
            .map(|i| format!("T{i}"))
            .unwrap_or_else(|| format!("T?{}", t.0))
    };
    let fname = |f: FrameId| -> String {
        frame_order
            .iter()
            .position(|x| *x == f)
            .map(|i| format!("F{i}"))
            .unwrap_or_else(|| format!("F?{}", f.0))
    };
    let sname = |s: ScopeId| -> String {
        scope_order
            .iter()
            .position(|x| *x == s)
            .map(|i| format!("S{i}"))
            .unwrap_or_else(|| format!("S?{}", s.0))
    };
    let hname = |h: HandleId| -> String {
        handle_ids
            .iter()
            .position(|x| *x == h)
            .map(|i| format!("h{i}"))
            .unwrap_or_else(|| format!("h?{}", h.0))
    };

    let mut out = String::new();
    out.push_str(
        "STORE
",
    );
    for (r, v) in &state.store.vars {
        out.push_str(&format!("  var r{}={}\n", r.0, enc(v)));
    }
    for (r, v) in &state.store.atomics {
        out.push_str(&format!("  atomic r{}={}\n", r.0, enc(v)));
    }
    for (r, m) in &state.store.mutexes {
        let s = match m {
            MutexState::Free => "free".to_string(),
            MutexState::Held(t) => format!("held({})", tname(*t)),
        };
        out.push_str(&format!("  mutex r{}={}\n", r.0, s));
    }
    for (r, rw) in &state.store.rwlocks {
        let s = match rw {
            RwLockState::Free => "free".to_string(),
            RwLockState::Writing(t) => format!("write({})", tname(*t)),
            RwLockState::Reading(rs) => {
                let mut names: Vec<String> = rs.iter().map(|t| tname(*t)).collect();
                names.sort();
                format!("read([{}])", names.join(","))
            }
        };
        out.push_str(&format!("  rwlock r{}={}\n", r.0, s));
    }
    for (r, n) in &state.store.semaphores {
        out.push_str(&format!("  sem r{}={}\n", r.0, n));
    }
    for (r, c) in &state.store.channels {
        out.push_str(&format!(
            "  chan r{} buf=[{}] send=[{}] recv=[{}]\n",
            r.0,
            c.buffer
                .iter()
                .map(Value::canonical)
                .collect::<Vec<_>>()
                .join(","),
            c.pending_send
                .iter()
                .map(|p| format!("{}:{}", tname(p.thread), enc(&p.value)))
                .collect::<Vec<_>>()
                .join(","),
            c.pending_recv
                .iter()
                .map(|t| tname(*t))
                .collect::<Vec<_>>()
                .join(","),
        ));
    }
    for (r, c) in &state.store.condvars {
        out.push_str(&format!(
            "  condvar r{} waiters=[{}]\n",
            r.0,
            c.waiters
                .iter()
                .map(|(t, l)| format!("{}:{}", tname(*t), l.0))
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    for f in &frame_order {
        let frame = &state.store.frames[f];
        let mut locals: Vec<String> = frame
            .locals
            .iter()
            .map(|(k, v)| format!("{k}={}", enc(v)))
            .collect();
        locals.sort();
        let mut handles: Vec<String> = frame
            .handles
            .iter()
            .map(|(k, h)| format!("{k}->{}", hname(*h)))
            .collect();
        handles.sort();
        let ret = match &frame.ret {
            Some(r) => format!("ret(pc={},dst={:?})", r.pc_next, r.dst),
            None => "ret(none)".to_string(),
        };
        out.push_str(&format!(
            "  frame {} func=f{} pc={} locals={{{}}} handles={{{}}} {}\n",
            fname(*f),
            frame.function.0,
            frame.pc,
            locals.join(","),
            handles.join(","),
            ret
        ));
    }
    out.push_str("THREADS\n");
    for t in &thread_order {
        let th = &state.threads[t];
        let stack: Vec<String> = th.stack.iter().map(|f| fname(*f)).collect();
        let status = match &th.status {
            ThreadStatus::Runnable => "run".to_string(),
            ThreadStatus::Finished => "done".to_string(),
            ThreadStatus::Blocked(b) => {
                format!("blocked:{}", block_reason_text(b, &tname, &hname, &sname))
            }
        };
        let mut kids: Vec<String> = th
            .handle_children
            .iter()
            .map(|(h, c)| format!("{}->{}", hname(*h), tname(*c)))
            .collect();
        kids.sort();
        out.push_str(&format!(
            "  {} status={} stack=[{}] children=[{}] scope={}\n",
            tname(*t),
            status,
            stack.join(","),
            kids.join(","),
            th.parent_scope
                .map(|s| sname(s))
                .unwrap_or_else(|| "-".into())
        ));
    }
    for (r, q) in &state.sem_waiters {
        out.push_str(&format!(
            "  semq r{}=[{}]\n",
            r.0,
            q.iter()
                .map(|(t, n)| format!("{}:{}", tname(*t), n))
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    for s in state.scopes.values() {
        let mut rem: Vec<String> = s.remaining.iter().map(|t| tname(*t)).collect();
        rem.sort();
        out.push_str(&format!(
            "  scope {} owner={} frame={} remaining=[{}]\n",
            sname(s.id),
            tname(s.owner),
            fname(s.owner_frame),
            rem.join(",")
        ));
    }
    out.push_str(&format!(
        "COMPLETED funcs={:?} scopes={:?}\n",
        state.completed_functions, state.completed_scopes
    ));
    out.push_str(&format!("REACHED {:?}\n", state.reached));
    out
}

fn block_reason_text(
    b: &BlockReason,
    _tname: &impl Fn(ThreadId) -> String,
    hname: &impl Fn(HandleId) -> String,
    sname: &impl Fn(ScopeId) -> String,
) -> String {
    match b {
        BlockReason::Lock(r) => format!("lock:{}", r.0),
        BlockReason::RwLockRead(r) => format!("rwread:{}", r.0),
        BlockReason::RwLockWrite(r) => format!("rwwrite:{}", r.0),
        BlockReason::ChannelSend(r) => format!("send:{}", r.0),
        BlockReason::ChannelRecv(r) => format!("recv:{}", r.0),
        BlockReason::Condvar(cv, lk) => format!("condvar:{}:{}", cv.0, lk.0),
        BlockReason::Semaphore(r) => format!("sem:{}", r.0),
        BlockReason::Join(h) => format!("join:{}", hname(*h)),
        BlockReason::Scope(s) => format!("scope:{}", sname(*s)),
    }
}
