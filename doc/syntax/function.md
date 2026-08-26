# Function

```json
{
  "name": "main",
  "kind": "normal",
  "params": [],
  "locals": [],
  "body": [
    { "sid": "s1", "kind": "scope", "func": "worker", "count": 4 },
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
Homogeneous scoped threads are a [`scope`](statement.md#threads-and-calls)
statement (`func` + `count`, implicit `join_all`), not a function `kind`.

## Typed data flow (params / returns / locals)

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
present, must be a writable Var/Atomic (E921).

```json
{
  "sid": "s1",
  "kind": "call",
  "func": "process",
  "args": ["budget", "10"],
  "dst": "ok_flag"
}
```
