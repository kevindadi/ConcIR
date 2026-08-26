# ConcIR Roadmap

Status legend: `[x]` done · `[ ]` planned · `[~]` in progress

## Modeling scope

- `[x]` FnSummary removed; body-less ("nobody") functions as codegen placeholders with optional `effects` hints
- `[x]` `call` expands into the callee skeleton (entry/return places) so cross-function lock chains enter the model; calling a body-less callee is an atomic pass-through
- `[x]` Modules as the program unit; entity names are ConcIR FQNs (`module::entity`), not backend crate paths
- `[x]` Compiler-IR layout: basic block = statements + (`call` \| `terminator`); `return` only as a terminator
- `[x]` Bounded typed data flow with projection: function `params`/`returns`/`locals`
  with a `modeled` flag; modeled values materialize as namespaced CVN
  variables (`p_{fn}_{param}`, `r_{fn}_{ret}`), bound at `call` and captured
  via the call `dst`; unmodeled values never enter the net
- `[x]` `assign_local` for writable intermediate values
- `[x]` Bounded `Int` value ranges — `{"Int": [lo, hi]}`; a variable update
  leaving the domain disables its transition
- `[x]` Channel `capacity` field on the resource (accepted; CVN still uses unbuffered-token `send`/`recv`)
- `[ ]` Channel capacity in the CVN — bounded-buffer semantics from the `capacity` field
- `[ ]` Dynamic thread identities / thread-local state — multiple spawns of the same function share control places (multi-token abstraction). A per-thread token color would remove the call-return continuation ambiguity noted below

## Call semantics

- `[~]` **Return-continuation ambiguity**: when two call sites call the same bodied function and are concurrently active, the shared `cp_f_ret` return token can hand back to either call site (no stack/thread identity). Accepted as an over-approximation today; a policy or a per-callsite return marker would make it sound
- `[ ]` Document and enforce the "one function not concurrently held by two callers" restriction, or model reentrancy explicitly

## Modular generation

- `[x]` Native `Module` with `provides` / `requires` as `{ resources, functions }`
- `[x]` FQN rules: same-module short name; cross-module FQN listed in `requires`; `entry` is an FQN
- `[x]` Schema-level module validation (E108) — provides/requires and unresolved imports
- `[ ]` Concatenate independently authored module files into one `Program` (unique module names, consistent shared resources, single entry)

## Validation & diagnostics

- `[ ]` Keep every emitted error code documented in `error_codes.md`; add a test that asserts the code set in code matches the documented set
- `[ ]` Expression language — extend beyond the current literal/`+ - * / %`/comparison subset (field access, boolean connectives, function calls in expressions)

## Repo hygiene

- `[ ]` Doc auto-check in CI: verify `syntax.md`/`error_codes.md` stay in sync with `src/ast.rs` and `src/validate/`
