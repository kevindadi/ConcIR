# CODE_REVIEW_ROUND3

Response to the third independent review of commit `f3463d6`
(`REVIEW.zh.md`). Item by item: root cause, fix locations, correct expectations
before/after, actual commands and results, finiteness rationale, and remaining
limits.

The reviewed evidence directory is untouched. The review probes were run
against a copy in `/tmp`; the new fixtures live in `tests/repro_round3/`.

## Gate

```
round3_regressions ....... 15 passed
round2_regressions ....... 18 passed
differential .............  9 passed
semantics_regression ..... 13 passed
repair_e2e ...............  4 passed
validator_risks ..........  8 passed
interp_petri_diff ........  4 passed
(all pre-existing validator tests) ok
dot_export ............... 4 failed  (pre-existing; see "Remaining")
```

Reviewer probes against the new binary (copy in `/tmp`):

```
r2_query_only_Var            interp PASS / petri PASS
r2_query_only_Var_negated    interp FAIL / petri FAIL
r2_query_only_Atomic         interp PASS / petri PASS
r2_query_only_Atomic_negated interp FAIL / petri FAIL
r2_query_only_repair         no_acceptable_candidate (patched FAIL)
r2_multi_lock_cv             interp PASS / petri PASS
r2_scope_loop                interp PASS 6 / petri PASS 6
r2_spawn_join_loop           interp PASS 10 / petri PASS 10
r2_semaphore_overflow        interp INVALID / petri INVALID (exit 4, E905)
r2_namespace_used_False/True PASS (both orderings)
r2_bounded_dst_fixed         FAIL both (disabled update)
r2_bounded_dst_with_place    FAIL both, complete
r2_fqn_scope_file/auto       repaired on main::t1
```

Rendezvous probe (`repro/rendezvous_probe.rs` against the new library):

```
interp: two waiting receivers, 2 successors; remaining [T2] and [T1]
petri:  two waiting receivers, 2 successors; remaining [T2] and [T1]
```

---

## B1 — Petri net omitted declared Var/Atomic (P1)

**Root cause.** The global resource loop created places only for Mutex /
Semaphore / Channel; Var/Atomic places came from read/write statements. A
variable observed only by a contract (or written only via a `dst`) had no
place; `read_data` returned `None`, `VarEq` defaulted to false, and its negation
became a false `PASS`. The same gap let a delete candidate be accepted.

**Fix.** `src/petri/net.rs::build` now creates a place for **every** declared
`Var` and `Atomic` (as it already did for the other sync kinds), and
`initial` initializes them. `src/petri/exec.rs::write_dst` no longer needs to
invent a place for a `dst`-only write.

**Regression.** `tests/round3_regressions.rs::b1_contract_only_resources_are_materialized`
and `b1_repair_must_reject_deleting_the_last_write`:
- query-only `Var`/`Atomic` with `AG(x==0)` ⇒ `PASS`; the negated version ⇒
  `FAIL`, both engines, complete.
- the delete-the-write repair candidate ⇒ `no_acceptable_candidate`; the
  patched program is `FAIL` in both engines (the initial `x=0` still violates
  the contract).

---

## B2 — Condvar lock association was a single global value (P1)

**Root cause.** `CondvarState.lock: Option<ResourceId>` was overwritten by every
`wait`, and `notify`/`notify_all` made all waiters re-acquire the last lock.
The Petri token already carried a per-waiter lock, so the interpreter reported
`INVALID` (E510) where the net reported `PASS`.

**Fix.** `src/interp/state.rs` stores `waiters: VecDeque<(ThreadId, ResourceId)>`;
`src/interp/exec.rs` records each waiter's own lock, and `notify`/`notify_all`
queue each waiter to re-acquire its own lock. The canonical renderer and the
differential projection render `(thread, lock)` pairs.

**Regression.** `b2_condvar_waiters_carry_their_own_locks`: `r2_multi_lock_cv`
is `PASS` and complete in both engines.

---

## B3 — Rendezvous dropped non-head matches (P1)

**Root cause.** The interpreter paired with `pending_recv/pending_send.pop_front()`
and the net's `SendPair`/`RecvPair` used `Binding::ControlWait`, which only
bound the front waiter. Two waiting receivers therefore produced a single
successor.

**Fix.** `src/interp/exec.rs` `ChannelSend`/`ChannelRecv` (cap 0) build one
successor per waiting partner; `src/petri/net.rs` builds `SendPair`/`RecvPair`
with `Binding::ControlChooseWait`, and `src/petri/exec.rs` enumerates every
wait token. Sender tokens keep their frozen value.

**Regression.** `b3_rendezvous_enumerates_every_receiver` is an **independent**
expectation (not engine agreement): at the two-receiver state both engines must
produce exactly 2 successors with different remaining waiters. The reviewer's
Rust probe confirms `2` for both.

---

## B4 — Contract short names bound to `ModuleId(0)` (P1)

**Root cause.** `ContractSpec::resolve` used `ModuleId(0)` for unqualified
names; reordering `modules` silently changed the verified object.

**Fix.** `src/sem/program.rs` adds `entry_module()`, and
`src/explore/contract.rs::resolve` binds unqualified names to the entry module's
namespace. Fully-qualified names resolve exactly.

**Regression.** `b4_contract_names_bind_to_the_entry_module` (both module
orders `PASS`) and `b4_declaration_reordering_keeps_a_real_target` (function and
resource reordering with a contract that references `x`; a non-entry-module
`x = 1` is not observed).

---

## B5 — `dst` writes bypassed bounded-Int domains (P1)

**Root cause.** `write_dst` inserted values directly; only explicit
assignments/stores called `within_type`. `atomic_load a -> x` could put `2`
into `x : [0,1]`.

**Fix.** Both engines gained a checked destination write
(`dst_type_ok`/`try_write_dst` for the interpreter; `write_dst -> bool` for the
net). It is applied to `read_shared`/`atomic_load`/`atomic_cas` `dst`,
`channel_recv` `dst`, `call` arguments, `call` returns, and the channel payload
on send. A domain violation disables the whole step, before any message is
consumed or a frame unwound.

**Regression.** `b5_bounded_dst_paths_respect_the_domain`:
- `r2_bounded_dst_with_place` ⇒ `FAIL` both, complete (was a false `PASS`);
- a bounded `channel_recv dst` and a bounded `call` return dst both make the
  out-of-domain goal unreachable (`FAIL`), both engines.

---

## B6 — Historical threads/scopes/handles kept the graph infinite (P2)

**Root cause.** Finished threads stayed in `threads`/`finished`; a joiner that
was woken by `finish_thread` left the child and the `handle_children` mapping
behind; scope members were never reclaimed; and identity counters (thread,
frame, handle) re-entered the state key. `scope(worker); goto` and
`spawn; join; goto` grew without bound.

**Fix.**
- `src/interp/exec.rs` / `src/petri/exec.rs`: on a successful `join` (whether
  the child was already finished or woke a blocked joiner) the handle binding,
  the child mapping, and the child are reclaimed; a completed scope reclaims
  its members and its record. A second `join` on a consumed handle is a defined
  `Invalid`.
- `src/explore/mod.rs`: the reachable graph deduplicates by the engine's
  **fully identity-normalized canonical form** (`TransitionSystem::canonical`),
  which renames threads, frames, scopes, handles, and child bindings to a dense
  order. The renderers (`src/interp/exec.rs::render_state`,
  `src/petri/exec.rs::canonical`) were rewritten to normalize these identities
  and to include frame handles, return continuations, and child mappings.

**Regression.** `b6_finite_concurrent_loops_complete`:
```
scope_loop:       interp PASS 6 states complete; petri PASS 6 complete
spawn_join_loop:  interp PASS 10 states complete; petri PASS 10 complete
```
plus `b6_nested_scopes_are_reclaimed`, `b6_stale_handle_second_join_is_invalid`,
and `b6_completion_threshold_saturates` (AtLeast(2) `PASS`, AtLeast(3) `FAIL`).
Genuine data/recursion/budget growth still yields `UNKNOWN`.

**Finiteness rationale.** Only internal numbering is factored; every fact that
affects future behavior or an observed predicate (locals, control, wait
relations, messages, monitors, reached facts) remains in the state. This is
identity alpha-equivalence, not deletion of behavior.

---

## B7 — Patch scope compared short function names (P2)

**Root cause.** `PatchScope::allows_function(name)` compared only the short
name; `functions=["main::t1"]` never matched, and `other::t1` was not excluded.

**Fix.** `PatchScope::allows_function(module, function)` matches an FQN entry as
exact `module::function` (a bare entry is a legacy short name). `check_allowed`
and `LockOrderEnumerator` pass both module and function. `CODE_REVIEW_ROUND2.md`
was corrected.

**Regression.** `b7_fqn_patch_scope_is_exact` (file and automatic providers both
repair `main::t1`) and `b7_patch_scope_excludes_other_module_same_function`
(scope `other::t1` rejects the `main::t1` file candidate and the automatic
enumerator yields 0 candidates).

---

## B8 — `SemaphoreRelease` could panic (P2)

**Root cause.** `available + count` overflowed.

**Fix.** `src/interp/exec.rs` and `src/petri/exec.rs` use `checked_add` and
return structured `Invalid` (`E905`) with location; both debug and release
report it as JSON, exit code 4.

**Regression.** `b8_semaphore_overflow_is_structured_invalid`: both engines
`INVALID`, `E905`, CLI exit 4.

---

## Strengthened independent acceptance (review §3)

- **Differential projection keeps handle bindings.** `tests/differential.rs`
  now projects `(handle name, child ordinal)` pairs (not a sorted child set) in
  both engines, so `h1→a,h2→b` and `h1→b,h2→a` differ. Resource iteration is
  name-sorted so declaration reordering is comparable.
- **Declaration reordering with a real target.**
  `differential_is_invariant_under_resource_and_function_reorder` and the B4
  regression assert stability for a contract that references the resource.
- **Independent expectations.** B1, B3, B5 and the completion threshold are
  asserted as hand-written expectations, not as engine agreement.
- **Early-exit metadata.** `VerificationReport` gained `analysis_started`; an
  early `Invalid`/`Unsupported` exit records the *requested* assumptions/bounds
  (from the spec) rather than a default that was never used
  (`early_exit_reports_requested_config_not_default`).
- **Completeness gate.** Every differential comparison asserts both engines are
  complete with no `Invalid`, `Unsupported`, or boundary events before
  comparing states and edges.

Reviewer probes and the independent rendezvous probe are reproduced above.

---

## Remaining / still open

1. **Single-origin step labels.** A rendezvous step still records one origin;
   the two participating instances are not both represented in `StepLabel`.
2. **Diagnostic-driven / multi-site repair.** `RepairContext` still does not
   carry the previous round's diagnostics; repairs are independent single-site
   candidates.
3. **Unsupported surface unchanged.** `RwLock`, `select`, `async_call`/`await`,
   `abstract_step`, `seq_hole`, async condvar, float control flow, channel
   close.
4. **Unstructured spawn without join** still accumulates a finished child
   (there is no handle to consume it); that is a genuine program leak and
   surfaces as `UNKNOWN`, not a false `PASS`.
5. **Pre-existing DOT snapshot failures** (`**.snap` git-ignored); left
   untouched and not masked.
6. **Fairness.** No fairness/starvation liveness; `AG EF` remains
   reachability-preservation.

No formal proof is claimed; this is a reviewable implementation with executable
evidence.
