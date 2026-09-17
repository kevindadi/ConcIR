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
- `return`: the returned expression is evaluated, then checked against the
  callee's **own declared `returns` type** (recursively, including composite
  types), and independently against the caller's `dst`. If either check fails
  the whole return step is disabled before any frame is popped, value written,
  joiner/scope woken, or completion recorded. A disabled return therefore never
  records `function_completed`. Then the frame is popped; if the stack is empty
  the thread becomes `Finished` (and notifies enclosing scope/join), otherwise
  the value is written into `ret_to.dst` of the caller and the caller resumes
  at `caller_pc_next`. Omitting the caller `dst` or returning from the entry
  function does not bypass the declared return type.
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

### 3.4 Waiting is enabledness, not a forced hand-off

A thread blocked on a mutex, semaphore, or channel is **enabled again as soon
as its condition holds**, and it completes its own blocking operation. There is
no forced atomic hand-off from an unlock/release/notify to a specific waiter,
and no FIFO order over waiting *threads*: every eligible waiter is an
independent choice, so the program's nondeterminism is preserved. Message order
on a channel is FIFO (a property of the channel), but the order in which
waiting threads are served is not specified.

### 3.5 Mutex

`Store.mutexes[m]` is `Free` or `Held(t)`.

- `lock m`: if `Free`, set `Held(t)` and fall through; otherwise block with
  `Lock(m)`.
- `unlock m`: if `Held(t)` for the current thread, set `Free`; otherwise it is a
  **semantic error** (`Invalid`), not ordinary blocking. `unlock` does not
  force the lock onto a particular waiter.
- While `m` is `Free`, **any** thread blocked in `Lock(m)` may acquire it (each
  is a separate choice); a runnable thread may also acquire it.

### 3.6 Bounded channel

`capacity` is required (the validator enforces it).

- `capacity >= 1`:
  - send: evaluate the value **once**, at the send statement. If `len <
    capacity`, push and fall through; otherwise block with the captured value
    in `SendWait`.
  - recv: if non-empty, pop the head into `dst` and fall through; otherwise
    block in `RecvWait`.
  - A blocked sender is enabled once there is space and pushes its captured
    value; a blocked receiver is enabled once the buffer is non-empty. Values
    are never re-evaluated while blocked. The buffer is a FIFO queue, so
    messages are received in send order.
- `capacity == 0`: rendezvous. A send registers in `SendWait` with its captured
  value; a recv registers in `RecvWait`. The arriving side is matched with
  **every** front waiter of the other side as a separate choice, so several
  waiting receivers (or senders) yield several matches; the two blocked sides
  can never coexist because the second arrival pairs. Frozen message values are
  carried by the sender tokens, so a match never re-evaluates a send. Waiting
  threads are not served FIFO.
- A missing `capacity` (validator E001) is `Invalid` for the backend.

### 3.7 Condvar

A condvar has a wait set of `(ThreadId, lock)` and each `wait` is paired with
the mutex the waiter held.

- `wait cv, m`: it is a **semantic error** if the current thread does not hold
  `m`. Otherwise, atomically: release `m`, add `(caller, m)` to `cv`'s wait set,
  and block with `CondvarWait { cv, m }`. Different waiters may be associated
  with **different** locks.
- `notify cv`: if the wait set is non-empty, remove **any** current waiter
  (every waiter is a legal choice; the enumeration order is deterministic but
  the choice is not fixed) and queue it to re-acquire **its own** lock. If
  empty, nothing is remembered — there is **no stored permit**.
- `notify_all cv`: remove **all** current waiters and queue each to re-acquire
  its own lock. Waiters that arrive later are unaffected. It also advances
  normally when there is no wait site and no waiter at all.
- A notified waiter becomes runnable only after it re-acquires its lock. Only
  then does its `wait` complete. The model has **no spurious wakeups**;
  progress never depends on one.
- `wait` does not exist for `Async` mode here (that is `Unsupported`).

### 3.8 Semaphore

`Store.semaphores[s]` is the number of available permits (initialised from the
resource `count`). `acquire n` (default 1): if `available >= n`, subtract and
fall through; else block in `SemWait`. `release n`: add `n`; every waiter whose
request can now be satisfied is an independent choice (no FIFO wake order).
`n <= 0` is `Invalid`. The addition is **checked**: a release that would
overflow `i64` is a structured `Invalid` (`E905`), never a panic or wrap.

### 3.9 Atomics and shared Vars

All are immediate (never queue). **Every** value-entry path respects the
destination's declared domain: explicit `assign_local` / `write_shared` /
`atomic_store`, `read_shared` / `atomic_load` / `atomic_cas` `dst`,
`channel_recv` `dst`, `call` arguments, and `call` returns. A write whose value
leaves a bounded `Int` range **disables the whole step** (no transition), and
the check happens before any message is consumed, lock released, or frame
unwound, so no half effect is emitted. The channel payload type is checked the
same way on `channel_send`. Unbounded `Int` is allowed but the explorer may
truncate and report `Unknown`. Semantic domain bounds, host integer overflow,
and the analysis budget are distinct outcomes and never substituted for one
another.

### 3.10 Per-frame handles, finite monitors, and identity canonicalization

Spawn/join handle **names** are bound to the current activation (frame), not to
the thread, so a callee cannot clobber its caller's bindings. Concrete child
identity is keyed by a unique handle id in the thread's child table. A
successful `join` consumes the handle binding and reclaims the finished child;
a scope reclaims its members when it completes; a second `join` on the same
handle is a defined semantic error (`Invalid`). No stale identity remains.

Historical completion facts are monitored per the contract: a plain
`FunctionCompleted` needs a boolean, `FunctionCompletedAtLeast(n)` saturates at
`n`, and functions no predicate observes are not counted. In addition, the
explorer deduplicates states by a **fully identity-normalized canonical form**
(threads, frames, scopes, handles, and their child bindings are renamed to a
dense order), so a finite concurrent loop (`scope(worker); goto`, or
`spawn; join; goto`) yields a finite graph and a complete result without
raising `max_depth`/`max_states`. This normalization only factors internal
identity numbering; every fact that affects future behavior or an observed
predicate stays in the state. Genuine data growth, recursion depth, or the
search budget still produce `Unknown`.



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

### 4.1 Semantic state key (deduplication)

`explore`/`verify` deduplicate by `TransitionSystem::state_key`, **not** by the
human-readable `canonical` text. The key is the full semantic state with
dynamic identities normalized:

- shared data: `Var`/`Atomic` values; `Mutex` `Free`/`Held(thread)`;
  semaphore permit counts; channel buffer contents, pending senders
  `(thread, value)`, and pending receivers;
- condvars: `(thread, lock)` wait pairs;
- frames: function, `pc`, all locals (by slot), handle bindings
  `name -> handle`, return continuation `(pc_next, dst)`;
- threads: entry function, status (`run` / blocked-with-normalized-reason /
  finished), control stack, child map `handle -> thread`, parent scope;
- scopes: id, owner, owner frame, owner sid, remaining members;
- durable monitors: completion counts, completed scopes, reached facts.

Identity normalization renames threads, frames, scopes, and handles to a dense
order derived from the state itself, so two states that differ only by fresh
identity numbering are one quotient state. This is alpha-equivalence of
internal identities; every fact that affects future behavior or an observed
predicate remains in the key.

`Value` has two encodings:

- `Value::canonical()` — readable display text (JSON-escaped, diagnostics
  only); and
- `Value::key()` — the *semantic* encoding: type-tagged and length-prefixed
  (`T<len>:<bytes>` for strings/field names, `S<n>[...]`, `A<n>[...]`,
  `I<i>;`, `B0/1`, `E<len>:<tag>`, `F<bits>;`). Length prefixes make the
  encoding injective, so distinct values can never share a key. Changing the
  hash function does not substitute for this.

Coverage of the equivalence claim is explicit and test-backed:

- `tests/round4_regressions.rs` runs an independent raw-`State`-`Eq`/`Hash`
  oracle that never calls the production key or renderer, and checks that no
  two raw states share a key while disagreeing on a goal.
- `tests/round5_regressions.rs` (a) translates one reachable state by a
  constant offset in *every* `ThreadId`/`FrameId`/`ScopeId`/`HandleId`
  occurrence and asserts the key, predicate truth, and the set of
  `(origin, successor-key)` actions are unchanged; (b) replays every stored
  quotient edge and asserts an enabled step of the source realizes the same
  origin and target key; and (c) groups a bounded raw BFS by key and asserts
  merged states agree on predicate truth and successor actions.

The covered equivalence is **dynamic-identity translation/renaming**, i.e.
alpha-equivalence of internal ids. It is not a claim that arbitrary graph
isomorphism yields one key, and it does not by itself prove the absence of
every missed or incorrect merge; the tests distinguish the two directions
(key equality with divergent behavior = incorrect merge; raw
translation-equivalent states with different keys = missed merge for the
translated case).

### 4.2 Resource domain checks and step atomicity

`within_type(value, type)` is recursive: it checks struct fields, array length
and elements, enum membership, exact primitives, and bounded-`Int` ranges.
Every value-entry path uses it — initialization, direct assignment/stores,
`read_shared`/`atomic_load`/`atomic_cas` `dst`, `channel_recv` `dst`, `call`
arguments, `call` returns, and channel payloads (checked against the channel's
own base, independently of the receiver's `dst`, including `dst = "_"`).

A value that leaves a declared domain **disables the whole step**; the check
runs before any token is consumed, message enqueued/dequeued, lock released,
control advanced, or frame unwound, so no half effect is ever emitted. Frozen
values (a blocked sender's payload, a `SendWait` token) are never re-evaluated
on resume. Semantic domain bounds, host integer overflow (`E905`), and the
analysis budget are distinct outcomes and are never substituted for one
another.



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
candidate patch or by the repair loop. It is cloned into each verification run,
and it retains the symbolic `ContractSpec` it was resolved from.

### 6.2 Properties

- **Safety**: `invariant` holds in every reachable state (the `Safety` /
  `Unreachable` forms).
- **Deadlock**: a reachable state where no thread can step, at least one thread
  is not `Finished`, and no boundary was hit from that state.
- **EF goal**: some reachable state satisfies the goal.
- **AG EF goal**: every reachable state can still reach the goal. Implemented
  exactly as: build the full reachable graph; compute backwards the set of
  states that can reach a goal state; report a reachable state outside it.
- Goals name a task / function activation / scope statement completion.
  Completion facts are durable: they survive `join` consuming the finished
  thread.
- Conjunctive goals (several must jointly hold) are checked as a conjunction,
  not as separate EF checks.

### 6.3 Unified checked entry

`explore::verify_program(program, contract_spec, engine)` is the only entry the
CLI and the repair loop use. It performs, in order:

1. CIR static validation (`validate::validate`); errors ⇒ `Invalid`.
2. Lowering; unknown names/types ⇒ `Invalid`.
3. Supportability (including the entry function) ⇒ `Unsupported`.
4. Contract validation, then resolution against the current program
   (properties, preserved behaviour, assumptions, bounds, predicate kinds).
   Unsupported assumptions ⇒ `Unsupported`; malformed input ⇒ `Invalid`.
   Unqualified names in the contract bind to the **entry module's** namespace
   (never `ModuleId(0)`), so declaration/module reordering does not change the
   verified object; fully-qualified names resolve exactly.
5. Contract-driven monitors, exploration, and property checking.

The report records the model and contract fingerprints, the semantic
assumptions, the analysis bounds, and whether the analysis actually started
(an early `Invalid`/`Unsupported` exit reports the *requested* configuration,
not a default that was never used).

### 6.4 Result types and exit codes

```
Outcome = Pass | Fail | Unknown | Invalid | Unsupported
```

- `Pass` only when the search is **complete** (no boundary events, state
  frontier exhausted) and every required property holds.
- `Fail` when a concrete counterexample exists; a valid safety counterexample
  found during a partial search is still a `Fail`.
- `Unknown` when the analysis is incomplete and no counterexample was found.
- `Invalid` for a statically invalid or semantically erroneous program, or for
  a malformed contract.
- `Unsupported` for constructs in §1.2 or unsupported semantic configuration.

CLI exit codes: `0` PASS / repaired, `1` FAIL, `2` usage/input error, `3`
UNKNOWN, `4` INVALID, `5` UNSUPPORTED. Only an overall PASS is success; the JSON
report keeps the detailed category.

### 6.5 Diagnostics

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
  target_module, target_function
  original_hash            // hash of the target function (version guard)
  changes: Vec<PatchChange> // swap_statements, delete_statement
  provenance: Vec<SourceRelation> // stable origin mapping
```

Patch application fails loudly on: unknown target, hash mismatch, a statement
touched by more than one change, or an illegal change. The contract and the
patcher are separate types; a patch cannot touch the contract. A
provider-independent `check_allowed(scope, patch)` enforces the scope and the
per-change `allow_lock_reorder` / `allow_statement_delete` permissions for
**every** provider, including file candidates. Scope function entries are
matched as `module::function` identities (a bare entry is a legacy short name),
so `["main::t1"]` allows only `main::t1` and excludes `other::t1`.

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

1. unified permission check (`module::function` scope, per-change `allow_*`),
2. patch legality (target exists, hash matches, no conflict),
3. CIR static validation (`validate::validate`),
4. supportability check (no §1.2 constructs),
5. re-lowering,
6. **re-binding the frozen symbolic `ContractSpec` against the new program**
   (a deleted `StatementReached` / `ScopeCompleted` target rejects the
   candidate; old body indices are never reused),
7. full verification of every property and preserved behaviour,
8. accept or reject.

Old-counterexample replay is a fast pre-filter only, never an acceptance
criterion. The loop has a deterministic candidate order, content-based
duplicate detection (normalized module/function/changes, not a user id), an
iteration budget, and per-round diagnostics. Terminal states: `Repaired`,
`NoAcceptableCandidate`, `BudgetExhausted`, `AnalysisUnknown`.

Acceptance means "satisfies the fixed contract"; it makes no claim about
natural-language requirements. A fully LLM-free end-to-end demo (buggy CIR →
rejected candidates → accepted patch) ships in `tests/repair_e2e.rs`; the
round-2 review regressions ship in `tests/round2_regressions.rs`.

### 7.4 Diagnostic-driven composite search

`src/repair/search.rs` adds a budgeted search over programs. Three strategies
share the same edit space, permissions, verification semantics, and budgets,
so "composite capability" and "diagnostic guidance" can be measured
separately:

- `Single` (A): expand only the root, one edit deep — the legacy baseline.
- `Composite` (B): bounded BFS over nodes, no diagnostic guidance.
- `Diagnostic` (C): the same BFS, with candidates filtered by the node's
  structured blocking facts.

**Data flow.** The search first verifies the original program. A complete
`PASS` returns `AlreadySatisfied` (no patch is produced); `Invalid`,
`Unsupported`, and `Unknown` roots are reported as such. Each search node
stores its parent, depth, total edits, program, stable program fingerprint,
the verification report, and the patch that produced it, so any node is
reconstructible and independently re-verifiable.

**Provider context.** `RepairContext` carries the node's program, the frozen
`ContractSpec`, the round/depth, the structured `VerificationReport`, and the
ancestor `NodeHistory`. Providers read `blocked[*].resource_name` (a
`module::entity` fact produced by the engine, never parsed text) to decide
relevance. The diagnostic strategy treats resource relevance as a *fact* and
the swap as a *heuristic*; the two are recorded separately (the report keeps the
facts, the candidate provenance records the heuristic).

**Verification bounds.** The frozen `ContractSpec.bounds` is the single source
of truth for every verification call (root and children). `SearchConfig` does
not carry bounds; the effective bounds are recorded in the artifact. A caller
who wants a smaller analysis sets the contract bounds. A run can never exceed
them or silently widen them.

**Deduplication before verification.** After a candidate is applied and
statically validated, its whole-program fingerprint is checked against the
cache. An already-verified program is *reused* (the attempt records
`reused_node`), consumes no verification budget, and is not re-enqueued. The
counts distinguish proposals, unique candidate programs, verification calls,
and cache hits. Strategy A (single) never expands children.

**Node and attempt identity.** Every unique verified program is a *node* with a
stable id (its index in `nodes`); `parent` is a node id or `None` for the root.
Every candidate proposal is an *attempt* referencing the node it came from.
Denied, apply-error, and static-invalid attempts produce no node; a reused
attempt references the existing node. The incoming patch of a node records its
`original_function_hash`, `parent_fingerprint`, and `program_fingerprint`.

**Budgets and stop reasons.** The root verification counts toward
`verification_budget`; a zero budget is rejected before any verification with
`InvalidConfig`. Candidate and verification budgets, `max_depth`, and
`max_total_edits` are all enforced; depth/edit truncation, a candidate-budget
exhaustion, and a verification-budget exhaustion are reported with distinct
stop reasons. Strategy A exhausting its single-step enumeration is
`NoAcceptableCandidate`, not a budget truncation. `UNKNOWN` encountered while
searching is reported (`saw_unknown`) and never accepted; a `UNKNOWN` root is
`AnalysisUnknown`.

**Acceptance and artifact.** Intermediate `FAIL` nodes are kept and expanded
(except in strategy A); `UNKNOWN`, `INVALID`, and `UNSUPPORTED` nodes are never
accepted or expanded. Only an overall complete `PASS` is a final repair. Every
run produces a self-contained `SearchArtifact` (schema
`concir-repair-artifact-v1`) with the input program, frozen contract, effective
config and bounds, source identity (crate version and a binary fingerprint),
all nodes/attempts and their relationships, full verification reports, the
patch chain, the accepted node id, the accepted program, counts, stop reason,
and a reproduce command. `replay_artifact` validates the record: structure
(unique sequential ids, parent existence/acyclicity, depth/edit consistency,
attempt relationships, effective bounds equal to the frozen contract, derivable
counts, budget coherence), then rebuilds every node by applying its incoming
patch with the frozen contract's permission check and compares the **normative
report** (outcome, completeness, model/contract fingerprints, assumptions,
bounds, property verdicts, diagnostics, unsupported/invalid/boundary evidence),
including the cost and analysis markers (`analysis_started`, `states_explored`,
`transitions_explored`, structured counterexample/blocking evidence) with the
total cost required to equal the fresh per-node sum. It then checks **attempt
consistency** (each patch independently explains its recorded result, verified
attempts map to their node in verification order, reused attempts point at
earlier verified nodes, budget-blocked attempts are applicable, valid, and only
appear once the budget is spent) and **outcome evidence**: the terminal
`outcome`/`stop_reason`/`truncation`/`saw_unknown` quadruple is re-derived from
the recorded graph through the same `TerminalFacts::classify` the live search
uses (one shared priority: root-unknown, candidate budget, verification budget,
depth+edit truncation, depth, edit, UNKNOWN, no acceptable candidate) and
compared as a whole. Truncation is a property of *expandable* nodes (the root,
and verified FAIL nodes under B/C — never under A): a bound only counts as
truncation when the strategy would have expanded the node, so strategy A's
single-step `no_acceptable_candidate` is not misread as truncation, while B/C
that still wanted to expand report `budget_exhausted`. The stated fields are
never treated as their own evidence. It then replays the accepted `patch_chain`
from the input and requires the chain end to equal the accepted node,
`accepted_program`, and `accepted_report`, and finally checks outcome
compatibility. The producer's binary fingerprint is an identifier, not a
required match; the schema version is. Inconsistencies are explicit errors.
Exhausting a strategy or budget only means "not found under this
strategy/budget"; no repair-nonexistence or global-optimality claim is made.
The development benchmark lives in `src/repair/benchmark.rs` and is exposed by
the `bench` CLI subcommand; it is a development regression set, not an
independent evaluation corpus.

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
