# `wait` semantics

`condvar_wait(cv, lock)`: release `lock`, block until woken, re-acquire
`lock`. Lock-safety analysis treats the net effect as lock-neutral.

A condvar wait loop (flattened CFG; the cycle is the `branch` back-edge):

```
s1: mutex_lock(mtx); goto s2
s2: read_shared(cond); branch(cond, then=s4, else=s3)
s3: condvar_wait(cv, mtx); goto s2   // back to the check, not to lock
s4: ...                              // condition holds; lock still held
```

`condvar_wait` as a [`select` guard](terminator.md) is E409 unless the
function is `async` and the Condvar is `mode: Async`.
