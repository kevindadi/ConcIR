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

# List enabled steps from the initial state (reference interpreter).
./target/release/concir-backend run examples/producer_consumer.json
```

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

## Reproducible checks

```bash
cargo test --test semantics_regression
cargo test --test validator_risks
cargo test --test interp_petri_diff
cargo test --test petri_projection
cargo test --test repair_e2e
```

Note: the pre-existing `tests/dot_export.rs` snapshot tests cannot pass on a
fresh checkout because `**.snap` is git-ignored (no committed snapshots). This
is unrelated to the backend and left untouched.
