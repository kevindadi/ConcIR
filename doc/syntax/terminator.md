# Terminator

CFG exits. This is the only place `return` may appear, and the only place
successors are named. See [Basic block](block.md).

| `kind`   | Fields                         | Description                                                                                   |
| -------- | ------------------------------ | --------------------------------------------------------------------------------------------- |
| `goto`   | `target`                       | Unconditional jump                                                                            |
| `branch` | `cond`, `then`, `else`         | Conditional; `else` is the JSON key. A back-edge (`then`/`else` to an earlier sid) is a loop. |
| `switch` | `var`, `cases`, `default`      | Multi-way branch; `default` is required                                                       |
| `return` | optional `value`               | Function return; one spelling only                                                            |
| `select` | `branches`, optional `default` | Multi-way wait; each branch has `guard` + `target`                                            |

`select` guards reuse the **same tagged JSON object** as the corresponding
[Statement](statement.md) (`kind` plus the same fields). Legal kinds:
`channel_recv`, `semaphore_acquire`, and `condvar_wait`.

## `select` `channel_recv` `dst`: popped payload

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

See [Resource](resource.md) for Channel `capacity`.

## `condvar_wait` as a select guard (E409)

In sync Rust, `Condvar::wait` is a blocking primitive and cannot be placed
in a non-blocking `select!`. ConcIR therefore rejects `condvar_wait` guards
unless:

- the enclosing function has `kind: "async"`, and
- the Condvar resource has `"mode": "Async"`.

The translator, at codegen, must map that async guard to `tokio::sync::Notify`,
a `watch` channel, or a timeout race — not to `std::sync::Condvar::wait`.
A sync wait loop uses `condvar_wait` as a **statement** plus a `branch`
back-edge, not `select`. See [Wait semantics](wait.md).
