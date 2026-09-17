# CODE_REVIEW_ROUND2

Second-round response to the independent review of commit `736f7e1`
(`REVIEW.zh.md`). This document records, item by item, the root cause, the fix
location, the regression that now encodes the **correct** expectation, the
commands run, actual results, engine-independence and finiteness arguments, and
what remains open.

The reviewed evidence directory is untouched. The review's own probes were run
against a copy in `/tmp` so the recorded `.stdout`/`.stderr` are not
overwritten. The counterexamples are migrated into `tests/repro_round2/`.

## Gate

```
round2_regressions ....... 18 passed
differential .............  8 passed
semantics_regression ..... 13 passed
repair_e2e ...............   4 passed
validator_risks ...........  8 passed
interp_petri_diff .........  4 passed
(all pre-existing validator tests) ok
dot_export ............... 4 failed  (pre-existing; see "Remaining")
```

The reviewed build's original outputs are **not** used as golden values; the
tests assert the correct outcomes.

---

## R1 — handle bindings must belong to the activation (P1)

**Root cause.** `handles` was stored on `ThreadState`/`NetThread` keyed by the
local handle string, so a callee using the same local name clobbered the
caller's binding; after return the caller's binding was not restored.

**Fix.**
- `src/interp/state.rs`: `Frame` now owns `handles: BTreeMap<String, HandleId>`;
  `ThreadState` keeps only `handle_children: BTreeMap<HandleId, ThreadId>`.
- `src/interp/exec.rs`: spawn binds the name on the current frame; join
  resolves it on the current frame; `finish_thread` joiners resolve via the
  joiner's frame (`src/petri/exec.rs` mirrors this).
- `src/petri/net.rs` / `src/petri/exec.rs`: `NetFrame.handles`, spawn/join and
  the joiner scan updated identically.

**Regression.** `tests/round2_regressions.rs::r1_*`:
- `nested_handle_false_pass` → **FAIL** (was PASS), complete, both engines;
- `nested_handle_renamed_control` (callee local renamed) → **FAIL**;
- `nested_handle_names` (no gate) → **PASS**.

**Actual.**
```
nested_handle_false_pass   check valid=true; interp FAIL 17 states; petri FAIL 23 states
nested_handle_renamed_control                           interp FAIL 17;      petri FAIL 23
nested_handle_names                                     interp PASS 18;      petri PASS 29
```

**Engine independence / finiteness.** Both engines were changed; the full
projection differential (`tests/differential.rs`) compares them on the nested
handle programs. Renaming a callee-local handle does not change the projection
(projection uses child thread ordinals, not names).

---

## R2 — patches reused stale body indices (P1)

**Root cause.** `StatementReached` / `ScopeCompleted` were resolved to body
indices once, against the original program; the repair loop then reused that
contract for the patched program, so `t2.s3` could silently re-bind to
`return`.

**Fix.**
- `VerificationContract` now stores its symbolic `source: ContractSpec` and
  exposes `rebind(program)` (`src/explore/contract.rs`).
- `run_repair` re-resolves the frozen spec against every candidate program and
  rejects the candidate if a target is gone (`src/repair/mod.rs`).
- The unified entry validates the contract against the analyzed program
  (`explore::verify_program`).

**Regression.** `r2_deleting_a_required_statement_is_not_accepted`,
`r2_fresh_resolve_of_deleted_target_is_invalid`:
```
stable_sid_deleted: outcome no_acceptable_candidate; reason
  "contract cannot be re-bound after patch: function 'main::t2' has no statement 's3'"
fresh_resolve_deleted_sid: exit 4, outcome INVALID,
  invalid: "function 'main::t2' has no statement 's3'"
```

---

## R3 — file candidates bypassed patch permissions (P1)

**Root cause.** The loop checked only the function name; `allow_lock_reorder`
was checked inside the automatic enumerator, and `allow_statement_delete` was
never enforced.

**Fix.** `patch::check_allowed(scope, patch)` (`src/repair/patch.rs`) is the
single provider-independent check: full `module::function` scope plus
per-change `allow_lock_reorder` / `allow_statement_delete`. `run_repair` calls
it before applying any candidate. `PatchScope` gained `modules`.

**Regression.** `r3_forbidden_delete_is_not_accepted`,
`r3_forbidden_reorder_is_not_accepted`:
```
forbidden_delete:  outcome no_acceptable_candidate
  reason "disallowed: ... statement deletion is not allowed by the contract"
forbidden_reorder: outcome no_acceptable_candidate
  reason "disallowed: ... lock reordering is not allowed by the contract"
```
Candidate dedup now uses normalized `module::function` + serialized changes,
and a statement touched by two changes is rejected (`patch::apply`).

---

## R4 — notify_one forced the queue head (P1)

**Root cause.** Both engines `pop_front`/`take_first`, a shared under-approximation
that neither engine comparison can catch.

**Fix.**
- Interpreter `step_notify` returns one successor **per current waiter** (plus
  one for the empty wait set).
- Net builds `CondvarNotifyHit` with a new `Binding::ControlChooseWait`, whose
  executor enumerates every token; the lock to re-acquire is carried by the
  waiter token (no static `condvar_lock` inference).
- The broader audit removed forced hand-off/FIFO over *waiting threads*:
  `MutexUnlock`, `CondvarWait`, `SemaphoreRelease`, and channel send/recv no
  longer hand off atomically; a blocked thread is enabled when its condition
  holds, and every eligible waiter is a choice (`src/interp/exec.rs`,
  `src/petri/exec.rs`, `src/petri/net.rs`). Channel **message** order stays
  FIFO.

**Regression.** `r4_notify_one_must_consider_every_waiter`:
```
notify_choice_false_pass: interp FAIL 197 states; petri FAIL 221 states; complete
```
The full-projection differential also passes on this program, so the two
engines now agree on the complete (nondeterministic) behaviour, not just on the
outcome.

---

## R5 — notify_all with no static wait site (P1)

**Root cause.** The net only built `notify_all` when a mutex could be inferred
from a `condvar_wait` site; a lone `notify_all` had no successor.

**Fix.** `CondvarNotifyAll` is built unconditionally and moves each waiter token
to *its own* lock's `LockWait` place (the token carries the lock). No static
wait site is required (`src/petri/net.rs`, `src/petri/exec.rs`).

**Regression.** `r5_notify_all_without_wait_advances`: interp PASS 3 states,
petri PASS 3 states, both complete.

---

## R6 — the verification entry skipped checks and ignored assumptions (P1)

**Root cause.** `concir-backend explore` lowered the program directly; there
was no static validation, and `sequential_consistency=false` /
`no_spurious_wakeups=false` were copied into the contract but never acted on.

**Fix.** `explore::verify_program(program, spec, engine)` is the single checked
entry (`src/explore/mod.rs`); the CLI and repair use it. It performs static
validation → lowering → supportability (including the entry) → contract
validation/binding → monitors → exploration/properties. `ContractSpec::resolve`
now returns `ContractError::{Unsupported,Invalid}`, validates assumptions,
bounds, property ids, and predicate resource kinds
(`src/explore/contract.rs`). Reports carry model/contract fingerprints, the
assumptions used, and bounds.

**Regression.** `r6_*`:
```
invalid_protected_write: check invalid (E309); explore INVALID both engines, exit 4
ignored_assumptions:     check valid; explore UNSUPPORTED both engines, exit 5
contract errors:         unknown resource / missing sid / zero bounds / wrong
                         predicate kind all return ContractError::Invalid (no panic)
```

---

## R7 — finite programs produced infinite histories (P2)

**Root cause.** Every return incremented `completed_functions` without bound,
so a finite control loop that calls an immediate-return helper grew new states
each iteration and ended `UNKNOWN`.

**Fix.** `MonitorConfig` (`src/sem/monitor.rs`) derives saturation thresholds
from the contract: `FunctionCompleted` → boolean, `FunctionCompletedAtLeast(n)`
→ saturate at `n`, unobserved functions are not counted. Both engines accept a
monitor and saturate at increment sites; `verify_program` supplies it.

**Regression.** `r7_finite_call_loop_is_complete_with_small_state_space`:
```
finite_call_loop: interp PASS 6 states complete; petri PASS 6 states complete
```
Genuine growth (unbounded data, recursion depth, `max_states`) still yields
`UNKNOWN`; the `tiny_bounds_contract.json` fixture exercises the truncation
path and the CLI exit code 3.

---

## R8 — CLI treated non-PASS as success (P2)

**Fix.** Documented exit codes in `src/bin/concir-backend.rs` and the usage
text: `0` PASS/repaired, `1` FAIL, `2` usage, `3` UNKNOWN, `4` INVALID,
`5` UNSUPPORTED. Only an overall PASS exits 0.

**Regression.** `r8_cli_exit_codes_are_documented` runs the built binary
(`CARGO_BIN_EXE_concir-backend`) and asserts `0/1/3/4/5`.

---

## R9 — engine disagreement on a body-less entry (P2)

**Root cause.** The interpreter created an empty frame for a transparent
body-less entry and then fell off the end (`E602/INVALID`); the net completed it
(`PASS`).

**Fix.** Both engines apply the same policy: a transparent body-less entry
(`is_nobody && is_transparent_nobody`) starts `Finished`; a body-less entry with
declared effects/blocking/return is rejected as `Unsupported` by the entry
check in `verify_program`.
`MachineState::initial` (`src/interp/state.rs`) and `PetriEngine::initial` both
complete a transparent entry.

**Regression.** `r9_bodyless_entry_is_transparent_in_both_engines` (PASS both,
complete), `r9_unused_unsupported_resource_does_not_block` (declared-but-unused
`RwLock` does not block), `r9_runtime_invalid_is_invalid_in_both_engines`
(INVALID both).

---

## R10 — the differential test compared only shared snapshots (P1, acceptance gap)

**Root cause.** `project_it`/`project_pn` were defined but unused; only
`shared_it`/`shared_pn` were compared, with no completeness assertion and no
edge relation.

**Fix.** `tests/petri_projection.rs` is replaced by `tests/differential.rs`,
which:
- first asserts both explorations are complete with **no** boundary events,
  `Invalid`, `Unsupported`, or truncation;
- compares a full normalization: shared data, mutex owner, concrete lock/sem/
  channel/condvar wait relations, messages with values, control positions, call
  stacks, frame locals, frame handle→child-thread bindings, return
  continuations, scopes, completion monitors, and reached/blocked/finished
  facts;
- compares the **labeled transition relation** `(source-projection, label,
  target-projection)`, not merely a reachable-state set;
- projects auxiliary net steps (wait-acquire / grant / delivery) onto the
  blocked thread's CIR statement — the interpreter's resume step — and treats
  registration/pairing phases explicitly;
- checks invariance under declaration reordering, and captures shared errors
  with hand-written expectations (R1 and R4 are asserted to be `FAIL`).

**Actual.**
```
differential_matches_producer_consumer ........................... ok
differential_matches_state_machine ............................... ok
differential_matches_semaphore_and_call .......................... ok
differential_matches_buffered_channel_and_rendezvous ............. ok
differential_matches_nested_handles .............................. ok
differential_matches_notify_choice ............................... ok
differential_is_invariant_under_declaration_reordering ........... ok
differential_catches_shared_error_with_hand_written_expectation .. ok
```

The engines share only AST/types/value evaluation/bounds/monitors; the
synchronization transition code (`src/interp/exec.rs` vs `src/petri/exec.rs`)
is separate.

---

## Commands and raw results

```bash
cargo build --bins
cargo test --test round2_regressions --test differential \
           --test semantics_regression --test repair_e2e \
           --test validator_risks --test interp_petri_diff --no-fail-fast
```

Reviewer probes (run against the new binary in a `/tmp` copy so the original
evidence directory is untouched):

```
notify_all_without_wait   interp PASS 3 / petri PASS 3
nested_handle_names       interp PASS 18 / petri PASS 29
finite_call_loop          interp PASS 6 / petri PASS 6
invalid_protected_write   INVALID both (exit 4)
ignored_assumptions       UNSUPPORTED both (exit 5)
nested_handle_false_pass  FAIL both (exit 1)
nested_handle_renamed_control FAIL both
notify_choice_false_pass  FAIL both
runtime_invalid_exit      INVALID both (exit 4)
bodyless_entry            PASS both
unused_unsupported        PASS both
forbidden_delete          no_acceptable_candidate (exit 1)
forbidden_reorder         no_acceptable_candidate (exit 1)
stable_sid_deleted        no_acceptable_candidate, "cannot be re-bound"
fresh_resolve_deleted_sid INVALID (exit 4)
```

## Independence and finiteness

- The interpreter and the net implement their synchronization transitions
  separately. Both were changed for R1/R4/R5/R9, and the full-projection +
  edge-relation differential is the check that one fix did not land in only one
  engine.
- Finiteness for `finite_call_loop` comes from contract-driven monitor
  saturation, not from dropping state that affects behaviour. Allocation
  counters remain excluded from equality (identities are internal); frames are
  removed on return; `completed_scopes`/`reached` are finite sets. Unbounded
  data, recursion depth, and search budget still yield `UNKNOWN`.

---

## Remaining / still open

1. **Single-origin step labels.** A rendezvous step still records one origin;
   the two participating instances are not both represented in `StepLabel`.
   This does not affect any current property or the differential (which
   projects pairing onto the CIR statements), but it limits diagnostic detail
   for cross-instance actions.
2. **Diagnostic-driven / multi-site repair.** `RepairContext` does not carry the
   previous round's structured diagnostics, and repair still enumerates
   independent single-site candidates. Combinatorial repairs are out of scope
   for this round.
3. **Unsupported surface unchanged.** `RwLock`, `select`, `async_call`/`await`,
   `abstract_step`, `seq_hole`, async condvar, float control flow, and channel
   close remain `Unsupported` by design.
4. **Pre-existing DOT snapshot failures.** `tests/dot_export.rs` cannot pass
   because `**.snap` is git-ignored; left untouched and not masked.
5. **Fairness.** No fairness/starvation liveness; `AG EF` remains
   reachability-preservation.

No formal proof is claimed; this is a reviewable implementation with executable
evidence.
