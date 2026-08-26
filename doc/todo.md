# ConcIR Roadmap

Status legend: `[x]` done · `[ ]` planned · `[~]` in progress

## Modeling scope

- `[x]` FnSummary removed; body-less ("nobody") functions as codegen placeholders with optional `effects` hints
- `[x]` `call` expands into the callee skeleton (entry/return places) so cross-function lock chains enter the model; calling a body-less callee is an atomic pass-through
- `[x]` Modules as the program unit; entity names are ConcIR FQNs (`module::entity`), not backend crate paths
- `[x]` Flattened CFG: function body is a statement list; non-control ops fall through; `goto`/`branch`/`switch`/`return`/`select` are statement kinds; no terminator object; loops are back-edges
- `[x]` Bounded typed data flow with projection: function `params`/`returns`/`locals`
  with a `modeled` flag
- `[x]` `assign_local` for writable intermediate values
- `[x]` Bounded `Int` value ranges — `{"Int": [lo, hi]}`
- `[x]` Channel `capacity` required on the resource (message store; `0` = rendezvous, `n ≥ 1` = bounded buffer of `n` payload slots)
- `[x]` Structured concurrency: `kind: "scope"` is a fork-join region (`spawn` = fork, `return` / `join_all` = join); `spawn_batch` enters a named scope; homogeneous N-way spawn is a loop
- `[ ]` Channel capacity in the CVN — bounded-buffer semantics from the `capacity` field
- `[ ]` Dynamic thread identities / thread-local state — multiple spawns of the same function share control places (multi-token abstraction)

## Call semantics

- `[~]` **Return-continuation ambiguity**: when two call sites call the same bodied function and are concurrently active, the shared return token can hand back to either call site. Accepted as an over-approximation today
- `[ ]` Document and enforce the "one function not concurrently held by two callers" restriction, or model reentrancy explicitly
- `[x]` `select` + `condvar_wait` is E409 unless the function is `async` and the Condvar is `mode: Async`; translator maps that guard to Notify/watch/timeout race

## Modular generation

- `[x]` Native `Module` with `provides` / `requires` as `{ resources, functions }`
- `[x]` FQN rules: same-module short name; cross-module FQN listed in `requires`; `entry` is an FQN
- `[x]` Schema-level module validation (E108)
- `[ ]` Concatenate independently authored module files into one `Program`

## Validation & diagnostics

- `[ ]` Keep every emitted error code documented in `error_codes.md`; add a test that asserts the code set in code matches the documented set
- `[ ]` Expression language — extend beyond the current literal/`+ - * / %`/comparison subset

## Repo hygiene

- `[ ]` Doc auto-check in CI: verify `doc/syntax/`/`error_codes.md` stay in sync with `src/ast.rs` and `src/validate/`
