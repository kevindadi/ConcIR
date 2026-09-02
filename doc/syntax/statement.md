# Statement

Every operation in a [function](function.md) `body`, including control
transfer. `body` is a list of statements; each is a CFG node
`{ "sid", "kind", … }`.

```json
{ "sid": "s1", "kind": "mutex_lock", "resource": "mtx" }
```

**Fallthrough.** A non-control op continues at the next entry in `body`.
Do not write a `goto` whose target is the immediately following statement.

**Control ops** (`goto` / `branch` / `switch` / `return` / `select`) name
successors. A path that does not end in `return` is E602. A then-arm that
must skip an else-arm uses `goto`:

```json
[
  { "sid": "s1", "kind": "branch", "cond": "flag == true", "then": "s2", "else": "s4" },
  { "sid": "s2", "kind": "write_shared", "resource": "x", "expr": "1" },
  { "sid": "s3", "kind": "goto", "target": "s5" },
  { "sid": "s4", "kind": "write_shared", "resource": "x", "expr": "0" },
  { "sid": "s5", "kind": "return" }
]
```

`sid` is `"s"` plus digits (`s1`, `s10`). The first statement is the entry.

In the CVN, a `read_shared` of a lock-protected Var (lock already held)
and an `atomic_load` are instantaneous data-flow steps — they do not
queue like `mutex_lock`.

`expr` / `cond` / `value` fields are strings that parse as the
[expression grammar](dataflow.md). Destinations follow the unified
`dst` rules. A `branch` or `switch` on a protected Var without the
lock is E309.

## Data

| `kind`          | Fields                                   | Description               |
| --------------- | ---------------------------------------- | ------------------------- |
| `nop`           | —                                        | No-op                     |
| `assign_local`  | `target`, `expr`                         | Write a function-local    |
| `read_shared`   | `resource`, optional `dst`               | Read a `Var`              |
| `write_shared`  | `resource`, `expr`                       | Write a `Var`             |
| `abstract_step` | `reads`, `writes`, `desc`                | Opaque **modeled** step (enters the CVN) |
| `seq_hole`      | `id`, `desc`, `reads`, `writes`          | Sequential fill site (not in the net) |
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
the old value happens to be Bool). Test success with a `branch`:

```json
{ "sid": "s1", "kind": "atomic_cas", "resource": "flag", "expected": "0", "desired": "1", "dst": "ret" },
{ "sid": "s2", "kind": "branch", "cond": "ret == 0", "then": "s3", "else": "s1" },
{ "sid": "s3", "kind": "return" }
```

On the back-edge, `ret` is the new current value; the next CAS should use it
as `expected` (via `assign_local` or by passing `ret` in `expected`).

### `seq_hole` vs `abstract_step` vs nobody

| Construct | In the net? | Role |
| --------- | :---------: | ---- |
| `abstract_step` | yes | Opaque concurrent step with a resource footprint |
| `seq_hole` | no | Hole where an LLM (or later pass) fills **sequential** code. `id` is unique per function (E109). `reads` / `writes` may name only Var / Atomic (E310); protected Vars still need the lock (E309). |
| empty `body` (nobody) | no | Whole-function placeholder; interface is `may_block` / `locks` / `effects` |

```json
{ "sid": "s3", "kind": "seq_hole", "id": "validate_payload", "desc": "checksum then store", "reads": ["buf"], "writes": [] }
```

Do not put lock / wait / send inside a hole — those belong in the skeleton.

## Synchronization

May block in the CVN.

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
See [Resource](resource.md) and [`select` guards](#select).

## Threads and calls

Unstructured `spawn` / `join` pair on **handles**. A `scope` statement
lists the functions to run together in one `thread::scope` and
**implicitly** `join_all` before the next statement. `funcs` uses the
[FQN rules](naming.md). Repeating one function N times is a `branch`
loop of `spawn`, not a count on `scope`.

| `kind`  | Key fields                     | Notes |
| ------- | ------------------------------ | ----- |
| `call`  | `func`, `args`, optional `dst` | Sequential; `args` match modeled params (E920); `dst` is a writable slot (E921) |
| `spawn` | `func`, `args`, `handle`       | Unstructured fork; unpaired handle is E401; target must have no modeled params (E922) |
| `scope` | `funcs`                        | Spawn each listed function and join them all (E410 if `funcs` is empty) |
| `join`  | `handle`                       | Join one unstructured spawn |
| `async_call` | `func`, `args`, `handle`  | Same E922 as spawn |
| `await` | `handle`                       | |

```json
{ "sid": "s1", "kind": "scope", "funcs": ["producer", "consumer"] }
```

Codegen is `thread::scope` + one `spawn` per name + implicit `handlers.join_all()`.

## Control

These kinds transfer control. They are statements like any other; there is
no separate terminator.

| `kind`   | Fields                         | Description                                                                                   |
| -------- | ------------------------------ | --------------------------------------------------------------------------------------------- |
| `goto`   | `target`                       | Unconditional jump. Omit when the target is the next statement (fallthrough).                 |
| `branch` | `cond`, `then`, `else`         | Conditional; `else` is the JSON key. A back-edge (`then`/`else` to an earlier sid) is a loop. |
| `switch` | `var`, `cases`, `default`      | Multi-way branch; `default` is required                                                       |
| `return` | optional `value`               | Function return                                                                               |
| `select` | `branches`, optional `default` | Multi-way wait; each branch has `guard` + `target`                                            |

`select` guards reuse the **same tagged JSON object** as the corresponding
statement (`kind` plus the same fields). Legal kinds: `channel_recv`,
`semaphore_acquire`, and `condvar_wait`.

### `select` `channel_recv` `dst`: popped payload

The Channel resource is the message store (`capacity` slots of `base`).
When a `channel_recv` guard fires, one slot is dequeued into `dst` — a
function local or Var/Atomic of that `base` (E206). `"_"` discards the
payload. This is the same field as statement `channel_recv`.

```json
{
  "sid": "s1",
  "kind": "select",
  "branches": [
    {
      "guard": { "kind": "channel_recv", "channel": "tx", "dst": "msg" },
      "target": "s2"
    }
  ]
}
```

See [Resource](resource.md) for Channel `capacity`.

### `condvar_wait` as a select guard (E409)

In sync Rust, `Condvar::wait` is a blocking primitive and cannot be placed
in a non-blocking `select!`. ConcIR therefore rejects `condvar_wait` guards
unless:

- the enclosing function has `kind: "async"`, and
- the Condvar resource has `"mode": "Async"`.

The translator, at codegen, must map that async guard to `tokio::sync::Notify`,
a `watch` channel, or a timeout race — not to `std::sync::Condvar::wait`.
A sync wait loop uses `condvar_wait` as a **statement** plus a `branch`
back-edge, not `select`.
