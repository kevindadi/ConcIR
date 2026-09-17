//! Machine state for the reference interpreter.
//!
//! The state is a complete semantic state: shared store, per-frame activation
//! data (including that frame's spawn/join handle bindings), thread stacks and
//! statuses, wait data, and durable completion facts. Identity allocation
//! counters travel with the state but are excluded from equality/dedup (they
//! affect only internal identities).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::hash::{Hash, Hasher};

use crate::sem::eval::ValueStore;
use crate::sem::ids::{FrameId, FunctionId, HandleId, ResourceId, ScopeId, SlotRef, ThreadId};
use crate::sem::outcome::BackendResult;
use crate::sem::program::SemProgram;
use crate::sem::value::{default_value, Value};

// ─────────────────────────── Store ───────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MutexState {
    Free,
    Held(ThreadId),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PendingSend {
    pub thread: ThreadId,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ChannelState {
    pub buffer: VecDeque<Value>,
    pub pending_send: VecDeque<PendingSend>,
    pub pending_recv: VecDeque<ThreadId>,
}

/// A condvar wait set. Each waiter carries the mutex it must re-acquire, so
/// different waiters may be associated with different locks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct CondvarState {
    pub waiters: VecDeque<(ThreadId, ResourceId)>,
}

/// A dynamic activation. Handle name → child bindings belong to the frame, so
/// a callee cannot clobber its caller's names.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Frame {
    pub id: FrameId,
    pub function: FunctionId,
    pub pc: usize,
    pub locals: BTreeMap<usize, Value>,
    pub handles: BTreeMap<String, HandleId>,
    pub ret: Option<RetAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RetAddr {
    pub pc_next: usize,
    pub dst: SlotRef,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Store {
    pub vars: BTreeMap<ResourceId, Value>,
    pub atomics: BTreeMap<ResourceId, Value>,
    pub mutexes: BTreeMap<ResourceId, MutexState>,
    pub semaphores: BTreeMap<ResourceId, i64>,
    pub channels: BTreeMap<ResourceId, ChannelState>,
    pub condvars: BTreeMap<ResourceId, CondvarState>,
    pub frames: BTreeMap<FrameId, Frame>,
}

impl Store {
    pub fn read_shared(&self, r: ResourceId) -> Option<&Value> {
        self.vars.get(&r).or_else(|| self.atomics.get(&r))
    }

    pub fn write_shared(&mut self, r: ResourceId, v: Value) {
        if self.vars.contains_key(&r) {
            self.vars.insert(r, v);
        } else {
            self.atomics.insert(r, v);
        }
    }

    pub fn read_frame(&self, id: FrameId) -> Option<&Frame> {
        self.frames.get(&id)
    }

    pub fn write_frame(&mut self, id: FrameId, f: Frame) {
        self.frames.insert(id, f);
    }
}

// ─────────────────────── Threads / scopes ────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BlockReason {
    Lock(ResourceId),
    ChannelSend(ResourceId),
    ChannelRecv(ResourceId),
    Condvar(ResourceId, ResourceId),
    Semaphore(ResourceId),
    Join(HandleId),
    Scope(ScopeId),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ThreadStatus {
    Runnable,
    Blocked(BlockReason),
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThreadState {
    pub id: ThreadId,
    pub status: ThreadStatus,
    pub stack: Vec<FrameId>,
    /// Function this thread was created to run.
    pub entry_function: FunctionId,
    /// Child threads this activation created, keyed by unique handle id. The
    /// *names* live on frames; this map only stores concrete child identity.
    pub handle_children: BTreeMap<HandleId, ThreadId>,
    /// Set when this thread was spawned by a `scope` statement.
    pub parent_scope: Option<ScopeId>,
}

impl ThreadState {
    pub fn current_frame(&self) -> Option<FrameId> {
        self.stack.last().copied()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScopeState {
    pub id: ScopeId,
    pub owner: ThreadId,
    pub owner_frame: FrameId,
    pub remaining: BTreeSet<ThreadId>,
}

// ───────────────────────── Allocation ────────────────────────

/// Identity counters. They travel with the state but are intentionally
/// excluded from equality/hash: only the relative order of identities matters
/// for behavior, not their absolute values.
#[derive(Debug, Clone, Default)]
pub struct Alloc {
    pub next_thread: u64,
    pub next_frame: u64,
    pub next_scope: u64,
    pub next_handle: u64,
}

impl PartialEq for Alloc {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}
impl Eq for Alloc {}
impl Hash for Alloc {
    fn hash<H: Hasher>(&self, state: &mut H) {
        0u8.hash(state);
    }
}

// ─────────────────────── Machine state ───────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MachineState {
    pub store: Store,
    pub threads: BTreeMap<ThreadId, ThreadState>,
    /// Requested permit counts of threads blocked on a semaphore.
    pub sem_waiters: BTreeMap<ResourceId, VecDeque<(ThreadId, i64)>>,
    pub scopes: BTreeMap<ScopeId, ScopeState>,
    pub finished: BTreeSet<ThreadId>,
    pub completed_functions: BTreeMap<FunctionId, usize>,
    pub completed_scopes: BTreeSet<(FunctionId, usize)>,
    pub reached: BTreeSet<(FunctionId, usize)>,
    pub alloc: Alloc,
}

/// A view of one frame plus the shared store for expression evaluation.
pub struct FrameView<'a> {
    pub frame: &'a Frame,
    pub store: &'a Store,
}

impl ValueStore for FrameView<'_> {
    fn get_slot(&self, slot: usize) -> Option<&Value> {
        self.frame.locals.get(&slot)
    }

    fn get_shared(&self, resource: ResourceId) -> Option<&Value> {
        self.store.read_shared(resource)
    }
}

impl MachineState {
    /// Build the initial state: one runnable thread executing the entry.
    pub fn initial(program: &SemProgram) -> BackendResult<MachineState> {
        let mut state = MachineState {
            store: Store::default(),
            threads: BTreeMap::new(),
            sem_waiters: BTreeMap::new(),
            scopes: BTreeMap::new(),
            finished: BTreeSet::new(),
            completed_functions: BTreeMap::new(),
            completed_scopes: BTreeSet::new(),
            reached: BTreeSet::new(),
            alloc: Alloc::default(),
        };
        state.init_resources(program);
        let tid = ThreadId(state.alloc.next_thread);
        state.alloc.next_thread += 1;
        let entry = program.entry();
        if program.function(entry).is_transparent_nobody() {
            state.threads.insert(
                tid,
                ThreadState {
                    id: tid,
                    status: ThreadStatus::Finished,
                    stack: Vec::new(),
                    entry_function: entry,
                    handle_children: BTreeMap::new(),
                    parent_scope: None,
                },
            );
            state.finished.insert(tid);
            state.completed_functions.insert(entry, 1);
            return Ok(state);
        }
        let frame = state.alloc_frame(program, entry)?;
        let fid = frame.id;
        state.store.write_frame(fid, frame);
        state.threads.insert(
            tid,
            ThreadState {
                id: tid,
                status: ThreadStatus::Runnable,
                stack: vec![fid],
                entry_function: entry,
                handle_children: BTreeMap::new(),
                parent_scope: None,
            },
        );
        Ok(state)
    }

    fn init_resources(&mut self, program: &SemProgram) {
        for r in program.resources() {
            match r.kind {
                crate::sem::program::ResKind::Var => {
                    if let Some(v) = &r.init {
                        self.store.vars.insert(r.id, v.clone());
                    }
                }
                crate::sem::program::ResKind::Atomic => {
                    if let Some(v) = &r.init {
                        self.store.atomics.insert(r.id, v.clone());
                    }
                }
                crate::sem::program::ResKind::Mutex => {
                    self.store.mutexes.insert(r.id, MutexState::Free);
                }
                crate::sem::program::ResKind::Condvar => {
                    self.store.condvars.insert(r.id, CondvarState::default());
                }
                crate::sem::program::ResKind::Semaphore => {
                    self.store.semaphores.insert(r.id, r.permits);
                }
                crate::sem::program::ResKind::Channel => {
                    self.store.channels.insert(r.id, ChannelState::default());
                }
                crate::sem::program::ResKind::RwLock => {}
            }
        }
    }

    /// Allocate a fresh frame with initialized activation slots.
    pub fn alloc_frame(
        &mut self,
        program: &SemProgram,
        function: FunctionId,
    ) -> BackendResult<Frame> {
        let id = FrameId(self.alloc.next_frame);
        self.alloc.next_frame += 1;
        let f = program.function(function);
        let mut locals = BTreeMap::new();
        for (i, slot) in f.slots.iter().enumerate() {
            let value = match &slot.init {
                Some(v) => v.clone(),
                None => default_value(&slot.ty),
            };
            locals.insert(i, value);
        }
        Ok(Frame {
            id,
            function,
            pc: 0,
            locals,
            handles: BTreeMap::new(),
            ret: None,
        })
    }

    pub fn thread(&self, id: ThreadId) -> Option<&ThreadState> {
        self.threads.get(&id)
    }

    pub fn current_frame_id(&self, tid: ThreadId) -> Option<FrameId> {
        self.threads.get(&tid).and_then(|t| t.current_frame())
    }

    pub fn frame(&self, id: FrameId) -> &Frame {
        &self.store.frames[&id]
    }

    pub fn frame_mut(&mut self, id: FrameId) -> &mut Frame {
        self.store.frames.get_mut(&id).expect("frame exists")
    }

    pub fn current_frame(&self, tid: ThreadId) -> &Frame {
        let fid = self.current_frame_id(tid).expect("runnable thread has a frame");
        self.frame(fid)
    }

    /// Current statement index of a thread.
    pub fn pc_of(&self, tid: ThreadId) -> Option<usize> {
        self.current_frame_id(tid).map(|f| self.frame(f).pc)
    }

    pub fn set_runnable(&mut self, tid: ThreadId) {
        if let Some(t) = self.threads.get_mut(&tid) {
            t.status = ThreadStatus::Runnable;
        }
    }

    pub fn block(&mut self, tid: ThreadId, reason: BlockReason) {
        if let Some(t) = self.threads.get_mut(&tid) {
            t.status = ThreadStatus::Blocked(reason);
        }
    }

    /// Advance the current frame past its current (non-control) statement.
    pub fn advance(&mut self, tid: ThreadId) {
        if let Some(fid) = self.current_frame_id(tid) {
            self.frame_mut(fid).pc += 1;
        }
    }

    pub fn write_dst(
        &mut self,
        frame_id: FrameId,
        dst: SlotRef,
        value: Value,
    ) -> BackendResult<()> {
        match dst {
            SlotRef::Discard => Ok(()),
            SlotRef::Local(slot) => {
                self.frame_mut(frame_id).locals.insert(slot, value);
                Ok(())
            }
            SlotRef::Shared(r) => {
                self.store.write_shared(r, value);
                Ok(())
            }
        }
    }

    pub fn read_dst(&self, frame_id: FrameId, dst: SlotRef) -> Option<Value> {
        match dst {
            SlotRef::Discard => None,
            SlotRef::Local(slot) => self.frame(frame_id).locals.get(&slot).cloned(),
            SlotRef::Shared(r) => self.store.read_shared(r).cloned(),
        }
    }
}
