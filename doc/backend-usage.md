# ConcIR backend: usage, support matrix, and migration

This page is the operator-facing companion to [`backend-design.md`](backend-design.md).
It documents what the non-LLM backend supports, the commands, the model
boundaries, and the static-validator changes.

## Commands

Build once:

```bash
cargo build --release
```

Static validation (unchanged, `concir` binary):

```bash
./target/release/concir examples/producer_consumer.json
```

Backend (`concir-backend` binary):

```bash
# Property verification. Optional contract JSON and engine (petri default).
./target/release/concir-backend explore examples/producer_consumer.json
./target/release/concir-backend explore examples/lockorder_bug.json examples/lockorder_contract.json
./target/release/concir-backend explore <program.json> <contract.json> interp

# Supportability report (prints every Unsupported construct).
./target/release/concir-backend support examples/complex_rwlock.json

# Deterministic, LLM-free repair. Optional candidate file and budget.
./target/release/concir-backend repair examples/lockorder_bug.json examples/lockorder_contract.json
./target/release/concir-backend repair <program.json> <contract.json> <patches.json> 16
./target/release/concir-backend repair <program.json> <contract.json> --strategy c --artifact out.json
./target/release/concir-backend replay out.json
./target/release/concir-backend bench [--artifact bench.json]

# List enabled steps from the initial state (reference interpreter).
./target/release/concir-backend run examples/producer_consumer.json
```

Exit codes: `0` PASS / repaired / already satisfied, `1` FAIL / no acceptable
candidate / budget exhausted, `2` usage or input error, `3` UNKNOWN, `4`
INVALID (static, semantic, or configuration), `5` UNSUPPORTED. `repair` uses
the same categories as `explore`.

Verification output is a JSON `VerificationReport` with an `outcome` of
`PASS` / `FAIL` / `UNKNOWN` / `INVALID` / `UNSUPPORTED`, per-property results,
structured diagnostics, boundary events, and unsupported notations.

## Support matrix

| Construct | Static validator | Reference interpreter | Petri net |
| --------- | :--------------: | :-------------------: | :-------: |
| `nop`, `assign_local`, `read_shared`, `write_shared` | yes | yes | yes |
| `atomic_load` / `atomic_store` / `atomic_cas` | yes | yes | yes |
| `mutex_lock` / `mutex_unlock` | yes | yes | yes |
| `channel_send` / `channel_recv` (`capacity = 0` and `>= 1`) | yes | yes | yes |
| `condvar_wait` / `condvar_notify` / `condvar_notify_all` | yes | yes | yes |
| `semaphore_acquire` / `semaphore_release` | yes | yes | yes |
| `call` / `return` (frames, returns) | yes | yes | yes |
| `spawn` / `join` / `scope` | yes | yes | yes |
| `goto` / `branch` / `switch` | yes | yes | yes |
| Bounded `Int`, `Bool`, `Enum`, `Struct`, arrays (values) | yes | yes | yes |
| Body-less ("nobody") function, no effects | yes | no-op | no-op |
| Body-less function with effects / blocking / return | yes | **Unsupported** | **Unsupported** |
| `rwlock_read` / `rwlock_write` / `rwlock_unlock` | yes | **Unsupported** | **Unsupported** |
| `select` | yes | **Unsupported** | **Unsupported** |
| `async_call` / `await` | yes | **Unsupported** | **Unsupported** |
| `abstract_step` | yes | **Unsupported** | **Unsupported** |
| `seq_hole` | yes | **Unsupported** | **Unsupported** |
| Async-mode `condvar_wait` | yes | **Unsupported** | **Unsupported** |
| Float used to decide a branch/switch | yes | **Unsupported** | **Unsupported** |
| Channel close / disconnect | n/a | **Unsupported** | **Unsupported** |
| Array indexing, `&&` / `||` in expressions | E931 | **Unsupported** | **Unsupported** |

`Unsupported` is returned as a structured value; it is never silently treated
as a no-op, and it never yields `PASS`.

## Model boundaries

- **Precise activation store.** The backend tracks every parameter, local, and
  return slot concretely (one copy per dynamic frame). The legacy `modeled`
  flag is a CVN projection hint and does not restrict the precise backend.
- **Bounded domains.** Bounded `Int` is enforced: a transition whose update
  would leave `[lo, hi]` is disabled. Unbounded `Int` can grow without bound;
  the analyzer's `max_states` then truncates the search and the result is
  `UNKNOWN`, never `PASS`.
- **Program limits vs analyzer limits.** Channel `capacity`, semaphore
  `count`, bounded `Int`, and function `bound` are semantics. `max_threads`,
  `max_frames_per_thread`, `max_states`, `max_depth`, and
  `max_boundary_events` are analyzer limits; hitting one is a recorded
  boundary event that makes the search incomplete.
- **Scheduling.** Sequentially consistent atomics and shared reads/writes. No
  spurious condvar wakeups. Progress never relies on spurious wakeups.
- **Fairness.** No fairness or starvation guarantees in v1. `AG EF` is
  reachability-preservation, not "eventually completes".

## Verification contract

See `doc/backend-design.md` §6 and `examples/lockorder_contract.json`. A
contract fixes the required properties, preserved behaviour, assumptions,
analysis bounds, and the allowed patch scope. It is immutable during repair.

## Structured patch and repair

`CirPatch` targets a `(module, function)`, records the target's content hash,
and carries structured changes with provenance. Supported changes:

- `swap_statements` — swap two adjacent, side-effect-free `mutex_lock`
  statements (the automatic lock-order repair).
- `delete_statement` — explicit candidate files only; rejected if it removes
  preserved behaviour or breaks static validation.

The repair loop is deterministic: legality → CIR static validation →
supportability → re-translation → **full** verification of every property and
preserved behaviour → accept/reject. Old-counterexample replay is not used as
an acceptance criterion.

### Diagnostic-driven composite search

```bash
# Strategy A (single legacy), B (composite), C (diagnostic, default).
concir-backend repair <program.json> <contract.json> --strategy a
concir-backend repair <program.json> <contract.json> --strategy c \
    --candidate-budget 64 --verification-budget 64 \
    --max-depth 4 --max-total-edits 4 --artifact out.json

# File-supplied candidates still use the single-edit legacy loop:
concir-backend repair <program.json> <contract.json> patches.json [budget]

# Replay a saved artifact: rebuild every node and re-verify the export.
concir-backend replay out.json

# Development benchmark (complete records for A/B/C):
concir-backend bench [--artifact bench.json]
```

The verification bounds come from the frozen contract (`contract.bounds`); the
`repair` flags control only the search budgets. `--verification-budget 0` is
rejected before any verification. Candidates are deduplicated by program
fingerprint *before* verification, so a repeated program never consumes
verification budget; the artifact reports proposals, unique programs,
verification calls, and cache hits separately.

`repair --strategy` prints the complete artifact to stdout (and writes
`--artifact` if given). The artifact (schema `concir-repair-artifact-v1`)
contains the input program, frozen contract, effective config and bounds, the
source identity (crate version plus a binary fingerprint), every node and
attempt with parent/patch relationships and full verification reports, the
patch chain, the accepted program and report, counts, the stop reason, and a
reproduce command. `replay` re-applies each patch to its parent, validates
fingerprints and the input, and re-verifies the accepted program; a broken
parent, patch base, or input fingerprint fails explicitly.

The search first verifies the original program: a complete `PASS` returns
`already_satisfied` and produces no patch; `invalid`/`unsupported`/`unknown`
roots are reported as such. Each node records its parent, incoming patch,
program fingerprint, and full verification report. Providers receive the node's
structured report; the diagnostic strategy uses blocked-resource facts (not
parsed text) to rank candidates. Intermediate `FAIL` nodes are search nodes, not
accepted repairs; only an overall complete `PASS` is accepted. Exhausting
candidates or budget means "not found under this strategy/budget", not "no
repair exists".

## Static-validator changes (migration)

The backend work fixed five risks in the existing validator. These add checks;
they do not change the JSON syntax.

| Change | Effect | Migration |
| ------ | ------ | --------- |
| **E309 extended** | `atomic_load` / `atomic_cas` / `channel_recv` / `read_shared` `dst` and `call` `dst` writing a protected `Var` now require the lock | hold the lock when a destination is a protected `Var`, or write to a local |
| **E512 new** | `condvar_wait` requires the paired lock to be held | lock before waiting |
| **`may_block` broader** | `channel_send`, `mutex_lock`, `rwlock_*` count as blocking; `may_block` now propagates through `call` | set `may_block: true`, or remove a wrong `may_block: false` |
| **`requires_held` is the entry condition** | a function that relies on the caller holding a lock is not flagged E309 | none; false positives disappear |
| **Module-aware protection** | protection lookups use `module::entity`, so same-named `Var`s in different modules no longer collide | none; fixes missed/extra E309 |

`E512` is documented in [`error_codes.md`](error_codes.md).

## Round-2 semantics and contract hardening

- **Waiting is enabledness, not a forced hand-off.** A mutex/semaphore/channel
  waiter becomes enabled when its condition holds, and every eligible waiter is
  an independent choice. Channel *messages* are FIFO; waiting *thread* order is
  not. `notify_one` enumerates every current waiter. No FIFO wake policy is
  forced on generic CIR primitives.
- **Per-frame handles.** Spawn/join handle names belong to the activation, so a
  callee cannot overwrite its caller's bindings. Renaming a callee-local handle
  does not change a property's result.
- **Contract-driven monitors.** Completion facts are saturated to what the
  contract observes, so a finite control loop (repeated calls, no growing data)
  yields a finite graph and a complete `PASS`. Unbounded data, recursion depth,
  and the search budget still produce `UNKNOWN`.
- **Unified checked entry.** The CLI and repair use
  `explore::verify_program`, which runs static validation, supportability,
  contract validation and binding, monitor derivation, exploration, and
  property checking. `sequential_consistency=false` or
  `no_spurious_wakeups=false` is `UNSUPPORTED`; malformed bounds/ids/predicates
  are `INVALID`; a statically invalid program is `INVALID`, never `PASS`.
- **Immutable symbolic contract.** The repair loop re-resolves the frozen
  `ContractSpec` against every candidate program, so statement/scope targets
  are re-bound. A deleted target rejects the candidate.
- **Unified patch permissions.** Every provider (including file candidates)
  passes the same `module::function` scope and per-change `allow_lock_reorder` /
  `allow_statement_delete` check. Duplicate candidates are detected by
  normalized change content; conflicting changes on one statement are rejected.
- **Exit codes.** `0` PASS/repaired · `1` FAIL · `2` usage · `3` UNKNOWN ·
  `4` INVALID · `5` UNSUPPORTED.
- **Reports** carry model and contract fingerprints, the assumptions used, the
  analysis bounds, and `analysis_started` (an early `Invalid`/`Unsupported`
  exit records the *requested* configuration, not a default never used).

## Round-3 corrections

- **Condvar waiters carry their own locks.** The wait set stores `(thread,
  lock)`; `notify`/`notify_all` queue each waiter to re-acquire its own lock.
  A condvar may therefore be waited on under different locks, and the
  interpreter and net agree.
- **Rendezvous enumerates every match.** With several waiting receivers (or
  senders) a single arrival yields one successor per candidate, carrying frozen
  message values; waiting threads are not served FIFO.
- **Every value-entry path respects the declared domain.** Explicit stores,
  `read_shared`/`atomic_load`/`atomic_cas` `dst`, `channel_recv` `dst`, `call`
  arguments, `call` returns, and channel payloads. A domain violation disables
  the whole step atomically (no partial consumption/unlock/unwind).
- **`SemaphoreRelease` uses checked arithmetic.** Overflow is structured
  `INVALID` (`E905`) with exit code 4, in both debug and release, not a panic.
- **Contract names bind to the entry module.** Unqualified contract names
  resolve in the entry module's namespace; fully-qualified names resolve
  exactly. Module/function/resource reordering is stable.
- **Patch scope is `module::function`.** `allowed_scope.functions=["main::t1"]`
  allows only `main::t1` (file and automatic providers) and excludes
  `other::t1`.
- **Finite identity.** Successful joins/scopes reclaim finished objects, and
  the explorer deduplicates by a fully identity-normalized canonical form, so
  finite spawn/join and scope loops complete in a small budget.

## Round-4 corrections

- **Semantic state key separated from display.** Deduplication uses
  `TransitionSystem::state_key`, a type-tagged, length-prefixed, identity-
  normalized encoding (`Value::key`). Distinct struct/string values can no
  longer collide (the old display string was unescaped). `canonical` is
  diagnostics-only and JSON-escaped.
- **Recursive composite domains.** `within_type` checks struct fields, array
  length and elements, enum membership and primitives, so a bounded member of a
  `Struct`/`Array` can no longer enter out of range.
- **Channel payload domains in both engines.** The Petri send paths
  (`SendRegister`, `SendPair`, `SendBuf`, `SendBufBlock`, `BufferDeliver`,
  `Rendezvous`) check the channel's own base before committing, independently
  of the receiver's `dst` (including `dst = "_"`).
- **Atomicity.** A disabled update leaves no token, message, lock, control
  advance, or frame pop behind; frozen sender values are never re-evaluated.

## Round-5 corrections

- **Declared return type is checked.** A `return` is checked against the
  callee's own `returns` type (recursively for composites) and then
  independently against the caller's `dst`. Omitting `dst` or returning from
  the entry does not bypass it. A violating return is disabled atomically: no
  frame pop, `dst` write, join/scope wake, or completion record.
- **C1 acceptance evidence.** The state-key equivalence is tested by an
  independent raw-`State` oracle, by an explicit dynamic-identity translation
  that updates every id reference, by replaying every stored quotient edge, and
  by a raw-BFS merge-consistency check. The documented coverage is
  dynamic-identity alpha-equivalence, not arbitrary graph isomorphism.

## Reproducible checks

```bash
cargo test --test round2_regressions
cargo test --test round3_regressions
cargo test --test round4_regressions
cargo test --test round5_regressions
cargo test --test differential
cargo test --test semantics_regression
cargo test --test validator_risks
cargo test --test interp_petri_diff
cargo test --test repair_e2e
cargo test --test repair_search
cargo test --test benchmark
cargo test --test dot_export
```

`tests/repro_round2/` … `tests/repro_round5/` contain the review
counterexamples (CIR, contracts, patches) as fixtures; `tests/repro_bench/`
contains the development-benchmark cases. See `CODE_REVIEW_ROUND2.md` …
`CODE_REVIEW_ROUND5.md` and `REPAIR_LOOP_HANDOFF.md`.

Toolchain used for the recorded results: `rustc 1.100.0-nightly`, on macOS.
The four historical DOT snapshot failures are fixed by committing the four
`tests/snapshots/*.snap` goldens and adjusting `.gitignore` to ignore only
pending snapshots (`*.snap.new`, `*.pending-snap`).

