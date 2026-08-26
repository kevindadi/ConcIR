# Basic block

A [function](function.md) body is a **flattened CFG** of basic blocks. Each
block has:

1. Zero or more [`Stmt`](statement.md)s (data, sync, thread, function call).
2. Exactly one [`Terminator`](terminator.md). A missing terminator, or a leftover
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
