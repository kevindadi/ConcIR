# Control flow

A [function](function.md) `body` is a **list of statements**. Each statement
is a CFG node: a `sid` plus a tagged [`Op`](statement.md). There is no
separate terminator object.

**Fallthrough.** A non-control statement continues at the next entry in
`body`. Do not write a `goto` whose target is the immediately following
statement.

**Control statements** name successors explicitly: `goto`, `branch`,
`switch`, `return`, `select`. A path that does not end in `return` is E602.

There is no structured `loop` statement: a loop is a `branch` or `goto`
whose target is an earlier sid (a back-edge). `mutex_lock` and
`atomic_load` are ordinary statements — they do not take a successor
`target`.

```json
[
  { "sid": "s1", "kind": "mutex_lock", "resource": "mtx" },
  { "sid": "s2", "kind": "write_shared", "resource": "count", "expr": "count + 1" },
  { "sid": "s3", "kind": "mutex_unlock", "resource": "mtx" },
  { "sid": "s4", "kind": "return" }
]
```

`s1`–`s3` fall through; `s4` exits. A then-arm that must skip an else-arm
uses `goto`:

```json
[
  { "sid": "s1", "kind": "branch", "cond": "flag == true", "then": "s2", "else": "s4" },
  { "sid": "s2", "kind": "write_shared", "resource": "x", "expr": "1" },
  { "sid": "s3", "kind": "goto", "target": "s5" },
  { "sid": "s4", "kind": "write_shared", "resource": "x", "expr": "0" },
  { "sid": "s5", "kind": "return" }
]
```

`sid` must be `"s"` followed by digits (`s1`, `s10`). The first statement
in `body` is the entry.
