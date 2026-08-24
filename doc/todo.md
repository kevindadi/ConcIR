# ConcIR Roadmap

Status legend: `[x]` done · `[ ]` planned · `[~]` in progress

## Modeling scope

- `[x]` FnSummary removed; body-less ("nobody") functions as codegen placeholders with optional `effects` hints
- `[x]` `call` expands into the callee skeleton (entry/return places) so cross-function lock chains enter the model; calling a body-less callee is an atomic pass-through
- `[x]` Optional `Function.module` for modular-fragment provenance
- `[x]` Bounded typed data flow with projection: function `params`/`returns`
  with a `modeled` flag; modeled values materialize as namespaced CVN
  variables (`p_{fn}_{param}`, `r_{fn}_{ret}`), bound at `call` and captured
  via the call out-var; unmodeled values never enter the net. Guards on modeled
  params resolve to the materialized variable
- `[ ]` Per-function `locals` and an assignment op — writable intermediate
  values (params/returns cover the function-signature data flow already)
- `[x]` Bounded `Int` value ranges — `{"Int": [lo, hi]}`; a variable update
  leaving the domain disables its transition, so counter loops terminate at the
  bound and the state space stays finite (unbounded Int previously exploded the
  state space)
- `[ ]` Channel capacity modeling — the `Channel` resource has no capacity field today; `send`/`recv` are unbuffered-token semantics. A bounded-channel `capacity` field would close the gap with sync/async channel backends
- `[ ]` Dynamic thread identities / thread-local state — multiple spawns of the same function share control places (multi-token abstraction). A per-thread token color would remove the call-return continuation ambiguity noted below

## Call semantics

- `[~]` **Return-continuation ambiguity**: when two call sites call the same bodied function and are concurrently active, the shared `cp_f_ret` return token can hand back to either call site (no stack/thread identity). Accepted as an over-approximation today; a policy or a per-callsite return marker would make it sound
- `[ ] ` Document and enforce the "one function not concurrently held by two callers" restriction, or model reentrancy explicitly

## Modular generation

- `[x]` Native `Module` declaration (same payload as `Program`, plus `name` / `provides` / `requires`)
- `[ ]` Concatenate `Module` fragments into one `Program` (unique function names and goal ids, consistent shared resources, single entry owner); stamp `Function.module`
- `[ ]` Schema-level module validation — `provides`/`requires` and shared-resource contracts, so the validator can catch interface drift before concatenation

## Validation & diagnostics

- `[ ] ` Keep every emitted error code documented in `error_codes.md`; add a test that asserts the code set in code matches the documented set
- `[ ] ` Expression language — extend beyond the current literal/`+ - * / %`/comparison subset (field access, boolean connectives, function calls in expressions)

## Repo hygiene

- `[ ] ` Doc auto-check in CI: verify `syntax.md`/`error_codes.md` stay in sync with `src/ast.rs` and `src/validate/`
