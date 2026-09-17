# ConcIR backend design: bounded operational semantics, Petri nets, and repair

**Status:** implemented in this repository alongside the existing static
validator. This document is the normative design for the new non-LLM backend:
a reference interpreter, a colored Petri-net translation, finite-state
exploration with properties, and a deterministic repair loop. It does **not**
redefine the research topic; it fixes the engineering contract of the backend
that `ConcPlanVerify` will later drive.

The existing validator (`src/validate/`, `src/ast.rs`, `src/expr.rs`,
`src/env.rs`) is preserved and remains the syntactic / static front end. The
backend consumes a successfully parsed [`Program`] and adds semantics only.
Nothing here calls an LLM or a network API.

---

## 1. Scope: the supported CIR subset

The backend is deliberately smaller than the full grammar. Anything outside
the supported subset yields `Unsupported` (never a silent no-op, never a fake
`Pass`).

### 1.1 Supported

| Area | Supported | Notes |
| ---- | --------- | ----- |
| Modules / FQNs | yes | resources and functions are resolved to typed IDs; short names are never compared across modules |
| Control | fallthrough, `goto`, `branch`, `switch`, `return` | `switch` on `Int` / bounded `Int` / `Enum` |
| Data | `assign_local`, `read_shared`, `write_shared` | sequentially consistent, single shared store |
| Atomics | `atomic_load`, `atomic_store`, `atomic_cas` | sequentially consistent |
| Mutex | `mutex_lock`, `mutex_unlock` | explicit `Free` / `Held(thread)`; owner-checked unlock |
| Channel | `channel_send`, `channel_recv` | `capacity = 0` rendezvous and `capacity >= 1` bounded FIFO |
| Condvar | `condvar_wait`, `condvar_notify`, `condvar_notify_all` | precise wait-set semantics, re-acquire the same lock |
| Semaphore | `semaphore_acquire`, `semaphore_release` | counting; optional `count` (default `1`) |
| Threads | `call`, `spawn`, `join`, `scope` | frames and thread handles are dynamic and exactly matched |
| Values | `Bool`, `Int`, bounded `Int`, `Float`, `String`, `Enum`, `Struct`, `Array` (values only) | bounded domain requirement for termination |
| Expressions | the parser in `src/expr.rs` | shared with the validator |

### 1.2 Explicitly `Unsupported` (v1)

These are reported as structured `Unsupported`, never downgraded:

- `rwlock_read` / `rwlock_write` / `rwlock_unlock`
- `select` (any guard)
- `async_call` / `await`
- `abstract_step`, `seq_hole`
- body-less ("nobody") functions that declare effects or `may_block: true`
- channel close / disconnect, `channel_send` on a closed channel
- array indexing in expressions, `&&` / `||`, calls inside expressions
- floating-point control flow that decides a synchronization choice
  (floats are values but not a finite analysis domain; using them in a guard
  is `Unsupported`)

### 1.3 Bounds are not semantics

Two different notions are kept apart and named differently:

- **Program limits** that the CIR itself declares: channel `capacity`,
  semaphore `count`, bounded `Int` ranges, function `bound`. These are part of
  the operational semantics. A channel of capacity 2 *is* full after two sends.
- **Analyzer limits** in [`AnalysisBounds`]: `max_threads`,
  `max_frames_per_thread`, `max_states`, `max_depth`, `max_boundary_events`.
  Reaching one is an `AnalysisBoundary`, recorded as a structured event. A
  state-space exploration that hit a boundary is *incomplete* and can never
  conclude `Pass`.

### 1.4 Semantic versioning

The backend introduces a new, explicit semantic layer. It does not change any
existing public type in `src/ast.rs`, `src/validate/`, or `src/expr.rs`. The
validator output (`ValidationReport`) is byte-compatible for unchanged
programs. New CLI subcommands are additive; the legacy `cir <file>` invocation
still prints the same report.

---

## 2. Identifiers

Strongly typed IDs prevent short-name confusion and stale-handle reuse:

```
ModuleId(u32)  ResourceId(u32)  FunctionId(u32)  StatementId(u32)
ThreadId(u64)  FrameId(u64)     ScopeId(u64)     HandleId(u64)
```

`StatementId` is a per-function index derived from `sid`; `ResourceId` and
`FunctionId` are program-wide. All comparisons are by ID, never by string.

Dynamic identities (`ThreadId`, `FrameId`, `ScopeId`, `HandleId`) are allocated
from monotone counters. When a frame slot is reused after a return, it gets a
**new** `FrameId` and freshly initialized instance data, so a stale handle can
never be mistaken for a new activation.

---

## 3. Operational semantics (reference interpreter)

The reference interpreter is a deterministic small-step machine. It is the
test oracle for the Petri-net translation and is implemented independently of
it. They share the AST, the resolved program, value types, and expression
evaluation; they do **not** share synchronization transition code.

### 3.1 Machine state

```
MachineState
  store: Store
  threads: BTreeMap<ThreadId, ThreadState>
  scopes:  BTreeMap<ScopeId, ScopeState>
  next_thread/frame/scope/handle: counters
  finished: BTreeSet<ThreadId>

Store
  vars:       BTreeMap<ResourceId, Value>        // Var
  atomics:    BTreeMap<ResourceId, Value>        // Atomic
  mutexes:    BTreeMap<ResourceId, MutexState>   // Free | Held(ThreadId)
  semaphores: BTreeMap<ResourceId, i64>          // available permits
  channels:   BTreeMap<ResourceId, ChannelState> // buffered queue OR rendezvous wait sets
  condvars:   BTreeMap<ResourceId, CondvarState> // wait set
  frames:     BTreeMap<FrameId, Frame>           // locals + return slot

ThreadState
  status: Runnable
        | Blocked(BlockReason)
        | Finished
  stack: Vec<FrameId>              // top-of-stack is current
  handles: BTreeMap<String, HandleId> // spawn/async handles created by this thread
  handle_children: BTreeMap<HandleId, ThreadId>
  scope_children: BTreeMap<ScopeId, Vec<ThreadId>>  // used at scope/join
```

`BlockReason` records the resource, the current statement, and (for condvar)
the lock to re-acquire. Diagnostics read the reason structurally; they never
parse place names.

### 3.2 Scheduling

The scheduler is a deterministic round-robin over runnable threads ordered by
`ThreadId`. A step is `(thread, action)` where `action` is the statement at the
top frame's program counter. The **first statement of a function body is the
entry**. Falling off the end is a run-time error (`Invalid`); the validator
already rejects such CFGs, but the interpreter must not silently accept it.

The interpreter is *not* confluent; the explorer is responsible for exploring
all interleavings. A single run follows one deterministic schedule.

### 3.3 Frames, calls, returns

- `call f(a...)`: evaluate modeled arguments **in the caller frame**, allocate
  a new `FrameId`, bind the callee's modeled params (positional, declaration
  order), push it. The callee's `Frame.ret_to = Some(RetAddr { caller_pc_next,
  dst })` where `dst` is resolved in the **caller's** scope.
- `return`: pop the frame. If the stack is empty the thread becomes
  `Finished` (and notifies enclosing scope/join). Otherwise write the return
  value into `ret_to.dst` of the caller and resume the caller at
  `caller_pc_next`.
- An overlapping call cannot misroute a return token: `ret_to` is stored in the
  callee frame instance, so the exact caller frame is addressed. (This closes
  the "shared return token" over-approximation for the reference semantics;
  the Petri net models the same exact matching.)
- `spawn f`: allocate a child thread with a fresh frame; record the handle in
  the spawning thread's `handles`. Spawn targets have no modeled params.
- `join h`: block until the child thread for handle `h` has finished.
- `scope [f1, f2, ...]`: allocate one `ScopeId`, spawn all listed functions as
  children registered to that scope, block until **all** of them finish, then
  fall through. The wait is on the scope's own member set, not on a global
  "all threads" condition.

### 3.4 Mutex

`Store.mutexes[m]` is `Free` or `Held(t)`.

- `lock m`: if `Free`, set `Held(t)` and fall through; otherwise block with
  `Lock(m)`.
- `unlock m`: if `Held(t)` for the current thread, set `Free` and wake the
  first waiter; otherwise it is a **semantic error** (`Invalid`), not ordinary
  blocking.
- On `unlock`/wake, a `Lock` waiter becomes runnable and resumes **after** its
  acquire statement.

### 3.5 Bounded channel

`capacity` is required (the validator enforces it).

- `capacity >= 1`: `ChannelState::Buffered(VecDeque<Value>)`.
  - send: evaluate the value **once**, at the send statement. If `len <
    capacity`, push and fall through; otherwise block with the captured value
    in `SendWait`.
  - recv: if non-empty, pop the head into `dst` and fall through; otherwise
    block in `RecvWait`.
  - When a recv frees a slot and a `SendWait` exists, the blocked sender's
    captured value enters the queue and the sender is resumed. Values are never
    re-evaluated while blocked.
- `capacity == 0`: rendezvous. A send registers in `SendWait` with its captured
  value; a recv registers in `RecvWait`. When both sides exist, they pair in
  FIFO order: the receiver gets the oldest sender's value, both sides resume.
- A missing `capacity` (validator E001) is `Invalid` for the backend.

### 3.6 Condvar

A condvar has a wait set of `(ThreadId, FrameId, pc)` and is always paired with
a mutex at `wait`.

- `wait cv, m`: it is a **semantic error** if the current thread does not hold
  `m`. Otherwise, atomically: release `m` (waking a lock waiter as usual), add
  the caller to `cv`'s wait set, and block with `CondvarWait { cv, m }`.
- `notify cv`: if the wait set is non-empty, remove the **oldest** waiter and
  place it on `m`'s re-acquire queue. If empty, nothing is remembered — there
  is **no stored permit**.
- `notify_all cv`: remove **all** current waiters and place them on `m`'s
  re-acquire queue. Waiters that arrive later are unaffected.
- A notified waiter becomes runnable only after it re-acquires the same `m`.
  Only then does its `wait` complete. The model has **no spurious wakeups**;
  progress never depends on one.
- `wait` does not exist for `Async` mode here (that is `Unsupported`).

### 3.7 Semaphore

`Store.semaphores[s]` is the number of available permits (initialised from the
resource `count`). `acquire n` (default 1): if `available >= n`, subtract and
fall through; else block in `SemWait`. `release n`: add `n` and wake waiters
(FIFO). `n <= 0` is `Invalid`.

### 3.8 Atomics and shared Vars

All are immediate (never queue). Bounded-Int writes whose result leaves the
declared range **disable** the step (no transition), matching the documented
CVN rule; this keeps counter loops finite. Unbounded `Int` is allowed but the
explorer may truncate and report `Unknown`.

---

## 4. Data and instance representation

```
Value = Bool | Int(i64) | Float(bits) | String | Enum(tag) | Struct(fields) | Array(values)
```

`Value` implements `Eq` + `Hash` (floats by bit pattern) so it can be part of a
complete semantic state. `_` is a discard target and never a stored value.

Activation data (params, locals, return slot) lives in `Frame.locals` keyed by
the **per-function slot index**, not by name. Two threads executing the same
function have different `FrameId`s and therefore non-interfering locals. Two
overlapping calls likewise.

Resources are keyed by `ResourceId`; therefore `a::mtx` and `b::mtx` can never
collide.

---

## 5. Petri net

### 5.1 Form

A **colored Petri net with a store**:

```
PetriNet { places: Vec<Place>, transitions: Vec<Transition>, initial: Marking }
PetriNetState { marking: BTreeMap<PlaceId, Vec<Token>>, store: Store }
```

- `Place` has a `PlaceKind`: a control location `Control(func, sid)`, or a
  resource place `Var(r)`, `Atomic(r)`, `Mutex(r)`, `Semaphore(r)`,
  `ChannelBuf(r)`, `ChannelSendWait(r)`, `ChannelRecvWait(r)`,
  `CondvarWait(r)`, `LockWait(r)`, `ScopeBarrier(scope)`.
- `Token` is either a `ControlToken { thread, frame }`, a `Value`, a
  `WaitToken { thread, frame, data }`, or a `MutexToken { Held(thread) | Free }`.
- `Transition` has input arcs, output arcs, an optional `Guard`, and an
  `Update`. Both guard and update are **declarative** (`NetExpr`), not opaque
  callbacks.
- `Marking.store` (the mutable data store: frames, var/atomic values, channel
  queues, condvar sets, semaphores) is **part of the complete state**: it is
  included in enabling, atomic update, serialization, equality, and dedup.
- A `TransitionOrigin` records `module`, `function`, `sid`, and semantic phase.
  A transition may carry several origins (e.g. the two sides of a rendezvous).
  Execution events bind a concrete `thread` / `frame` on top of the origin.

### 5.2 Enabling and firing

- `enabled_bindings(state)`: for each transition, for each binding of its
  input tokens, evaluate the guard against the store. Bindings are enumerated
  deterministically (`ThreadId` / `FrameId` order).
- `fire(state, transition, binding)`: consume input tokens, apply the update to
  the store, produce output tokens. The whole firing is atomic.
- Dedup is on the **complete state** (`marking` + `store`), compared by
  value — never by a loose hash. Hashes may be used as an index, but equality
  is re-checked.

### 5.3 Translation and independence

The builder in `src/petri/build.rs` maps each resolved CIR statement to one or
more transitions and creates the resource places. Rendering semantics live in
`src/petri/exec.rs` and are written against `PetriNetState` only; they never
call the reference interpreter. The two implementations share:

- the resolved program (`SemProgram`),
- value types and `Eq`/`Hash`,
- expression evaluation over a store,
- `AnalysisBounds`.

They independently implement:

- when a transition is enabled,
- how the store and marking change,
- how blocked threads are represented.

### 5.4 Differential testing

`tests/interp_petri_diff.rs` compares, for a set of small bounded programs:

- the canonical set of reachable semantic states,
- the set of observable steps (transition origin + thread binding),
- the blocked states,
- the property verdicts.

Auxiliary transitions (e.g. rendezvous registration and pairing) are projected
away before comparison: only transitions whose origin is a CIR statement are
observable; registration/pairing phases are internal and are projected by
matching the *end* control locations. State counts alone are never used as an
oracle, and the translator's own output is not the only oracle (hand-written
expected states supplement it).

---

## 6. Verification contract and results

### 6.1 Contract

```
VerificationContract
  properties: Vec<Property>            // must hold
  preserved: Vec<ObservableBehavior>   // must be preserved by any patch
  assumptions: SemanticAssumptions     // e.g. sequential consistency
  bounds: AnalysisBounds
  allowed_scope: PatchScope            // which functions / statements may change
```

The contract object is constructed from data and is **never** modified by a
candidate patch or by the repair loop. It is cloned into each verification run.

### 6.2 Properties

- **Safety**: `Assertion { location, condition }` and resource invariants
  (e.g. mutex owner check, channel capacity).
- **Deadlock**: a reachable state where no thread can step, at least one thread
  is not `Finished`, and no boundary was hit from that state.
- **EF goal**: some reachable state satisfies the goal.
- **AG EF goal**: every reachable state can still reach the goal. Implemented
  exactly as: build the full reachable graph; compute backwards the set of
  states that can reach a goal state; report a reachable state outside it.
- Goals name a task / thread instance / scope completion. Completion facts are
  durable: they survive `join` consuming the finished thread.
- Conjunctive goals (several must jointly hold) are checked as a conjunction,
  not as separate EF checks.

### 6.3 Result types

```
Outcome = Pass | Fail | Unknown | Invalid | Unsupported
```

- `Pass` only when the search is **complete** (no boundary events, state
  frontier exhausted) and every required property holds.
- `Fail` when a concrete counterexample exists; a valid safety counterexample
  found during a partial search is still a `Fail`.
- `Unknown` when the analysis is incomplete and no counterexample was found.
- `Invalid` for semantic errors of the program under the fixed semantics
  (e.g. unlock by a non-owner, wait without the lock).
- `Unsupported` for constructs in §1.2.

Dead transitions and unreachable statements are **diagnostics**, not repair
acceptance failures, unless the contract explicitly requires their liveness.

### 6.4 Diagnostics

```
DiagnosticRecord
  property, configuration
  counterexample_prefix: Vec<ObservedStep>
  instance_locations: Vec<{ thread, frame, function, sid }>
  blocked: Vec<{ thread, resource, holder, queue/wait-set summary }>
  cir_statements: Vec<{ module, function, sid }>
  complete: bool                       // evidence completeness
  proven_facts: Vec<Fact>              // what is established
  repair_hints: Vec<Hint>              // heuristic suggestions, kept separate
```

Diagnostics are built from structured state; no place-name string parsing is
used to reconstruct them. `proven_facts` and `repair_hints` are separate
fields.

---

## 7. Structured patch and iterative repair

### 7.1 Patch

```
CirPatch
  target_module, target_function, target_sid
  original_hash            // hash of the target function (version guard)
  changes: Vec<PatchChange> // e.g. SwapLockOrder, InsertUnlock, ...
  provenance: Vec<SourceRelation> // stable origin mapping
  diff: before/after text
```

Patch application fails loudly on: unknown target, hash mismatch, conflicting
changes. The contract and the patcher are separate types; a patch cannot touch
the contract.

### 7.2 CandidateProvider

```
trait CandidateProvider {
    fn name(&self) -> &str;
    fn next_candidate(&mut self, ctx: &RepairContext) -> Option<CirPatch>;
}
```

Implementations shipped: `FileCandidateProvider` (reads patch JSON from a
file), `LockOrderEnumerator` (deterministic, finite enumeration of a repaired
lock order for adjacent, side-effect-free mutex acquisitions). No LLM provider
exists in this crate.

### 7.3 Loop

Each candidate goes through:

1. patch legality (target exists, hash matches, no conflict),
2. CIR static validation (`validate::validate`),
3. supportability check (no §1.2 constructs),
4. re-translation,
5. verification of **all** required properties,
6. accept or reject.

Old-counterexample replay is a fast pre-filter only, never an acceptance
criterion. The loop has a deterministic candidate order, duplicate detection,
an iteration budget, and per-round diagnostics. Terminal states:
`Repaired`, `NoAcceptableCandidate`, `BudgetExhausted`, `AnalysisUnknown`.

Acceptance means "satisfies the fixed contract"; it makes no claim about
natural-language requirements. A fully LLM-free end-to-end demo (buggy CIR →
rejected candidates → accepted patch) ships in `tests/repair_e2e.rs`.

---

## 8. Module layout and staged plan

```
src/sem/        ids, values, resolved program, bounds, outcomes
src/interp/     reference interpreter (frames, threads, sync)
src/petri/      net core, translator, deterministic executor
src/explore/    state exploration, properties, diagnostics, projection
src/repair/     contract, patch, candidate providers, repair loop
src/bin/concir-backend.rs   CLI: run / explore / repair / diff
```

Stages (each compiles and is tested before the next):

1. **sem + interp** — bounded operational semantics. Tests: locals isolation,
   call/return, spawn/join/scope, mutex, channel, condvar, semaphore,
   non-owner unlock, wait-without-lock, early notify, notify bursts,
   notify_all isolation.
2. **petri** — net core, translation, executor, differential tests.
3. **explore** — graph construction, safety / deadlock / EF / AG EF, structured
   diagnostics, dead-transition analysis.
4. **repair** — contract, patch, providers, loop, end-to-end demo.
5. **validator hardening** — fix the reproduced risks in §9 and add regression
   tests.

---

## 9. Static-validator risks to reproduce and fix

Each is first reproduced by a failing test, then fixed:

| # | Risk | Location |
| - | ---- | -------- |
| R1 | `atomic_load` / `channel_recv` / `call` `dst` writing a protected `Var` is not checked for lock ownership | `validate/locks.rs::protected_var_accesses` |
| R2 | `condvar_wait` does not check that the paired lock is held | `validate/locks.rs` |
| R3 | `may_block` does not cover blocking ops (`channel_send`, lock acquire) and does not propagate through calls | `ast.rs::Op::is_blocking`, `validate/interface.rs` |
| R4 | `requires_held` is not an analysis entry condition, producing false positives/negatives in the callee body | `validate/locks.rs::check_var_access_without_lock` |
| R5 | cross-module resource identity is collapsed to short names | `validate/locks.rs` (`protection_map`, `required_lock`), `validate/types.rs` |

Semantics are not silently changed: fixes add checks or make the existing
checks module-aware. Any behaviour change is documented in
`CODE_REVIEW_HANDOFF.md` with a migration note.
