//! Colored Petri-net core and CIR translation.
//!
//! Places are explicit; tokens carry control/thread bindings and colored data.
//! Transition templates carry an input arc (via [`Binding`]) and an output
//! successor place (`next`) plus an op-specific update. The mutable `store`
//! (frames, thread bookkeeping, completion facts) is part of the complete
//! state: it participates in enabling, atomic update, serialization, equality,
//! and dedup.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::sem::eval::LExpr;
use crate::sem::ids::{
    FrameId, FunctionId, HandleId, ResourceId, ScopeId, SlotId, SlotRef, ThreadId,
};
use crate::sem::outcome::Phase;
use crate::sem::program::{ResKind, SemFunction, SemOp, SemProgram, SlotClass};
use crate::sem::value::{default_value, Value};

pub type PlaceId = u32;
pub type TransitionId = u32;

/// Static identity of a place. Places are created on demand during build.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum PlaceKey {
    Control {
        function: FunctionId,
        sid: usize,
    },
    Var(ResourceId),
    Atomic(ResourceId),
    Mutex(ResourceId),
    Semaphore(ResourceId),
    /// Single colored token holding the buffered message queue.
    Channel(ResourceId),
    ChannelSend(ResourceId),
    ChannelRecv(ResourceId),
    Condvar(ResourceId),
    LockWait(ResourceId),
    SemWait(ResourceId),
    JoinWait {
        function: FunctionId,
        sid: usize,
    },
    ScopeWait {
        function: FunctionId,
        sid: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NetToken {
    Control {
        thread: ThreadId,
        frame: FrameId,
    },
    Data(Value),
    Mutex(MutexToken),
    LockWait {
        thread: ThreadId,
        frame: FrameId,
    },
    CondvarWait {
        thread: ThreadId,
        frame: FrameId,
        lock: ResourceId,
    },
    SendWait {
        thread: ThreadId,
        frame: FrameId,
        value: Value,
    },
    RecvWait {
        thread: ThreadId,
        frame: FrameId,
    },
    SemWait {
        thread: ThreadId,
        frame: FrameId,
        count: i64,
    },
    JoinWait {
        thread: ThreadId,
        frame: FrameId,
    },
    ScopeWait {
        thread: ThreadId,
        frame: FrameId,
    },
}

impl NetToken {
    pub fn control(thread: ThreadId, frame: FrameId) -> Self {
        NetToken::Control { thread, frame }
    }

    pub fn thread(&self) -> Option<ThreadId> {
        Some(match self {
            NetToken::Control { thread, .. }
            | NetToken::LockWait { thread, .. }
            | NetToken::CondvarWait { thread, .. }
            | NetToken::SendWait { thread, .. }
            | NetToken::RecvWait { thread, .. }
            | NetToken::SemWait { thread, .. }
            | NetToken::JoinWait { thread, .. }
            | NetToken::ScopeWait { thread, .. } => *thread,
            _ => return None,
        })
    }

    pub fn frame(&self) -> Option<FrameId> {
        Some(match self {
            NetToken::Control { frame, .. }
            | NetToken::LockWait { frame, .. }
            | NetToken::CondvarWait { frame, .. }
            | NetToken::SendWait { frame, .. }
            | NetToken::RecvWait { frame, .. }
            | NetToken::SemWait { frame, .. }
            | NetToken::JoinWait { frame, .. }
            | NetToken::ScopeWait { frame, .. } => *frame,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MutexToken {
    Free,
    Held(ThreadId),
}

#[derive(Debug, Clone, Serialize)]
pub struct Place {
    pub id: PlaceId,
    pub key: PlaceKey,
}

/// Input arc: how a transition is bound to tokens when enumerating firings.
#[derive(Debug, Clone)]
pub enum Binding {
    /// Enumerate control tokens on one place.
    Control(PlaceId),
    /// Enumerate wait tokens on one place (front-first).
    Wait(PlaceId),
    /// Pair the front tokens of two wait places (rendezvous).
    Pair(PlaceId, PlaceId),
    /// Pair a control token with the front wait token of another place.
    ControlWait(PlaceId, PlaceId),
}

#[derive(Debug, Clone)]
pub enum NetOp {
    Nop,
    AssignLocal {
        target: SlotRef,
        expr: LExpr,
    },
    ReadVar {
        resource: ResourceId,
        dst: Option<SlotRef>,
    },
    WriteVar {
        resource: ResourceId,
        expr: LExpr,
    },
    AtomicLoad {
        resource: ResourceId,
        dst: SlotRef,
    },
    AtomicStore {
        resource: ResourceId,
        expr: LExpr,
    },
    AtomicCas {
        resource: ResourceId,
        expected: LExpr,
        desired: LExpr,
        dst: SlotRef,
    },
    MutexLockGrant {
        resource: ResourceId,
    },
    MutexLockBlock {
        resource: ResourceId,
    },
    MutexUnlock {
        resource: ResourceId,
    },
    LockWaitAcquire {
        resource: ResourceId,
    },
    CondvarWait {
        condvar: ResourceId,
        lock: ResourceId,
    },
    CondvarNotifyHit {
        condvar: ResourceId,
        lock: ResourceId,
    },
    CondvarNotifyMiss {
        condvar: ResourceId,
    },
    CondvarNotifyAll {
        condvar: ResourceId,
        lock: ResourceId,
    },
    SemAcquire {
        resource: ResourceId,
        count: i64,
    },
    SemAcquireBlock {
        resource: ResourceId,
        count: i64,
    },
    SemRelease {
        resource: ResourceId,
        count: i64,
    },
    SemGrant {
        resource: ResourceId,
    },
    SendRegister {
        channel: ResourceId,
    },
    RecvRegister {
        channel: ResourceId,
    },
    SendPair {
        channel: ResourceId,
    },
    RecvPair {
        channel: ResourceId,
    },
    Rendezvous {
        channel: ResourceId,
    },
    SendBuf {
        channel: ResourceId,
        expr: LExpr,
    },
    SendBufBlock {
        channel: ResourceId,
        expr: LExpr,
    },
    RecvBuf {
        channel: ResourceId,
        dst: SlotRef,
    },
    RecvBufBlock {
        channel: ResourceId,
    },
    BufferDeliver {
        channel: ResourceId,
    },
    BufferRecvWait {
        channel: ResourceId,
    },
    Goto {
        target: usize,
    },
    BranchThen {
        cond: LExpr,
        target: usize,
    },
    BranchElse {
        cond: LExpr,
        target: usize,
    },
    SwitchCase {
        var: LExpr,
        label: String,
        target: usize,
    },
    SwitchDefault {
        var: LExpr,
        labels: Vec<String>,
        target: usize,
    },
    Call {
        func: FunctionId,
        args: Vec<LExpr>,
        dst: Option<SlotRef>,
    },
    Spawn {
        func: FunctionId,
        handle: String,
    },
    Scope {
        funcs: Vec<FunctionId>,
    },
    JoinReady {
        handle: String,
    },
    JoinBlock {
        handle: String,
    },
    ReturnInner {
        value: Option<LExpr>,
    },
    ReturnFinal {
        value: Option<LExpr>,
    },
}

#[derive(Debug, Clone)]
pub struct Transition {
    pub id: TransitionId,
    pub op: NetOp,
    pub binding: Binding,
    /// Successor control place for transitions that move a control token.
    pub next: Option<PlaceId>,
    /// Resource/wait output places (documentation of output arcs).
    pub outputs: Vec<PlaceId>,
    pub origin: crate::sem::outcome::TransitionOrigin,
}

#[derive(Debug, Clone)]
pub struct PetriNet {
    pub places: Vec<Place>,
    pub place_of: BTreeMap<PlaceKey, PlaceId>,
    pub transitions: Vec<Transition>,
    pub entry: FunctionId,
    /// Condvar → associated mutex, learned from `condvar_wait` sites.
    pub condvar_lock: BTreeMap<ResourceId, ResourceId>,
}

// ─────────────────────────── Net state ───────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NetFrame {
    pub id: FrameId,
    pub function: FunctionId,
    pub pc: usize,
    pub locals: BTreeMap<SlotId, Value>,
    pub ret: Option<NetRetAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NetRetAddr {
    pub pc_next: usize,
    pub dst: SlotRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NetScope {
    pub id: ScopeId,
    pub owner: ThreadId,
    pub owner_frame: FrameId,
    pub owner_sid: usize,
    pub remaining: BTreeSet<ThreadId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NetThread {
    pub entry_function: FunctionId,
    pub stack: Vec<FrameId>,
    pub handles: BTreeMap<String, HandleId>,
    pub handle_children: BTreeMap<HandleId, ThreadId>,
    pub parent_scope: Option<ScopeId>,
    /// Static key of the place the thread is currently waiting in, if any.
    pub blocked_at: Option<PlaceKey>,
}

#[derive(Debug, Clone, Default)]
pub struct NetAlloc {
    pub next_thread: u64,
    pub next_frame: u64,
    pub next_scope: u64,
    pub next_handle: u64,
}

impl PartialEq for NetAlloc {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}
impl Eq for NetAlloc {}
impl std::hash::Hash for NetAlloc {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        0u8.hash(state);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NetStore {
    pub frames: BTreeMap<FrameId, NetFrame>,
    pub threads: BTreeMap<ThreadId, NetThread>,
    pub scopes: BTreeMap<ScopeId, NetScope>,
    pub finished: BTreeSet<ThreadId>,
    pub completed_functions: BTreeMap<FunctionId, usize>,
    pub completed_scopes: BTreeSet<(FunctionId, usize)>,
    pub reached: BTreeSet<(FunctionId, usize)>,
    pub alloc: NetAlloc,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NetState {
    pub marking: BTreeMap<PlaceId, Vec<NetToken>>,
    pub store: NetStore,
}

impl NetStore {
    pub fn current_frame(&self, tid: ThreadId) -> Option<FrameId> {
        self.threads.get(&tid).and_then(|t| t.stack.last().copied())
    }

    pub fn frame(&self, id: FrameId) -> &NetFrame {
        &self.frames[&id]
    }

    pub fn frame_mut(&mut self, id: FrameId) -> &mut NetFrame {
        self.frames.get_mut(&id).expect("frame exists")
    }
}

impl NetState {
    pub fn place_tokens(&self, pid: PlaceId) -> &[NetToken] {
        self.marking.get(&pid).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn put(&mut self, pid: PlaceId, t: NetToken) {
        self.marking.entry(pid).or_default().push(t);
    }

    pub fn take_first(&mut self, pid: PlaceId) -> Option<NetToken> {
        let v = self.marking.get_mut(&pid)?;
        if v.is_empty() {
            None
        } else {
            Some(v.remove(0))
        }
    }

    pub fn take_token(&mut self, pid: PlaceId, t: &NetToken) -> bool {
        if let Some(v) = self.marking.get_mut(&pid) {
            if let Some(pos) = v.iter().position(|x| x == t) {
                v.remove(pos);
                return true;
            }
        }
        false
    }

    pub fn set_single(&mut self, pid: PlaceId, t: NetToken) {
        self.marking.insert(pid, vec![t]);
    }

    pub fn single(&self, pid: PlaceId) -> Option<&NetToken> {
        self.marking.get(&pid).and_then(|v| v.first())
    }

    pub fn read_data(&self, pid: PlaceId) -> Option<&Value> {
        match self.single(pid) {
            Some(NetToken::Data(v)) => Some(v),
            _ => None,
        }
    }

    pub fn set_data(&mut self, pid: PlaceId, v: Value) {
        self.set_single(pid, NetToken::Data(v));
    }

    pub fn read_mutex(&self, pid: PlaceId) -> Option<MutexToken> {
        match self.single(pid) {
            Some(NetToken::Mutex(m)) => Some(m.clone()),
            _ => None,
        }
    }

    pub fn set_mutex(&mut self, pid: PlaceId, m: MutexToken) {
        self.set_single(pid, NetToken::Mutex(m));
    }
}

// ───────────────────────────── Build ─────────────────────────────

struct Builder<'a> {
    program: &'a SemProgram,
    places: Vec<Place>,
    place_of: BTreeMap<PlaceKey, PlaceId>,
    transitions: Vec<Transition>,
    condvar_lock: BTreeMap<ResourceId, ResourceId>,
}

impl<'a> Builder<'a> {
    fn place(&mut self, key: PlaceKey) -> PlaceId {
        if let Some(&id) = self.place_of.get(&key) {
            return id;
        }
        let id = self.places.len() as PlaceId;
        self.place_of.insert(key.clone(), id);
        self.places.push(Place { id, key });
        id
    }

    fn add(
        &mut self,
        op: NetOp,
        binding: Binding,
        next: Option<PlaceId>,
        outputs: Vec<PlaceId>,
        origin: crate::sem::outcome::TransitionOrigin,
    ) {
        let id = self.transitions.len() as TransitionId;
        self.transitions.push(Transition {
            id,
            op,
            binding,
            next,
            outputs,
            origin,
        });
    }

    fn ctrl(&mut self, f: FunctionId, sid: usize) -> PlaceId {
        self.place(PlaceKey::Control { function: f, sid })
    }

    fn origin(
        &self,
        function: FunctionId,
        sid: usize,
        phase: Phase,
    ) -> crate::sem::outcome::TransitionOrigin {
        let f = self.program.function(function);
        crate::sem::outcome::TransitionOrigin {
            module: f.module,
            function,
            sid: Some(sid),
            phase,
        }
    }
}

pub fn build(program: &SemProgram) -> PetriNet {
    let mut b = Builder {
        program,
        places: Vec::new(),
        place_of: BTreeMap::new(),
        transitions: Vec::new(),
        condvar_lock: BTreeMap::new(),
    };

    // Learn condvar → mutex associations from wait sites.
    for f in program.functions() {
        for stmt in &f.body {
            if let SemOp::CondvarWait { condvar, lock } = &stmt.op {
                b.condvar_lock.insert(*condvar, *lock);
            }
        }
    }
    let condvar_lock = b.condvar_lock.clone();

    for f in program.functions() {
        for (si, stmt) in f.body.iter().enumerate() {
            build_stmt(&mut b, f, si, stmt);
        }
    }

    let resource_ids: Vec<ResourceId> = program.resources().iter().map(|r| r.id).collect();
    for rid in resource_ids {
        match program.resource(rid).kind {
            ResKind::Mutex => {
                b.place(PlaceKey::LockWait(rid));
                b.place(PlaceKey::Mutex(rid));
            }
            ResKind::Semaphore => {
                b.place(PlaceKey::SemWait(rid));
                b.place(PlaceKey::Semaphore(rid));
            }
            ResKind::Channel => {
                b.place(PlaceKey::Channel(rid));
                b.place(PlaceKey::ChannelSend(rid));
                b.place(PlaceKey::ChannelRecv(rid));
            }
            _ => {}
        }
    }

    PetriNet {
        places: b.places,
        place_of: b.place_of,
        transitions: b.transitions,
        entry: program.entry(),
        condvar_lock,
    }
}

fn build_stmt(b: &mut Builder, f: &SemFunction, si: usize, stmt: &crate::sem::program::SemStmt) {
    let fid = f.id;
    let input = b.ctrl(fid, si);
    let next_sid = si + 1;
    let fall = b.ctrl(fid, next_sid);
    let origin = b.origin(fid, si, Phase::Statement);
    match &stmt.op {
        SemOp::Nop => b.add(NetOp::Nop, Binding::Control(input), Some(fall), vec![fall], origin),
        SemOp::AssignLocal { target, expr } => b.add(
            NetOp::AssignLocal {
                target: *target,
                expr: expr.clone(),
            },
            Binding::Control(input),
            Some(fall),
            vec![fall],
            origin,
        ),
        SemOp::ReadShared { resource, dst } => {
            let var = b.place(PlaceKey::Var(*resource));
            b.add(
                NetOp::ReadVar {
                    resource: *resource,
                    dst: *dst,
                },
                Binding::Control(input),
                Some(fall),
                vec![fall, var],
                origin,
            );
        }
        SemOp::WriteShared { resource, expr } => {
            let var = b.place(PlaceKey::Var(*resource));
            b.add(
                NetOp::WriteVar {
                    resource: *resource,
                    expr: expr.clone(),
                },
                Binding::Control(input),
                Some(fall),
                vec![fall, var],
                origin,
            );
        }
        SemOp::AtomicLoad { resource, dst } => {
            let at = b.place(PlaceKey::Atomic(*resource));
            b.add(
                NetOp::AtomicLoad {
                    resource: *resource,
                    dst: *dst,
                },
                Binding::Control(input),
                Some(fall),
                vec![fall, at],
                origin,
            );
        }
        SemOp::AtomicStore { resource, value } => {
            let at = b.place(PlaceKey::Atomic(*resource));
            b.add(
                NetOp::AtomicStore {
                    resource: *resource,
                    expr: value.clone(),
                },
                Binding::Control(input),
                Some(fall),
                vec![fall, at],
                origin,
            );
        }
        SemOp::AtomicCas {
            resource,
            expected,
            desired,
            dst,
        } => {
            let at = b.place(PlaceKey::Atomic(*resource));
            b.add(
                NetOp::AtomicCas {
                    resource: *resource,
                    expected: expected.clone(),
                    desired: desired.clone(),
                    dst: *dst,
                },
                Binding::Control(input),
                Some(fall),
                vec![fall, at],
                origin,
            );
        }
        SemOp::MutexLock { resource } => {
            let mtx = b.place(PlaceKey::Mutex(*resource));
            let lw = b.place(PlaceKey::LockWait(*resource));
            b.add(
                NetOp::MutexLockGrant { resource: *resource },
                Binding::Control(input),
                Some(fall),
                vec![fall, mtx],
                origin.clone(),
            );
            b.add(
                NetOp::MutexLockBlock { resource: *resource },
                Binding::Control(input),
                Some(lw),
                vec![lw, mtx],
                origin,
            );
        }
        SemOp::MutexUnlock { resource } => {
            let mtx = b.place(PlaceKey::Mutex(*resource));
            b.add(
                NetOp::MutexUnlock { resource: *resource },
                Binding::Control(input),
                Some(fall),
                vec![fall, mtx],
                origin,
            );
        }
        SemOp::ChannelSend { channel, value } => {
            let cap = b.program.resource(*channel).capacity;
            b.place(PlaceKey::Channel(*channel));
            if cap == 0 {
                let send = b.place(PlaceKey::ChannelSend(*channel));
                let recv = b.place(PlaceKey::ChannelRecv(*channel));
                b.add(
                    NetOp::SendRegister { channel: *channel },
                    Binding::Control(input),
                    Some(send),
                    vec![send],
                    origin.clone(),
                );
                b.add(
                    NetOp::SendPair { channel: *channel },
                    Binding::ControlWait(input, recv),
                    Some(fall),
                    vec![fall, send],
                    origin,
                );
            } else {
                b.add(
                    NetOp::SendBuf {
                        channel: *channel,
                        expr: value.clone(),
                    },
                    Binding::Control(input),
                    Some(fall),
                    vec![fall],
                    origin.clone(),
                );
                let send = b.place(PlaceKey::ChannelSend(*channel));
                b.add(
                    NetOp::SendBufBlock {
                        channel: *channel,
                        expr: value.clone(),
                    },
                    Binding::Control(input),
                    Some(send),
                    vec![send],
                    origin,
                );
            }
        }
        SemOp::ChannelRecv { channel, dst } => {
            let cap = b.program.resource(*channel).capacity;
            b.place(PlaceKey::Channel(*channel));
            if cap == 0 {
                let send = b.place(PlaceKey::ChannelSend(*channel));
                let recv = b.place(PlaceKey::ChannelRecv(*channel));
                b.add(
                    NetOp::RecvRegister { channel: *channel },
                    Binding::Control(input),
                    Some(recv),
                    vec![recv],
                    origin.clone(),
                );
                b.add(
                    NetOp::RecvPair { channel: *channel },
                    Binding::ControlWait(input, send),
                    Some(fall),
                    vec![fall, recv],
                    origin,
                );
            } else {
                b.add(
                    NetOp::RecvBuf {
                        channel: *channel,
                        dst: *dst,
                    },
                    Binding::Control(input),
                    Some(fall),
                    vec![fall],
                    origin.clone(),
                );
                let recv = b.place(PlaceKey::ChannelRecv(*channel));
                b.add(
                    NetOp::RecvBufBlock { channel: *channel },
                    Binding::Control(input),
                    Some(recv),
                    vec![recv],
                    origin,
                );
            }
        }
        SemOp::CondvarWait { condvar, lock } => {
            let cv = b.place(PlaceKey::Condvar(*condvar));
            let mtx = b.place(PlaceKey::Mutex(*lock));
            b.add(
                NetOp::CondvarWait {
                    condvar: *condvar,
                    lock: *lock,
                },
                Binding::Control(input),
                Some(cv),
                vec![cv, mtx],
                origin,
            );
        }
        SemOp::CondvarNotify { condvar } => {
            let cv = b.place(PlaceKey::Condvar(*condvar));
            let lock_opt = b.condvar_lock.get(condvar).copied();
            if let Some(lock) = lock_opt {
                let mtx = b.place(PlaceKey::Mutex(lock));
                let lw = b.place(PlaceKey::LockWait(lock));
                b.add(
                    NetOp::CondvarNotifyHit { condvar: *condvar, lock },
                    Binding::Control(input),
                    Some(fall),
                    vec![fall, cv, lw, mtx],
                    origin.clone(),
                );
            }
            b.add(
                NetOp::CondvarNotifyMiss { condvar: *condvar },
                Binding::Control(input),
                Some(fall),
                vec![fall, cv],
                origin,
            );
        }
        SemOp::CondvarNotifyAll { condvar } => {
            let cv = b.place(PlaceKey::Condvar(*condvar));
            let lock_opt = b.condvar_lock.get(condvar).copied();
            if let Some(lock) = lock_opt {
                let mtx = b.place(PlaceKey::Mutex(lock));
                let lw = b.place(PlaceKey::LockWait(lock));
                b.add(
                    NetOp::CondvarNotifyAll {
                        condvar: *condvar,
                        lock,
                    },
                    Binding::Control(input),
                    Some(fall),
                    vec![fall, cv, lw, mtx],
                    origin,
                );
            }
        }
        SemOp::SemaphoreAcquire { resource, count } => {
            let sem = b.place(PlaceKey::Semaphore(*resource));
            let sw = b.place(PlaceKey::SemWait(*resource));
            b.add(
                NetOp::SemAcquire {
                    resource: *resource,
                    count: *count,
                },
                Binding::Control(input),
                Some(fall),
                vec![fall, sem],
                origin.clone(),
            );
            b.add(
                NetOp::SemAcquireBlock {
                    resource: *resource,
                    count: *count,
                },
                Binding::Control(input),
                Some(sw),
                vec![sw, sem],
                origin,
            );
        }
        SemOp::SemaphoreRelease { resource, count } => {
            let sem = b.place(PlaceKey::Semaphore(*resource));
            b.add(
                NetOp::SemRelease {
                    resource: *resource,
                    count: *count,
                },
                Binding::Control(input),
                Some(fall),
                vec![fall, sem],
                origin,
            );
        }
        SemOp::Call { func, args, dst } => {
            let callee = b.ctrl(*func, 0);
            b.add(
                NetOp::Call {
                    func: *func,
                    args: args.clone(),
                    dst: *dst,
                },
                Binding::Control(input),
                Some(callee),
                vec![callee],
                origin,
            );
        }
        SemOp::Spawn { func, handle } => {
            let child = b.ctrl(*func, 0);
            b.add(
                NetOp::Spawn {
                    func: *func,
                    handle: handle.clone(),
                },
                Binding::Control(input),
                Some(fall),
                vec![fall, child],
                origin,
            );
        }
        SemOp::Scope { funcs } => {
            let sw = b.place(PlaceKey::ScopeWait { function: fid, sid: si });
            let mut outputs = vec![sw];
            for func in funcs {
                outputs.push(b.ctrl(*func, 0));
            }
            b.add(
                NetOp::Scope { funcs: funcs.clone() },
                Binding::Control(input),
                Some(fall),
                outputs,
                origin,
            );
        }
        SemOp::Join { handle } => {
            let jw = b.place(PlaceKey::JoinWait { function: fid, sid: si });
            b.add(
                NetOp::JoinReady {
                    handle: handle.clone(),
                },
                Binding::Control(input),
                Some(fall),
                vec![fall],
                origin.clone(),
            );
            b.add(
                NetOp::JoinBlock {
                    handle: handle.clone(),
                },
                Binding::Control(input),
                Some(jw),
                vec![jw],
                origin,
            );
        }
        SemOp::Goto { target } => {
            let out = b.ctrl(fid, *target);
            b.add(
                NetOp::Goto { target: *target },
                Binding::Control(input),
                Some(out),
                vec![out],
                origin,
            );
        }
        SemOp::Branch {
            cond,
            then,
            else_target,
        } => {
            let then_p = b.ctrl(fid, *then);
            let else_p = b.ctrl(fid, *else_target);
            b.add(
                NetOp::BranchThen {
                    cond: cond.clone(),
                    target: *then,
                },
                Binding::Control(input),
                Some(then_p),
                vec![then_p],
                origin.clone(),
            );
            b.add(
                NetOp::BranchElse {
                    cond: cond.clone(),
                    target: *else_target,
                },
                Binding::Control(input),
                Some(else_p),
                vec![else_p],
                origin,
            );
        }
        SemOp::Switch {
            var,
            cases,
            default,
        } => {
            for (label, target) in cases {
                let out = b.ctrl(fid, *target);
                b.add(
                    NetOp::SwitchCase {
                        var: var.clone(),
                        label: label.clone(),
                        target: *target,
                    },
                    Binding::Control(input),
                    Some(out),
                    vec![out],
                    origin.clone(),
                );
            }
            let def = b.ctrl(fid, *default);
            b.add(
                NetOp::SwitchDefault {
                    var: var.clone(),
                    labels: cases.keys().cloned().collect(),
                    target: *default,
                },
                Binding::Control(input),
                Some(def),
                vec![def],
                origin,
            );
        }
        SemOp::Return { value } => {
            b.add(
                NetOp::ReturnInner {
                    value: value.clone(),
                },
                Binding::Control(input),
                None,
                Vec::new(),
                origin.clone(),
            );
            b.add(
                NetOp::ReturnFinal {
                    value: value.clone(),
                },
                Binding::Control(input),
                None,
                Vec::new(),
                origin,
            );
        }
        SemOp::Unsupported { .. } => {}
    }
}

/// Control places for a function (inspection helper).
pub fn control_places(net: &PetriNet, function: FunctionId) -> Vec<PlaceId> {
    net.places
        .iter()
        .filter(|p| matches!(&p.key, PlaceKey::Control { function: f, .. } if *f == function))
        .map(|p| p.id)
        .collect()
}

/// Store view for expression evaluation inside the net engine.
pub struct NetFrameView<'a> {
    pub frame: &'a NetFrame,
    pub state: &'a NetState,
    pub net: &'a PetriNet,
}

impl crate::sem::eval::ValueStore for NetFrameView<'_> {
    fn get_slot(&self, slot: usize) -> Option<&Value> {
        self.frame.locals.get(&slot)
    }

    fn get_shared(&self, resource: ResourceId) -> Option<&Value> {
        let key = if self.net.place_of.contains_key(&PlaceKey::Var(resource)) {
            PlaceKey::Var(resource)
        } else {
            PlaceKey::Atomic(resource)
        };
        let pid = *self.net.place_of.get(&key)?;
        self.state.read_data(pid)
    }
}

/// Initial locals for a function.
pub fn default_locals(program: &SemProgram, function: FunctionId) -> BTreeMap<SlotId, Value> {
    let f = program.function(function);
    f.slots
        .iter()
        .enumerate()
        .map(|(i, s)| (i, s.init.clone().unwrap_or_else(|| default_value(&s.ty))))
        .collect()
}

/// Modeled parameter slot indices of a function.
pub fn modeled_params(f: &SemFunction) -> Vec<SlotId> {
    f.slots
        .iter()
        .enumerate()
        .filter(|(_, s)| s.class == SlotClass::Param && s.modeled)
        .map(|(i, _)| i)
        .collect()
}
