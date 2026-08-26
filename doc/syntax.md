# ConcIR Syntax

ConcIR (Concurrency Intermediate Representation) is a statement-level,
verification-oriented IR. The shape follows compiler intermediate code
(basic blocks, statements, calls, terminators) rather than a source language.
ConcIR is language-neutral: names are ConcIR identifiers and FQNs, never
backend crate paths or source-language keywords.

The executable definition is `src/ast.rs`, `src/fqn.rs`, and `src/validate/`.
The formal grammar is [`ebnf.md`](ebnf.md). See [`error_codes.md`](error_codes.md)
for diagnostics and [`todo.md`](todo.md) for the roadmap.

## Naming: identifiers and FQNs

| Form             | Pattern                             | Example                          |
| ---------------- | ----------------------------------- | -------------------------------- |
| Identifier       | `[A-Za-z_][A-Za-z0-9_]*`            | `storage`, `main`, `log_mtx`     |
| Entity FQN       | `module::entity` (exactly one `::`) | `storage::log_mtx`, `core::main` |
| Control location | `module::function.sid`              | `core::main.s3`                  |

Rules:

1. A **module name** is an identifier. It is ConcIR's own namespace, not a
   Rust crate or Java package.
2. An **entity FQN** names a resource or function as `module::entity`. Extra
   `::` segments are illegal (`crate::foo::bar` is not a ConcIR FQN).
3. A **control location** is `module::function.sid`. Use this when referring
   to a basic block from outside the function.
4. **Same-module references use the short name.** Inside module `core`, write
   `main` and `log_mtx`, not `core::main`.
5. **Cross-module references must be FQNs** and must appear in the importing
   module's `requires`.
6. **`provides` always uses short names** declared in this module.
7. **`requires` always uses FQNs.**
8. **`entry` is always an FQN.**

## Top-level program

A program is a set of modules plus one entry FQN. There is no `goals` field;
reachability queries belong to the verifier / CVN layer, not the IR.

```json
{
  "program": "app",
  "version": "3.1.0",
  "modules": [ ... ],
  "entry": "core::main"
}
```

| Field     | Type   | Required | Description                       |
| --------- | ------ | :------: | --------------------------------- |
| `program` | string |   yes    | Program name                      |
| `version` | string |    no    | Defaults to `"3.1.0"`             |
| `modules` | array  |   yes    | One or more [`Module`](#module)s  |
| `entry`   | FQN    |   yes    | Entry function, e.g. `core::main` |

## Module

A module is an independently authored fragment: resources, protection,
functions, and a name-resolution contract.

```json
{
  "name": "storage",
  "provides": { "resources": ["log_mtx"], "functions": ["flush"] },
  "requires": { "resources": [], "functions": ["core::log"] },
  "resources": [
    {"name": "log_mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}
  ],
  "protection": [],
  "functions": [ ... ]
}
```

| Field        | Type    | Required | Description                     |
| ------------ | ------- | :------: | ------------------------------- |
| `name`       | ident   |   yes    | Module identity                 |
| `provides`   | NameSet |    no    | Short names this module exports |
| `requires`   | NameSet |    no    | FQNs this module imports        |
| `resources`  | array   |    no    | Resources owned by this module  |
| `protection` | array   |    no    | Var → lock mapping              |
| `functions`  | array   |    no    | Function definitions            |

`NameSet` is `{ "resources": [...], "functions": [...] }` (both default `[]`).

The validator consumes the assembled `Program`. `provides` / `requires` are
enforced (E108): a provided name must be declared here; a required FQN must
exist and be exported by the owning module; a cross-module call target must
appear in `requires.functions`.

## Resource

**Synchronization primitives** (`kind: "sync"`):

```json
{"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}
{"name": "sem", "kind": "sync", "type": "Semaphore", "mode": "Async", "count": 3}
{"name": "tx",  "kind": "sync", "type": "Channel", "mode": "Async", "base": "Int", "capacity": 8}
```

| type      |   mode   |  count   |   base   | capacity |
| --------- | :------: | :------: | :------: | :------: |
| Mutex     | required |    —     |    —     |    —     |
| RwLock    | required |    —     |    —     |    —     |
| Condvar   | required |    —     |    —     |    —     |
| Semaphore | required | required |    —     |    —     |
| Channel   | required |    —     | required | required |

**Channel is the message store.** `base` is the payload type of each slot;
`capacity` is the number of in-flight slots (E001 if missing or negative):

- `capacity: 0` — rendezvous (no buffered payload)
- `capacity: n` (`n ≥ 1`) — bounded buffer of `n` messages of type `base`

`channel_send` enqueues into those slots; `channel_recv` (statement or
`select` guard) dequeues one slot into `dst`. The CVN currently still treats
send/recv as unbuffered tokens; bounded-buffer semantics from `capacity` are
on the roadmap.

**Shared variables** (`kind: "var"`):

```json
{"name": "count", "kind": "var", "type": "Var",    "base": "Int", "init": 0}
{"name": "flag",  "kind": "var", "type": "Atomic", "base": "Bool", "init": false}
```

**`base` values**:

| Value                                | Description            | init example |
| ------------------------------------ | ---------------------- | ------------ |
| `"Bool"`                             | Boolean                | `true`       |
| `"Int"`                              | Integer                | `0`          |
| `{"Int": [lo, hi]}`                  | Bounded Int `[lo, hi]` | `3`          |
| `"Float"`                            | Floating-point         | `3.14`       |
| `"String"`                           | String                 | `""`         |
| `{"Enum": ["A","B"]}`                | Enum                   | `"A"`        |
| `{"Struct": {"x":"Int"}}`            | Struct                 | `{"x": 0}`   |
| `{"Array": {"elem":"Int","len":10}}` | Fixed-length array     | `[]`         |

Bounded Int: a CVN update leaving `[lo, hi]` disables the transition, so
counter loops stay finite. Literals outside the domain are E208 / E203.

## Protection

```json
{ "var": "counter", "lock": "mtx" }
```

Each `Var` appears at most once. `Atomic` resources must not appear here.

## Function

```json
{
  "name": "main",
  "kind": "normal",
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

`kind` values: `"normal"` / `"async"` / `"closure"`.

An empty `body` is a nobody function: a codegen placeholder, not a call-chain
element. Optionally attach `effects: { "reads": [...], "writes": [...] }`.

### Typed data flow (params / returns / locals)

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

## Basic block

A function body is a **flattened CFG** of basic blocks. Each block has:

1. Zero or more [`Stmt`](#statement)s (data, sync, thread, function call).
2. Exactly one [`Terminator`](#terminator). A missing terminator, or a leftover
   `call` field, is a parse error.

Control flow lives **only** in the terminator. There is no structured `loop`
statement: a loop is a `branch` whose `then` or `else` is a back-edge to an
earlier block. `mutex_lock` and `atomic_load` are statements, not block exits
— they do not take a successor `target`. `return` appears only as a terminator.

A block may pack several instantaneous operations (held-lock `read_shared`,
`atomic_load`, `assign_local`) before a single terminator, so the CFG does not
grow a sid per load.

```json
{
  "sid": "s3",
  "statements": [
    { "kind": "mutex_lock", "resource": "mtx" },
    { "kind": "write_shared", "resource": "count", "expr": "count + 1" },
    { "kind": "mutex_unlock", "resource": "mtx" }
  ],
  "terminator": { "kind": "goto", "target": "s4" }
}
```

`sid` must be `"s"` followed by digits (`s1`, `s10`).

## Statement

Every non-control operation. None of these transfer control; the terminator
does. In the CVN, a `read_shared` of a lock-protected Var (lock already held)
and an `atomic_load` are instantaneous data-flow steps — they do not queue
like `mutex_lock`.

**Data**

| `kind`          | Fields                                   | Description               |
| --------------- | ---------------------------------------- | ------------------------- |
| `nop`           | —                                        | No-op                     |
| `assign_local`  | `target`, `expr`                         | Write a function-local    |
| `read_shared`   | `resource`, optional `dst`               | Read a `Var`              |
| `write_shared`  | `resource`, `expr`                       | Write a `Var`             |
| `abstract_step` | `reads`, `writes`, `desc`                | Opaque modeled step       |
| `atomic_load`   | `resource`, `dst`                        | Instantaneous Atomic read; `dst` := current value |
| `atomic_store`  | `resource`, `value`                      | Atomic write              |
| `atomic_cas`    | `resource`, `expected`, `desired`, `dst` | Compare-and-swap; `dst` := **old value** (see below) |

### `atomic_cas` `dst`: old value, not Bool

`dst` is written with the value of `resource` **before** the swap — the same
type as the Atomic's `base`. This matches Rust
`Atomic*::compare_exchange` / C++ `compare_exchange_strong` (the observed
current value), **not** a Bool success flag.

- Success: `dst == expected` (the snapshot still equals what we compared
  against). The resource now holds `desired`.
- Failure: the resource is unchanged and `dst` holds the latest observed
  value. A spin loop uses that value as the next `expected`.

Do not write `dst` as Bool unless the Atomic itself is `Bool` (in which case
the old value happens to be Bool). Test success with a terminator:

```json
{
  "sid": "s1",
  "statements": [
    {
      "kind": "atomic_cas",
      "resource": "flag",
      "expected": "0",
      "desired": "1",
      "dst": "ret"
    }
  ],
  "terminator": {
    "kind": "branch",
    "cond": "ret == 0",
    "then": "s2",
    "else": "s1"
  }
}
```

On the back-edge, `ret` is the new current value; the next CAS should use it
as `expected` (via `assign_local` or by passing `ret` in `expected`).

**Synchronization** (may block in the CVN, but are still statements)

| `kind`                                           | Key fields                   |
| ------------------------------------------------ | ---------------------------- |
| `mutex_lock` / `mutex_unlock`                    | `resource`                   |
| `rwlock_read` / `rwlock_write` / `rwlock_unlock` | `resource`                   |
| `channel_send`                                   | `channel`, `value`           |
| `channel_recv`                                   | `channel`, `dst`             |
| `condvar_wait`                                   | `condvar`, `lock`            |
| `condvar_notify` / `condvar_notify_all`          | `condvar`                    |
| `semaphore_acquire` / `semaphore_release`        | `resource`, optional `count` |

`channel_recv` `dst` is the popped payload (Channel `base`); `"_"` discards.
The in-flight messages live in the Channel resource's `capacity` slots.

**Threads and calls.** Spawn / join pair on **handles**, not function names.
`func` uses the [FQN rules](#naming-identifiers-and-fqns).

| `kind`              | Key fields                     |
| ------------------- | ------------------------------ |
| `call`              | `func`, `args`, optional `dst` |
| `spawn`             | `func`, `args`, `handle`       |
| `spawn_batch`       | `func`, `count`, `handle`      |
| `join` / `join_all` | `handle`                       |
| `async_call`        | `func`, `args`, `handle`       |
| `await`             | `handle`                       |

## Terminator

CFG exits. This is the only place `return` may appear, and the only place
successors are named.

| `kind`   | Fields                         | Description                                                                                   |
| -------- | ------------------------------ | --------------------------------------------------------------------------------------------- |
| `goto`   | `target`                       | Unconditional jump                                                                            |
| `branch` | `cond`, `then`, `else`         | Conditional; `else` is the JSON key. A back-edge (`then`/`else` to an earlier sid) is a loop. |
| `switch` | `var`, `cases`, `default`      | Multi-way branch; `default` is required                                                       |
| `return` | optional `value`               | Function return; one spelling only                                                            |
| `select` | `branches`, optional `default` | Multi-way wait; each branch has `guard` + `target`                                            |

`select` guards reuse the **same tagged JSON object** as the corresponding
Statement (`kind` plus the same fields). Legal kinds: `channel_recv`,
`semaphore_acquire`, and `condvar_wait`.

### `select` `channel_recv` `dst`: popped payload

The Channel resource is the message store (`capacity` slots of `base`).
When a `channel_recv` guard fires, one slot is dequeued into `dst` — a
function local or Var/Atomic of that `base` (E206). `"_"` discards the
payload. This is the same field as statement `channel_recv`.

```json
{
  "kind": "select",
  "branches": [
    {
      "guard": { "kind": "channel_recv", "channel": "tx", "dst": "msg" },
      "target": "s_handle_msg"
    }
  ]
}
```

**`condvar_wait` as a select guard (E409).** In sync Rust, `Condvar::wait` is a
blocking primitive and cannot be placed in a non-blocking `select!`. ConcIR
therefore rejects `condvar_wait` guards unless:

- the enclosing function has `kind: "async"`, and
- the Condvar resource has `"mode": "Async"`.

The translator, at codegen, must map that async guard to `tokio::sync::Notify`,
a `watch` channel, or a timeout race — not to `std::sync::Condvar::wait`.
A sync wait loop uses `condvar_wait` as a **statement** plus a `branch`
back-edge, not `select`.

## Validation pipeline

Nine passes; each emits diagnostics independently:

```
structure  →  names  →  types  →  compat  →  protection
    E0xx       E1xx      E2xx     E3xx        E7xx

→  concurrency  →  locks  →  control  →  dataflow
       E4xx        E5xx      E6xx         E9xx
```

JSON that does not match this grammar fails at deserialize (E000), including
unknown `kind` tags, a leftover `call` field on a block, or a missing terminator.

## `wait` semantics

`condvar_wait(cv, lock)`: release `lock`, block until woken, re-acquire
`lock`. Lock-safety analysis treats the net effect as lock-neutral.

A condvar wait loop (flattened CFG; the cycle is the `branch` back-edge):

```
s1: mutex_lock(mtx); goto s2
s2: read_shared(cond); branch(cond, then=s4, else=s3)
s3: condvar_wait(cv, mtx); goto s2   // back to the check, not to lock
s4: ...                              // condition holds; lock still held
```
