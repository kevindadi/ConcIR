# `wait` semantics

`condvar_wait(cv, lock)`: release `lock`, block until woken, re-acquire
`lock`. Lock-safety analysis treats the net effect as lock-neutral.

A condvar wait loop (statement-level CFG; the cycle is a `goto` / `branch`
back-edge):

```
s1: mutex_lock(mtx)                  // fallthrough
s2: read_shared(cond)
s3: branch(cond, then=s5, else=s4)
s4: condvar_wait(cv, mtx); goto s2   // back to the check, not to lock
s5: ...                              // condition holds; lock still held
```

`condvar_wait` as a [`select` guard](statement.md#condvar_wait-as-a-select-guard-e409) is E409 unless the
function is `async` and the Condvar is `mode: Async`.
