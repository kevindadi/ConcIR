# ConcIR Syntax

ConcIR (Concurrency Intermediate Representation) is a statement-level,
verification-oriented IR. The shape follows compiler intermediate code
(basic blocks, statements, calls, terminators) rather than a source language.
ConcIR is language-neutral: names are ConcIR identifiers and FQNs, never
backend crate paths or source-language keywords.

The executable definition is `src/ast.rs`, `src/fqn.rs`, and `src/validate/`.
See [`error_codes.md`](error_codes.md) for diagnostics and [`todo.md`](todo.md)
for the roadmap.

## Naming: identifiers and FQNs

| Form | Pattern | Example |
| ---- | ------- | ------- |
| Identifier | `[A-Za-z_][A-Za-z0-9_]*` | `storage`, `main`, `log_mtx` |
| Entity FQN | `module::entity` (exactly one `::`) | `storage::log_mtx`, `core::main` |
| Control location | `module::function.sid` | `core::main.s3` |

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

| Field     | Type   | Required | Description                          |
| --------- | ------ | :------: | ------------------------------------ |
| `program` | string |   yes    | Program name                         |
| `version` | string |    no    | Defaults to `"3.1.0"`                |
| `modules` | array  |   yes    | One or more [`Module`](#module)s     |
| `entry`   | FQN    |   yes    | Entry function, e.g. `core::main`    |

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

| Field        | Type    | Required | Description |
| ------------ | ------- | :------: | ----------- |
| `name`       | ident   |   yes    | Module identity |
| `provides`   | NameSet |    no    | Short names this module exports |
| `requires`   | NameSet |    no    | FQNs this module imports |
| `resources`  | array   |    no    | Resources owned by this module |
| `protection` | array   |    no    | Var → lock mapping |
| `functions`  | array   |    no    | Function definitions |

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
| Channel   | required |    —     | required | optional |

`capacity` is accepted on Channel for later bounded-buffer modeling. Current
CVN semantics still treat `channel_send` / `channel_recv` as unbuffered tokens.

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
      "call": {
        "kind": "spawn",
        "func": "worker",
        "handle": "h_worker",
        "target": "s2"
      }
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
  "call": {
    "kind": "call",
    "func": "process",
    "args": ["budget", "10"],
    "dst": "ok_flag",
    "target": "s2"
  }
}
```

## Basic block

A function body is a list of basic blocks. Each block has:

1. Zero or more [`Stmt`](#statement)s (data / structured loop).
2. Exactly one exit: either a [`Call`](#call) **or** a
   [`Terminator`](#terminator). Both or neither is a parse error.

This is the MIR / LLVM layout: statements, then a call with a continuation,
or a CFG terminator. `return` appears only as a terminator.

```json
{
  "sid": "s3",
  "statements": [
    { "kind": "write_shared", "resource": "count", "expr": "count + 1" }
  ],
  "terminator": { "kind": "goto", "target": "s4" }
}
```

`sid` must be `"s"` followed by digits (`s1`, `s10`).

## Statement

Data operations and structured loop headers. They do not transfer control by
themselves except `loop`, whose `body` / `exit` are additional CFG edges.

| `kind`           | Fields                         | Description |
| ---------------- | ------------------------------ | ----------- |
| `nop`            | —                              | No-op       |
| `assign_local`   | `target`, `expr`               | Write a function-local |
| `read_shared`    | `resource`, optional `dst`     | Read a `Var` |
| `write_shared`   | `resource`, `expr`             | Write a `Var` |
| `abstract_step`  | `reads`, `writes`, `desc`      | Opaque modeled step |
| `loop`           | `body`, `exit`                 | Structured loop header |

## Call

Thread control, synchronization, and function invocation. Every variant except
`select` names the successor block in `target` — the continuation after the
call returns (MIR `Call { destination, target }`).

Spawn / join pair on **handles**, not function names.

| `kind`                | Key fields                                      | Notes |
| --------------------- | ----------------------------------------------- | ----- |
| `mutex_lock`          | `resource`, `target`                            | Mutex |
| `mutex_unlock`        | `resource`, `target`                            | Mutex |
| `rwlock_read`         | `resource`, `target`                            | RwLock read lock |
| `rwlock_write`        | `resource`, `target`                            | RwLock write lock |
| `rwlock_unlock`       | `resource`, `target`                            | RwLock |
| `channel_send`        | `channel`, `value`, `target`                    | Channel |
| `channel_recv`        | `channel`, `dst`, `target`                      | `dst` is the data target; `target` is the successor |
| `condvar_wait`        | `condvar`, `lock`, `target`                     | See [wait semantics](#wait-semantics) |
| `condvar_notify`      | `condvar`, `target`                             | |
| `condvar_notify_all`  | `condvar`, `target`                             | |
| `semaphore_acquire`   | `resource`, optional `count`, `target`          | |
| `semaphore_release`   | `resource`, optional `count`, `target`          | |
| `atomic_load`         | `resource`, `dst`, `target`                     | |
| `atomic_store`        | `resource`, `value`, `target`                   | |
| `atomic_cas`          | `resource`, `expected`, `desired`, `dst`, `target` | |
| `call`                | `func`, `args`, optional `dst`, `target`        | Synchronous call |
| `spawn`               | `func`, `args`, `handle`, `target`              | OS thread |
| `spawn_batch`         | `func`, `count`, `handle`, `target`             | |
| `join`                | `handle`, `target`                              | Pairs with `spawn` on the same handle |
| `join_all`            | `handle`, `target`                              | |
| `async_call`          | `func`, `args`, `handle`, `target`              | Async task |
| `await`               | `handle`, `target`                              | Pairs with `async_call` |
| `select`              | `branches`, optional `default`                  | Each branch has `guard` + `target` |

`select` guards: `channel_recv`, `condvar_wait`, `semaphore_acquire`.

Call targets (`func`) use the [FQN rules](#naming-identifiers-and-fqns): short
name in the same module, FQN listed in `requires` otherwise.

## Terminator

CFG exits. This is the only place `return` may appear.

| `kind`   | Fields | Description |
| -------- | ------ | ----------- |
| `goto`   | `target` | Unconditional jump |
| `branch` | `cond`, `then`, `else` | Conditional; `else` is the JSON key |
| `switch` | `var`, `cases`, `default` | Multi-way branch; `default` is required |
| `return` | optional `value` | Function return; one spelling only |

There is no separate `op: "return"` and no `transfer` field.

## Validation pipeline

Nine passes; each emits diagnostics independently:

```
structure  →  names  →  types  →  compat  →  protection
    E0xx       E1xx      E2xx     E3xx        E7xx

→  concurrency  →  locks  →  control  →  dataflow
       E4xx        E5xx      E6xx         E9xx
```

JSON that does not match this grammar fails at deserialize (E000), including
unknown `kind` tags and a block with both `call` and `terminator`.

## `wait` semantics

`condvar_wait(cv, lock)`: release `lock`, block until woken, re-acquire
`lock`. Lock-safety analysis treats the net effect as lock-neutral.

A condvar wait loop:

```
s1: mutex_lock(mtx) → s2
s2: read_shared(cond); branch(cond, then=s4, else=s3)
s3: condvar_wait(cv, mtx) → s2     // back to the check, not to lock
s4: ...                            // condition holds; lock still held
```
