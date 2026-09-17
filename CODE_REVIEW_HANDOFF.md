# CODE_REVIEW_HANDOFF

Non-LLM concurrency backend for ConcIR: bounded operational semantics, Petri
nets, exploration, diagnostics, and deterministic repair.

This document is the reviewer's map. It lists changed files, capabilities,
key semantic decisions, how to reproduce the checks, open issues, and the
code to review most carefully.

> **Round 2 (review of commit `736f7e1`).** A second independent review found
> wrong `PASS`, wrongly accepted repairs, and engine disagreement. The
> corrections, evidence, and remaining items are in
> [`CODE_REVIEW_ROUND2.md`](CODE_REVIEW_ROUND2.md). In particular the eager
> hand-off below was removed: waiting is now enabledness with all eligible
> waiters as choices, handle names belong to frames, the contract is re-bound
> per candidate, and there is one checked verification entry with documented
> exit codes.
>
> **Round 3 (review of commit `f3463d6`).** A third review found more wrong
> `PASS`/repairs and engine disagreement. See
> [`CODE_REVIEW_ROUND3.md`](CODE_REVIEW_ROUND3.md): declared resources always
> get net places, condvar waiters carry their own locks, rendezvous enumerates
> every match, contract names bind to the entry module, patch scope is
> `module::function`, every value-entry path respects bounded domains, semaphore
> release is checked, and finite concurrent loops complete via identity
> canonicalization.

---

## 1. Files and responsibilities

### New — semantic core (`src/sem/`)

| File | Responsibility |
| ---- | -------------- |
| `ids.rs` | Strongly typed IDs: `ModuleId`, `ResourceId`, `FunctionId`, `StatementId`, `ThreadId`, `FrameId`, `ScopeId`, `HandleId`; `SlotRef`. |
| `value.rs` | `Value` with `Eq`/`Hash` (floats by bits), bounded-`Int` domain checks, JSON→typed init. |
| `program.rs` | Lowering `Program` → `SemProgram`: names → IDs, expressions → `LExpr`, slot tables, supportability (`Unsupported`) reporting. |
| `eval.rs` | `LExpr`, `ValueStore`, and expression evaluation. |
| `outcome.rs` | `Outcome`, `BackendError`, `Unsupported`, `Invalid`, `AnalysisBounds`, `BoundaryEvent`, `TransitionOrigin`/`Phase`, `StepLabel`. |
| `system.rs` | `TransitionSystem` trait, `Predicate` language, `BlockedRecord`, `InstanceState`. |

### New — reference interpreter (`src/interp/`)

| File | Responsibility |
| ---- | -------------- |
| `state.rs` | `MachineState`, `Store`, frames, threads, scopes, mutex/semaphore/channel/condvar state. |
| `exec.rs` | Small-step semantics; independent synchronization transitions. |

### New — Petri net (`src/petri/`)

| File | Responsibility |
| ---- | -------------- |
| `net.rs` | Colored net: places, tokens, transition templates, binding, `NetState` (marking + store), CIR→net build. |
| `exec.rs` | Deterministic net executor implementing `TransitionSystem`; independent sync logic. |

### New — exploration (`src/explore/`)

| File | Responsibility |
| ---- | -------------- |
| `contract.rs` | Serializable verification contract, property/predicate specs, patch scope, assumptions. |
| `mod.rs` | Reachable-graph construction, safety / deadlock / EF / AG EF, structured diagnostics, canonical projection. |

### New — repair (`src/repair/`)

| File | Responsibility |
| ---- | -------------- |
| `patch.rs` | `CirPatch`, content hash, strict application (`swap_statements`, `delete_statement`). |
| `candidates.rs` | `CandidateProvider`, `LockOrderEnumerator`, `FileCandidateProvider`. |
| `mod.rs` | Deterministic repair loop and terminal outcomes. |

### New — CLI / examples / tests

- `src/bin/concir-backend.rs` — `check` / `explore` / `run` / `repair` / `support`.
- `examples/lockorder_bug.json`, `examples/lockorder_contract.json` — end-to-end demo.
- `tests/interp_smoke.rs`, `interp_petri_diff.rs`, `petri_projection.rs`,
  `semantics_regression.rs`, `validator_risks.rs`, `repair_e2e.rs`.

### Modified

| File | Change |
| ---- | ------ |
| `src/lib.rs` | Declares `sem`, `interp`, `petri`, `explore`, `repair`. |
| `src/ast.rs` | `Op::is_blocking` now covers `channel_send`, `mutex_lock`, `rwlock_read`, `rwlock_write`. |
| `src/validate/locks.rs` | Rewritten: FQN-canonical held sets, module-aware protection, `requires_held` entry condition, `dst` write checks, `condvar_wait` ownership (E512). |
| `src/validate/interface.rs` | Transitive `may_block` fixpoint through `call`; used by E802 and E804. |
| `doc/error_codes.md` | Documents E512. |
| `README.md` | Backend section. |

New docs: `doc/backend-design.md`, `doc/backend-usage.md`.

---

## 2. Capabilities

### Implemented

- Precise bounded interpreter with dynamic frames/threads/handles/scopes.
- Mutex (owner-checked unlock), bounded and rendezvous channels, condvars
  (exact wait sets, no stored notify, no spurious wakeups), counting
  semaphores, sequentially consistent atomics and shared vars.
- Exact call/return matching via per-frame return addresses; spawn/join and
  scope wait on their own member sets.
- Colored Petri-net translation and an independent executor.
- Complete finite-state exploration with boundary tracking.
- Properties: safety invariants, `Unreachable`, global deadlock, EF goal,
  AG EF goal, conjunctive goals, durable completion facts.
- Structured diagnostics (counterexample prefix, instances, blocked reasons,
  CIR statements, proven facts, repair hints kept separate).
- Verification contract, structured patch, candidate providers, deterministic
  repair loop with duplicate detection and budget.
- Fixed static-validator risks R1–R5 with regression tests.

### Not implemented (reported as `Unsupported`, never silent)

- `RwLock`, `select`, `async_call` / `await`, `abstract_step`, `seq_hole`.
- Body-less functions with declared effects/blocking/return.
- Channel close/disconnect.
- Async-mode `condvar_wait`.
- Float used to decide a control-flow branch.
- Partial-order / symmetry reduction (intentionally out of v1).
- Fairness and starvation liveness.

---

## 3. Key semantic decisions

1. **Precise activation store.** Every parameter/local/return slot is tracked
   concretely per dynamic frame. The legacy `modeled` flag is a CVN projection
   hint and does *not* restrict the precise backend.
2. **Waiting is enabledness, not a forced hand-off.** A blocked
   mutex/semaphore/channel thread becomes enabled when its condition holds and
   completes its own operation; every eligible waiter is an independent
   choice. `notify_one` enumerates every current waiter. Channel *messages* are
   FIFO; waiting *thread* order is not. Handles belong to the frame. See
   `tests/differential.rs` (full projection + edge relation).
3. **Bounded `Int` disables the step.** An update leaving `[lo, hi]` is not
   enabled, matching the documented CVN rule; this keeps counter loops finite.
4. **Program limits vs analyzer limits.** Channel capacity, semaphore count,
   bounded `Int`, and function `bound` are semantics. `AnalysisBounds` are
   analyzer limits and produce boundary events that make a search incomplete.
5. **Deadlock requires a processed, boundary-free state.** Frontier states
   left unexpanded by the state limit are never reported as deadlocks.
6. **No spurious condvar wakeups; no stored notification.** `notify` with no
   waiter is lost by design; progress must not rely on a spurious wakeup.
7. **Durable completion facts.** `completed_functions`, `completed_scopes`,
   and `reached` are part of the state, so EF/AG EF goals survive `join`.
8. **AG EF via reverse reachability** on the full reachable graph (not the
   spanning tree).
9. **Net store is part of the state.** The mutable store participates in
   enabling, atomic firing, canonicalization, equality, and dedup.
10. **`Unsupported` is structured and terminal.** It never becomes `PASS`.

---

## 4. Test commands and actual results

Run (all except the pre-existing DOT snapshots):

```bash
cargo test --test semantics_regression --test validator_risks \
           --test interp_petri_diff --test petri_projection \
           --test repair_e2e --test interp_smoke --no-fail-fast
```

Observed:

```
semantics_regression ....... 13 passed
validator_risks ............  8 passed
interp_petri_diff ..........  4 passed
petri_projection ...........  2 passed
repair_e2e .................  4 passed
interp_smoke ...............  2 passed
```

Existing validator tests: all pass (`validate_dataflow` 43, `validate_scope`
7, `validate_typedef` 7, `module_syntax` 7, `validate_interface` 6,
`validate_seq_hole` 6, plus the rest).

Pre-existing failure (unchanged, unrelated): `tests/dot_export.rs` has 4
failing snapshot tests because `.gitignore` contains `**.snap` and no
snapshots are committed. See §6.

CLI checks:

```bash
cargo build --release
./target/release/concir-backend support examples/complex_rwlock.json
# supported: false; RwLock, select/async/external constructs listed

./target/release/concir-backend explore examples/producer_consumer.json
# outcome PASS, complete true, 65 states

./target/release/concir-backend explore examples/lockorder_bug.json examples/lockorder_contract.json
# outcome FAIL, no-deadlock fails; blocked: (0 scope), (1 lock b?), (2 lock a?)
```

---

## 5. End-to-end reproduction: buggy CIR → accepted patch

Input: `examples/lockorder_bug.json` (two mutexes `a`, `b`; `t1` acquires
`a`→`b`, `t2` acquires `b`→`a`; `main` scopes them). Contract:
`examples/lockorder_contract.json` (deadlock-free; preserve `t1` and `t2`
completion).

1. Static validity:

   ```bash
   ./target/release/concir-backend check examples/lockorder_bug.json
   # valid: true
   ```

2. Verification finds the ABBA deadlock:

   ```bash
   ./target/release/concir-backend explore examples/lockorder_bug.json examples/lockorder_contract.json
   # outcome FAIL, states_explored 106
   # diagnostic no-deadlock:
   #   blocked: thread 1 waiting on lock b, thread 2 waiting on lock a
   ```

3. Repair enumerates and verifies candidate lock reorderings:

   ```bash
   ./target/release/concir-backend repair examples/lockorder_bug.json examples/lockorder_contract.json
   # outcome "repaired", candidates_tried 1
   # accepted_patch: lockorder:main:t1:s1-s2
   #   changes: [{kind: swap_statements, a: s1, b: s2}]
   # rounds: accepted, "all required properties and preserved behaviour hold"
   ```

4. The accepted program is re-checked by `validate::validate` and re-verified
   (`tests/repair_e2e.rs::end_to_end_lock_order_repair_succeeds` asserts
   `PASS`). The patch is `t1: swap s1 <-> s2`, which makes both threads acquire
   `b` then `a` in a consistent order.

Rejection demonstrations in `tests/repair_e2e.rs`:

- `rejects_patch_that_removes_required_behavior` — a candidate that removes
  the deadlock but makes a preserved goal unreachable is rejected by full
  verification.
- `budget_exhaustion_when_no_candidate_satisfies` — with an unsatisfiable
  preserved behaviour and budget 1, the loop ends `BudgetExhausted`.
- `tests/semantics_regression.rs::truncated_search_is_unknown_not_pass` —
  a truncated search yields `UNKNOWN`, not `PASS`.

---

## 6. Open issues

1. **Pre-existing DOT snapshot failures.** `tests/dot_export.rs` depends on
   insta snapshots that are git-ignored (`**.snap`) and were never committed.
   Four tests fail on any fresh checkout. This is unrelated to the backend and
   was deliberately left untouched; not masked by editing expectations.
2. **Old-counterexample replay is not implemented.** It is documented as an
   optional fast pre-filter only; the loop always runs full verification.
3. **Unbounded `Int` termination** relies on `max_states`; such searches end
   `UNKNOWN` rather than `PASS`.
4. **Shared `frame.pc`/control-place synchronization in the net** is subtle:
   the control place is the source of truth and `frame.pc` is kept in sync by
   `place_control`. Any new net operation that moves a control token must use
   that helper.
5. **No fairness/starvation checks.** `AG EF` means reachability preservation,
   not eventual completion.
6. **`bound` is treated as a program-level limit** (spawn disabled at the
   bound), which can surface as a semantic deadlock under that declared bound.
7. **`abstract_step` / `seq_hole`** have no semantics; any reachable use is
   `Unsupported`.

---

## 7. Where to review most carefully

- `src/petri/exec.rs` — the `fire` match and `place_control` / `wake_control_next`
  / `grant_lock_now` / `grant_semaphore_now`; eager hand-offs must not
  double-advance a frame. The differential tests are the safety net.
- `src/interp/exec.rs` — `drain_channel` (alternating sender-admit / receiver-deliver
  fixpoint), `CondvarWait`/`Notify`, and `finish_thread` scope/join wakeups.
- `src/explore/mod.rs` — `check_property` (deadlock requires `processed` and no
  boundary; AG EF reverse reachability) and the reachable-graph edge recording.
- `src/validate/locks.rs` — FQN canonicalization, `requires_held` seeding, and
  the `dst_writes` table.
- `src/repair/mod.rs` — acceptance requires a full `PASS`; nothing else accepts.

---

## 8. Compatibility and migration

- No existing public type or JSON syntax changed. `ValidationReport` remains
  byte-compatible for programs that do not use the newly checked patterns.
- Behavior changes are additive checks: extended E309, new E512, broader and
  propagated `may_block`, module-aware protection, and `requires_held` as the
  entry condition. `doc/backend-usage.md` §"Static-validator changes" lists
  each with a migration note.
- The legacy `cir <file>` invocation and exit codes are unchanged.
