# ConcIR Syntax

ConcIR (Concurrency Intermediate Representation) is a statement-level, verification-oriented concurrency model. This document is the canonical grammar reference; the executable definition is `src/ast.rs` plus `src/validate/`.

See [`error_codes.md`](error_codes.md) for the validation error reference and [`todo.md`](todo.md) for the roadmap.

## Top-level structure

```json
{
  "program": "<program name>",
  "resources": [ ... ],
  "protection": [ ... ],
  "functions": [ ... ],
  "entry": "<entry function name>",
  "goals": [ ... ]
}
```

| Field        | Type   | Required | Description                                                                            |
| ------------ | ------ | :------: | -------------------------------------------------------------------------------------- |
| `program`    | string |   yes    | Program name                                                                           |
| `resources`  | array  |   yes    | Shared resource declarations                                                           |
| `protection` | array  |   yes    | Protection mapping (may be empty)                                                      |
| `functions`  | array  |   yes    | Function definitions; must include at least the entry function                         |
| `entry`      | string |   yes    | Entry function name                                                                    |
| `goals`      | array  |    no    | Reachability and variable postcondition goals; defaults to an empty array when omitted |

## Resource

**Synchronization primitives** (`kind: "sync"`):

```json
{"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}
{"name": "sem", "kind": "sync", "type": "Semaphore", "mode": "Async", "count": 3}
{"name": "tx",  "kind": "sync", "type": "Channel", "mode": "Async", "base": "Int"}
```

| type      |   mode   |  count   |   base   |
| --------- | :------: | :------: | :------: |
| Mutex     | required |    —     |    —     |
| RwLock    | required |    —     |    —     |
| Condvar   | required |    —     |    —     |
| Semaphore | required | required |    —     |
| Channel   | required |    —     | required |

Channel currently has no capacity field; the translator abstracts it as a
resource that starts empty, where `send` produces one message token and `recv`
consumes one message token. Capacity, message contents, and FIFO ordering are
not modeled in the current ConcIR/CVN semantics.

**Shared variables** (`kind: "var"`):

```json
{"name": "count", "kind": "var", "type": "Var",    "base": "Int", "init": 0}
{"name": "flag",  "kind": "var", "type": "Atomic", "base": "Bool", "init": false}
```

**`base_type` values**:

| Value                                | Description        | init example |
| ------------------------------------ | ------------------ | ------------ |
| `"Bool"`                             | Boolean            | `true`       |
| `"Int"`                              | Integer            | `0`          |
| `{"Int": [lo, hi]}`                  | Bounded Int `[lo, hi]` | `3`       |
| `"Float"`                            | Floating-point     | `3.14`       |
| `"String"`                           | String             | `""`         |
| `{"Enum": ["A","B"]}`                | Enum               | `"A"`        |
| `{"Struct": {"x":"Int"}}`            | Struct             | `{"x": 0}`   |
| `{"Array": {"elem":"Int","len":10}}` | Fixed-length array | `[]`         |

**Bounded Int**: `{"Int": [lo, hi]}` restricts a variable's value domain. In the
CVN a variable update leaving the domain disables its transition, so counter
loops terminate at the bound and the state space stays finite. `init` values and
literal write/store/cas values outside `[lo, hi]` are validation errors (E208 /
E203 / E204 / E205).

## Protection

```json
{ "var": "counter", "lock": "mtx" }
```

Each `Var` may appear at most once. `Atomic` resources do not appear in protection.

## Function

```json
{
  "name": "main",
  "kind": "normal",
  "body": [
    { "sid": "s1", "op": ["spawn", "worker"], "transfer": ["next", "s2"] },
    { "sid": "s2", "op": "return", "transfer": "return" }
  ]
}
```
`kind` values: `"normal"` / `"async"` / `"closure"`

The optional `module` field records the source fragment when the program was
assembled from modular ConcIR parts; it is used for cross-module repair
attribution and is absent for single-fragment programs.

### Typed data flow (params / returns)

Functions may declare typed parameters and an optional return value. Each
carries a `modeled` flag implementing the **projection principle**: only
`modeled: true` values enter the CVN variable store; unmodeled values are
codegen-only placeholders and are never materialized in the net.

```json
{
  "name": "process",
  "kind": "normal",
  "params": [
    { "name": "budget", "type": "Int", "modeled": true },
    { "name": "label", "type": "String", "modeled": false }
  ],
  "returns": { "name": "ok", "type": "Bool", "modeled": true },
  "body": [ ... ]
}
```

- Modeled params become variables named `p_{fn}_{param}`, bound at `call`
  sites and readable in the function's guards / expressions.
- A modeled return becomes a variable named `r_{fn}_{ret}`, written by
  `["return", <expr>]` and captured into a caller Var via the call's out-var.
- A parameter referenced by any expression must be `modeled: true`
  (otherwise validation error E912). Unmodeled params are never referenced by
  the body.

At a `call` site the extra elements after the callee name are interpreted from
the callee's signature: when the callee models a return, the first extra
element is the capture out-var (`""` = no capture) followed by the arguments;
otherwise all extras are arguments. The out-var must be a writable Var/Atomic
resource (E921); the argument count must match the modeled parameters (E920).

```json
{ "sid": "s1", "op": ["call", "process", "ok_flag", "budget", "10"], "transfer": ["next", "s2"] }
```
### Body-less ("nobody") functions

An empty `body` array marks a function with no control flow and no callsites. It is a codegen placeholder, not a call-chain element. When spawned, the translator models it as a trivial skeleton (entry → single transition → return); a `call` to one is an atomic pass-through. Optionally attach an `effects` object to hint the data footprint for codegen:

```json
{
  "name": "compute",
  "kind": "normal",
  "body": [],
  "effects": { "reads": ["counter"], "writes": ["result"] }
}
```

`effects` carries `reads`/`writes` (both default to `[]`). The write values are
modeled as unknown in the CVN.

## Operation (op)

| Format                                      | Description                                    |
| ------------------------------------------- | ---------------------------------------------- |
| `["res_op", "<resource>", "<action>", ...]` | Shared resource operation                      |
| `["spawn", "<function name>"]`              | Create an OS thread                            |
| `["spawn_async", "<function name>"]`        | Create an async task                           |
| `["join", "<function name>"]`               | Wait for a thread                              |
| `["await", "<function name>"]`              | Wait for an async task                         |
| `["call", "<function name>", ...]`  | Synchronous call; optional out-var + argument expressions (see typed data flow) |
| `["return", "<expr>"]`              | Function return with an optional value expression (binds a modeled `returns`) |
| `"return"`                          | Function return (string, without a value)                          |
| `"nop"`                             | No-op; useful as an explicit control-flow node |

`call` targets are resolved after merge, so any defined function may be called — body-less or bodied, including one with synchronization operations.

### `res_op` action list

| action       | Arguments         | Applicable types                     |
| ------------ | ----------------- | ------------------------------------ |
| `lock`       | none              | Mutex, RwLock                        |
| `read`       | none              | RwLock (read lock), Var (read value) |
| `write`      | val               | Var                                  |
| `drop`       | none              | Mutex, RwLock                        |
| `wait`       | lock_name         | Condvar                              |
| `notify`     | none              | Condvar                              |
| `notify_all` | none              | Condvar                              |
| `acquire`    | none              | Semaphore                            |
| `release`    | none              | Semaphore                            |
| `send`       | val               | Channel                              |
| `recv`       | none              | Channel                              |
| `load`       | none              | Atomic                               |
| `store`      | val               | Atomic                               |
| `cas`        | expected, desired | Atomic                               |

## Transfer

| Format                                                   | Description                         |
| -------------------------------------------------------- | ----------------------------------- |
| `["next", "<sid>"]`                                      | Sequential transfer                 |
| `["branch", "<condition>", "<true_sid>", "<false_sid>"]` | Conditional branch                  |
| `["switch", "<variable>", {"<label>": "<sid>", ...}]`    | Multi-way branch                    |
| `"return"`                                               | Function end (string, not an array) |

## BusinessGoal

```json
{
  "id": "workers_return",
  "desc": "Both workers reach return",
  "marking": { "worker.s5": 1, "mtx": 1 },
  "variables": { "ready": true }
}
```

`desc`, `marking`, and `variables` may be omitted. Keys in `marking` may be: a declared resource name; a control location of the form `function.sid`; or a raw CVN place id starting with `cp_`, `rp_`, `wp_`, or `ra_`. Do not use display forms such as `cp(worker, ret)` or `rp(mtx)`. Goal token counts mean the minimum number that must be reached; for Channel/Condvar resources that start empty, use 0 for an emptiness check. `variables` uses CVN variable names and JSON scalar values.

## Validation pipeline

The validator runs 8 passes in a fixed order; each pass emits diagnostics independently:

```
structure  →  names  →  types  →  compat  →  protection
    E0xx       E1xx      E2xx     E3xx        E7xx

→  concurrency  →  locks  →  control
       E4xx        E5xx      E6xx
```

## `wait` semantics

ConcIR semantics of `wait(cv, lock_name)`: release the associated lock, block until woken, then re-acquire the lock.

Therefore, in lock-safety analysis, the net effect of `wait` is that lock state is unchanged (release followed immediately by re-acquire). When modeling a condvar wait loop, write it as:

```
s1: lock(mtx)            -- acquire lock
s2: read(cond)           -- check condition
    branch(cond, s4, s3)
s3: wait(cv, mtx)        -- release lock, wait, re-acquire lock
    next(s2)             -- back to condition check, not back to lock
s4: ...                  -- condition satisfied; continue (lock still held)
```
