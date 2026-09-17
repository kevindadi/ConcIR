# CODE_REVIEW_ROUND5

Response to the review of commit `df62ffd`
(`round3-working-tree` is round 4; this is the follow-up `df62ffd` review).
One P1 (D1) plus the remaining C1 acceptance evidence.

The reviewed evidence directory is untouched; probes were run from a copy in
`/tmp`, and the D1 fixtures live in `tests/repro_round5/`.

## Gate

```
round5_regressions ....... 6 passed
round4_regressions ....... 9 passed
round3_regressions ...... 15 passed
round2_regressions ...... 18 passed
differential ............. 9 passed
semantics_regression .... 13 passed
repair_e2e ............... 4 passed
validator_risks .......... 8 passed
interp_petri_diff ........ 4 passed
(all pre-existing validator tests) ok
dot_export ............... 4 failed  (pre-existing)
```

Total: **192 passed / 4 failed** (baseline was 186/4; the 4 failures are the
same pre-existing DOT snapshots).

Reviewer D1 probe re-run against the new binary (copy in `/tmp`):

```
r4_return_domain_wide_dst  interp FAIL 2 / petri FAIL 2  (complete)
r4_return_domain_discard   interp FAIL 2 / petri FAIL 2  (complete)
r4_return_domain_entry     interp FAIL 1 / petri FAIL 1  (complete)
r4_return_domain_valid     interp PASS 2 / petri PASS 2  (complete)
```

---

## D1 — return path only checked the caller `dst` (P1)

**Root cause.** `return` evaluated the value and only checked whether it fit the
caller's destination. The callee's own resolved `SemFunction.returns` type was
never consulted, so a function declaring `returns: Int[0,1]` could return a
shared `Int = 2` into a wide local, into no destination, or from the entry
function, and still be recorded as completed.

**Fix.**
- `src/interp/exec.rs::SemOp::Return`: after evaluating the value, check it
  against `function.returns.ty` (the current callee) with the recursive
  `within_type`, then separately check the caller `dst`. Both checks run before
  popping the frame, writing, recording completion, or waking join/scope.
- `src/petri/exec.rs::ReturnInner` / `ReturnFinal`: the same check against the
  frame's declared `returns` type before unwinding/finishing.

**Correct expectations and results.**
- `r4_return_domain_wide_dst` (wide `Int` dst) ⇒ `FAIL`, both engines, complete.
- `r4_return_domain_discard` (call omits `dst`) ⇒ `FAIL`, both engines.
- `r4_return_domain_entry` (entry returns the out-of-domain value) ⇒ `FAIL`.
- `r4_return_domain_valid` (in-domain value) ⇒ `PASS`.

**Atomicity.** `tests/round5_regressions.rs::d1_disabled_return_is_atomic`
asserts the caller never reaches its next statement (`StatementReached main::s2`
is unreachable) and no `function_completed` is recorded, so the disabled return
left no unwind, write, or wake behind.

**Additional coverage** (`d1_nested_and_composite_returns`): a nested call chain
(`main -> mid -> two`) where `two`'s declared bounded return is disabled; a
composite `Struct{n: Int[0,1]}` return from an out-of-domain `Struct{n: Int}`
(disabled) and its in-domain counterpart (completes). The existing caller-`dst`
constraint is preserved.

`call`'s valid way to ignore a result remains omitting `dst`; an explicit
`dst = "_"` on `call` is statically `INVALID` and was not used as a fixture.

---

## C1 — remaining acceptance evidence

The review noted that the round-4 raw oracle only compared predicate truth for
two fixed goals and did not test successor-quotient behavior or trace replay.
`tests/round5_regressions.rs` now adds:

1. **Explicit identity translation** —
   `c1_identity_translation_preserves_key_predicates_and_successors` clones a
   reachable state and offsets *every* `ThreadId`, `FrameId`, `ScopeId`, and
   `HandleId` occurrence (frames, stacks, handle tables, child maps, scopes,
   mutex owners, channel/condvar/semaphore wait sets, blocked reasons,
   allocation counters), then asserts `state_key`, predicate truth, and the set
   of `(origin, successor-key)` actions are unchanged. Done independently for
   the interpreter and the Petri net.
2. **Quotient edge replay** — `c1_quotient_edges_are_executable` walks every
   stored edge of the production reachable graph and asserts an enabled step of
   the source realizes the same transition origin and target key, so the stored
   counterexample edges are executable.
3. **Merge consistency on a bounded raw BFS** —
   `c1_raw_oracle_successor_quotient` builds the graph with the raw `State`
   `Eq`/`Hash` (never the production key), groups by `state_key`, and asserts
   merged states agree on predicate truth and on successor actions.

The documented coverage is dynamic-identity alpha-equivalence. The review's
point is accepted: this is **not** a proof that arbitrary graph isomorphism
collapses to one key, and no such claim is made. The tests separate the two
failure directions (equal key with divergent behavior = incorrect merge; a
translation-equivalent state with a different key = missed merge for that
translation). `doc/backend-design.md` §4.1 was corrected accordingly (the
previous text implied successor-quotient behavior was already checked).

The value-key injectivity tests, the dynamic-loop finiteness, and the original
C1 collision negatives are unchanged and still pass.

---

## Remaining / still open

1. **Single-origin step labels** for rendezvous.
2. **Diagnostic-driven / multi-site repair** out of scope.
3. **Unsupported surface unchanged** (`RwLock`, `select`, async, holes, async
   condvar, float control, channel close).
4. **Unstructured spawn without join** still grows and yields `UNKNOWN`.
5. **Pre-existing DOT snapshot failures** (`**.snap` git-ignored).
6. **Fairness** not modeled.
7. **Identity equivalence** is proven only for explicit translation/renaming;
   arbitrary-graph-isomorphism collapse and the absence of every possible
   missed merge are not claimed.

No formal proof is claimed; this is a reviewable implementation with executable
evidence.
