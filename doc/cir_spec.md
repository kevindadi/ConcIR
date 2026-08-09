# ConcIR Specification Reference

This file is the short index for the canonical ConcIR schema. The executable definition is `src/ast.rs` plus `src/validate/`; the complete JSON reference and examples are maintained in [`../README.md`](../README.md). LLM prompts must follow the same contract.

## Canonical shape
```json
{
  "program": "<name>",
  "resources": [],
  "protection": [],
  "functions": [],
  "entry": "<function>",
  "goals": []
}
```

Required top-level fields are `program`, `resources`, `protection`, `functions`, and
`entry`. `goals` defaults to an empty array when omitted.

## Semantic inventory

- `Resource`: `sync` resources are `Mutex`, `RwLock`, `Condvar`, `Semaphore`, or `Channel`; `var` resources are `Var` or `Atomic`. Sync resources require `mode`. `Semaphore` requires `count`; `Channel` requires `base`; `Var` and `Atomic` require `base` and `init`.
- `Function`: `name`, `kind` (`normal`, `async`, or `closure`), and a body. An empty `body` marks a body-less ("nobody") function with no control flow and no callsites; it is modeled as a trivial skeleton when referenced. Optional `effects: { reads, writes }` attach computation hints to that skeleton.
- `Statement`: `{ "sid", "op", "transfer" }`, where `sid` is unique within the function and has the form `s` followed by digits.
- `Op`: `res_op`, `spawn`, `spawn_async`, `join`, `await`, `call`, `return`, or `nop`.
- `Transfer`: `next`, `branch`, `switch`, or `return`.
- `BusinessGoal`: optional `id`, `desc`, `marking`, and `variables` postconditions. Marking keys are resource names, `function.sid`, or raw `cp_`/`rp_`/`wp_`/`ra_` place ids. Display forms such as `cp(worker, ret)` are not valid. `res_op` action tuples are strict: `lock`, `drop`, `read`, `notify`, `notify_all`, `acquire`, `release`, `recv`, and `load` take no arguments; `write`, `store`, and `send` take one; `wait` takes the associated mutex name; and `cas` takes expected and desired values. Unknown actions and extra or missing arguments are validation errors.

## Scope boundary

Channel capacity, message payload identity/FIFO order, dynamic thread identities, function parameters and return values, cancellation/timeouts, memory ordering, fairness, exceptions, I/O, and arbitrary data-structure mutation are outside the current ConcIR contract. The representation is intended for finite-state control-flow and synchronization verification, not as a general-purpose concurrent language.
