# Function

```json
{
  "name": "main",
  "kind": "normal",
  "params": [],
  "locals": [],
  "body": [
    { "sid": "s1", "kind": "scope", "funcs": ["producer", "consumer"] },
    { "sid": "s2", "kind": "return" }
  ]
}
```

`kind` is the body / execution model:

| `kind`     | Meaning                                 |
| ---------- | --------------------------------------- |
| `"normal"` | Ordinary sequential function            |
| `"async"`  | Async function (`async_call` / `await`) |

`form` is an optional codegen hint: `"function"` (default) or `"closure"`.
`spawn` may target either; ConcIR does not require thread bodies to be closures.

An empty `body` is a nobody function: a codegen placeholder, not a call-chain
element. Optionally attach `effects: { "reads": [...], "writes": [...] }`.

The `body` is a list of [statements](statement.md) (CFG nodes; fallthrough
plus explicit `goto` / `branch` / `switch` / `return` / `select`).
A [`scope`](statement.md#threads-and-calls) statement lists the functions to
spawn together (`funcs`); they join before the next statement. Repeating
the same function N times is a `branch` loop, not a count field.

## Typed data flow (params / returns / locals)

> **3.5 proposal.** The rules below are what the validator enforces
> today. Closing names, destinations, expressions, and spawn-vs-call is
> specified in [Data flow](dataflow.md) and is not yet implemented.

Each declaration has a `modeled` flag (projection): only `modeled: true`
values enter the CVN store. Unmodeled values are codegen-only.

```json
{
  "name": "process",
  "kind": "normal",
  "params": [
    { "name": "budget", "type": "Int", "modeled": true },
    { "name": "label", "type": "String", "modeled": false }
  ],
  "returns": { "name": "ok", "type": "Bool", "modeled": true },
  "locals": [
    { "name": "tmp", "type": "Int", "modeled": true, "init": 0 }
  ],
  "body": [ ... ]
}
```

- Modeled params become `p_{fn}_{param}`, bound at `call` sites.
- A modeled return becomes `r_{fn}_{ret}`, written by
  `{ "kind": "return", "value": "<expr>" }` and captured into the caller's
  `dst`.
- Referencing a `modeled: false` parameter is E912.

At a `call` site: `args` must match modeled parameters (E920); `dst`, if
present, must be a writable slot — local, param, Var, or Atomic (E921).
The callee must declare a modeled return (E923). See [Data flow](dataflow.md).

```json
{
  "sid": "s1",
  "kind": "call",
  "func": "process",
  "args": ["budget", "10"],
  "dst": "ok_flag"
}
```
