# Statement

Every non-control operation. None of these transfer control; the
[terminator](terminator.md) does. In the CVN, a `read_shared` of a
lock-protected Var (lock already held) and an `atomic_load` are instantaneous
data-flow steps — they do not queue like `mutex_lock`.

## Data

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

## Synchronization

May block in the CVN, but are still statements.

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
See [Resource](resource.md) and [`select` guards](terminator.md).

`condvar_wait` lock release / re-acquire is described under
[Wait semantics](wait.md).

## Threads and calls

Unstructured `spawn` / `join` pair on **handles**, not function names. A
`kind: "scope"` function joins leftover handles at `return` (see
[Function](function.md)). `func` uses the [FQN rules](naming.md).

| `kind`         | Key fields                     | Notes |
| -------------- | ------------------------------ | ----- |
| `call`         | `func`, `args`, optional `dst` | Sequential call; cannot target a scope (E411) |
| `spawn`        | `func`, `args`, `handle`       | Fork a thread; `form` is not restricted |
| `spawn_batch`  | `func`, `args`, optional `dst` | Enter a `kind: "scope"` function and wait |
| `join`         | `handle`                       | Early join of one spawn |
| `join_all`     | —                              | Mid-scope barrier; E412 outside a scope |
| `async_call`   | `func`, `args`, `handle`       | |
| `await`        | `handle`                       | |
