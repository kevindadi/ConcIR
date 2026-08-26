# Function

```json
{
  "name": "main",
  "kind": "scope",
  "params": [],
  "locals": [],
  "body": [
    {
      "sid": "s1",
      "statements": [
        { "kind": "spawn", "func": "worker", "handle": "h_worker" }
      ],
      "terminator": { "kind": "goto", "target": "s2" }
    },
    { "sid": "s2", "terminator": { "kind": "return" } }
  ]
}
```

`kind` is the body / execution model:

| `kind`     | Meaning |
| ---------- | ------- |
| `"normal"` | Ordinary sequential function |
| `"async"`  | Async function (`async_call` / `await`) |
| `"scope"`  | Structured fork-join region (see below) |

`form` is an optional codegen hint: `"function"` (default) or `"closure"`.
`spawn` may target either; ConcIR does not require thread bodies to be closures.

An empty `body` is a nobody function: a codegen placeholder, not a call-chain
element. Optionally attach `effects: { "reads": [...], "writes": [...] }`.

The `body` is a flattened CFG of [basic blocks](block.md).

## `kind: "scope"` — fork-join

A scope is `thread::scope` / structured concurrency: every `spawn` in its
body is a **fork**; `return` is the **join barrier** for handles not yet
explicitly `join`ed. `join` is still allowed for an early join of one
handle. `join_all` (no handle) is a mid-scope barrier over remaining
forks; it is illegal outside a scope (E412).

Enter a named scope with `spawn_batch` (not `call` / `spawn` / `async_call`,
which are E411). The caller of `spawn_batch` waits until the scope's
fork-join completes. Homogeneous "N copies of one function" is **not**
`spawn_batch`: write a `branch` loop of `spawn` inside the scope.

```json
{
  "name": "section",
  "kind": "scope",
  "body": [
    {
      "sid": "s1",
      "statements": [
        { "kind": "spawn", "func": "producer", "handle": "hp" },
        { "kind": "spawn", "func": "consumer", "handle": "hc" }
      ],
      "terminator": { "kind": "goto", "target": "s2" }
    },
    { "sid": "s2", "terminator": { "kind": "return" } }
  ]
}
```

```json
{ "kind": "spawn_batch", "func": "section" }
```

`spawn_batch` target must have `kind: "scope"` (E410). Spawns inside a
scope do not need a matching `join` (no E401); unstructured `spawn`
outside a scope still warns if unpaired.

See [Statement](statement.md) for `spawn` / `spawn_batch` / `join` fields.

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
  "statements": [
    {
      "kind": "call",
      "func": "process",
      "args": ["budget", "10"],
      "dst": "ok_flag"
    }
  ],
  "terminator": { "kind": "goto", "target": "s2" }
}
```
