# CODE_REVIEW_ROUND4

Response to the review of the round-3 working tree
(`round3-working-tree/REVIEW.zh.md`). Item by item: root cause, fix locations,
correct expectations, actual commands and results, and remaining limits.

The reviewed evidence directory is untouched; probes were run from a copy in
`/tmp`, and the new fixtures live in `tests/repro_round4/`.

## Gate

```
round4_regressions ....... 9 passed
round3_regressions ...... 15 passed
round2_regressions ...... 18 passed
differential ............. 9 passed
semantics_regression .... 13 passed
repair_e2e ............... 4 passed
validator_risks .......... 8 passed
interp_petri_diff ........ 4 passed
(all pre-existing validator tests) ok
dot_export ............... 4 failed  (pre-existing; see "Remaining")
```

Total: **186 passed / 4 failed** (baseline was 177/4; the 4 failures are the
same pre-existing DOT snapshots).

Reviewer probes re-run against the new binary (copy in `/tmp`):

```
r3_channel_domain_0      interp FAIL 3 / petri FAIL 3   (complete)
r3_channel_domain_1      interp FAIL 3 / petri FAIL 3   (complete)
r3_canonical_collision_A interp PASS 16 / petri PASS 16 (complete)
r3_canonical_collision_B interp PASS 16 / petri PASS 16 (complete)
r3_canonical_false_pass  interp FAIL 16 / petri FAIL 16 (complete)
r3_nested_domain         interp FAIL 1 / petri FAIL 1   (complete)
```

Reviewer raw oracle (independent raw-`State` `Eq`/`Hash` BFS, no production
key/renderer for the graph):

```
interp-A: raw_states=16 goal_reachable=true equal_key_with_different_predicate_truth=0
petri-A:  raw_states=16 goal_reachable=true equal_key_with_different_predicate_truth=0
interp-B: raw_states=16 goal_reachable=true equal_key_with_different_predicate_truth=0
petri-B:  raw_states=16 goal_reachable=true equal_key_with_different_predicate_truth=0
```

---

## C1 — display string used as the state key (P1)

**Root cause.** `explore`/`verify` deduplicated by `system.canonical(state)`,
whose `Value::Str` rendering did not escape `"`, so the two distinct structs
`{a:"X\",b:\"Y", b:"Z"}` and `{a:"X", b:"Y\",b:\"Z"}` produced the same key.
The merge dropped the `A` state, turning `EF(A)` into a false `FAIL` and
`AG(!A)` into a false `PASS`.

**Fix.**
- `src/sem/value.rs`: added `Value::key()` — a type-tagged, length-prefixed
  encoding (`T<len>:…`, `S<n>[…]`, `A<n>[…]`, `I<i>;`, `B0/1`, `E<len>:…`,
  `F<bits>;`) that is injective. `write_canonical` now JSON-escapes strings and
  field names so the display text is unambiguous too.
- `src/sem/system.rs`: `TransitionSystem` gained a required
  `state_key(&self, state) -> String`, documented as the semantic dedup key;
  `canonical` remains the diagnostics renderer.
- `src/interp/exec.rs::{render_state, canonical, state_key}` and
  `src/petri/exec.rs::{render_net, canonical, state_key}`: one identity-
  normalizing renderer parameterized by a value encoder; `state_key` uses
  `Value::key`, `canonical` uses `Value::canonical`.
- `src/explore/mod.rs`: the reachable graph indexes by `state_key`.

The identity normalization and lifecycle reclamation from round 3 are
unchanged, so B6 finiteness is preserved.

**Regression.** `tests/round4_regressions.rs`:
- `c1_canonical_collision_no_longer_merges_states`: `EF(A)` and `EF(B)` ⇒
  `PASS`, `AG(!A)` ⇒ `FAIL`, both engines.
- `c1_value_key_is_injective_for_the_colliding_structs`: the two structs have
  different `Value::key()` and different `canonical()`; nested/odd strings too.
- `c1_raw_state_oracle_agrees_with_production_key`: an independent BFS over the
  raw `State` `Eq`/`Hash` confirms both goals are reachable and that no two raw
  states share a `state_key` while disagreeing on the goal.
- `c1_cli_reports_fail_for_the_false_pass`: CLI exit code 1.

---

## C2 — Petri channel payload domain not checked (P1)

**Root cause.** The interpreter checked the channel base on `channel_send`, but
the Petri send/registration/delivery paths did not, so an out-of-domain payload
was accepted (and a `dst = "_"` receiver could not mask it).

**Fix.** `src/petri/exec.rs` checks the channel's own base (`payload_ok`) in
`SendRegister`, `SendPair`, `RecvPair`, `Rendezvous`, `SendBuf`,
`SendBufBlock`, and `BufferDeliver`. The check runs before any token is
consumed, control advanced, or delivery performed. `SendPair` was reordered to
evaluate and validate the payload before taking the receiver token.

**Regression.** `c2_channel_payload_domain_is_enforced`: both capacities `FAIL`
and complete in both engines. `c2_valid_channel_payload_still_flows`: an
in-range payload still flows without deadlock.

---

## C3 — `within_type` did not recurse into composites (P1)

**Root cause.** `within_type` returned `true` for every type except a
top-level bounded `Int`, and even a bounded `Int` accepted non-`Int` values.

**Fix.** `src/sem/value.rs::within_type` now recurses: exact primitives,
bounded-`Int` range, enum membership, struct field set + each field, and array
length + each element. All value-entry paths use it (initialization, stores,
`dst` writes, `call` arguments/returns, channel payloads).

**Regression.** `c3_within_type_recurses_into_composites` (unit cases),
`c3_nested_bounded_field_update_is_disabled` (`r3_nested_domain` ⇒ `FAIL`,
complete, both engines), and `c3_array_and_mixed_nesting_domains`
(`Array<Int[0,1]>` out-of-domain ⇒ `FAIL`; a legal `Struct<Array<bounded>>`
value still flows ⇒ `PASS`).

---

## Acceptance strengthening (review §3)

- **Raw-state oracle.** `c1_raw_state_oracle_agrees_with_production_key` uses
  only the engines' `initial`/`successors`/`satisfied` and a `HashSet` of the
  raw `State` (raw `Eq`/`Hash`); it does not call `state_key` or any renderer
  for the graph, and it asserts raw finiteness, goal reachability, and that no
  equal key disagrees on the goal. The reviewer's own oracle was also re-run
  (16 raw states, 0 conflicts).
- **Special strings / nested encodings.** The colliding structs and
  delimiter-like strings are asserted injective.
- **Channel capacities and blocking order.** Capacity 0 and 1 covered, plus a
  legal payload; the round-3 rendezvous test covers two waiting receivers.
- **Composite types and all write paths.** Nested bounded fields, arrays, mixed
  struct/array; negative and positive.
  (Round 5 corrected the claim that *every* return path checked the domain:
  only the caller `dst` was checked. The callee's declared `returns` type was
  added in `CODE_REVIEW_ROUND5.md` — D1.)
- **Not only engine agreement.** Each case asserts an independent expected
  outcome, exploration completeness, and (for C1) the CLI category.

---

## Remaining / still open

1. **Single-origin step labels** for rendezvous (both instances not both
   represented).
2. **Diagnostic-driven / multi-site repair** is still out of scope.
3. **Unsupported surface unchanged** (`RwLock`, `select`, async, holes, async
   condvar, float control, channel close).
4. **Unstructured spawn without join** still accumulates a finished child and
   yields `UNKNOWN`, not a false `PASS`.
5. **Pre-existing DOT snapshot failures** (`**.snap` git-ignored); untouched.
6. **Fairness** is not modeled.

No formal proof is claimed; this is a reviewable implementation with executable
evidence.
