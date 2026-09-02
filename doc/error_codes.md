# ConcIR Error Code Reference

The validator emits structured diagnostics. Locations use JSON paths, e.g.
`modules[0].functions[1].body[3].call`.

See [`syntax/`](syntax/README.md) for the grammar and [`todo.md`](todo.md) for the roadmap.

## E0xx — Structural errors

Supplemental structural checks after successful JSON deserialization.
Unknown `kind` tags and leftover fields from the old block shape
(`statements`, `terminator`, `call`) fail at parse time (E000), not here.

| Code | Name                  | Severity | Description                                                                                |
| ---- | --------------------- | :------: | ------------------------------------------------------------------------------------------ |
| E000 | JsonParseError        |  error   | JSON syntax error or invalid top-level structure; deserialization failed                   |
| E001 | MissingField          |  error   | Resource declaration missing a field required by its type (Semaphore `count`; Channel `base` and `capacity`; Var/Atomic `base`/`init`). Channel `capacity` must be ≥ 0. |
| E005 | InvalidSidFormat      |  error   | sid format is not `"s"` + digits (e.g. `"s1"`, `"s10"`)                                    |
| E006 | InvalidSeqHoleId      |  error   | `seq_hole.id` is not an identifier `[A-Za-z_][A-Za-z0-9_]*`                                |
| E008 | InvalidKind           |  error   | Resource `kind` is not `"sync"` / `"var"`, or sync `type` value is illegal                 |
| E009 | InvalidMode           |  error   | `mode` is not `"Sync"` / `"Async"`                                                         |
| E010 | InvalidFnKind         |  error   | Function `kind` is not `"normal"` / `"async"`, or `form` is not `"function"` / `"closure"` |
| E208 | InitValueTypeMismatch |  error   | Resource initial value type does not match the declared `base`                             |

## E1xx — Name resolution

| Code | Name              | Severity | Description                                                                                                                                                                              |
| ---- | ----------------- | :------: | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| E101 | UndefinedResource |  error   | Resource name is not declared in this module and not imported                                                                                                                            |
| E102 | UndefinedFunction |  error   | Function name referenced by spawn/call/async_call has no definition                                                                                                                      |
| E103 | UndefinedSid      |  error   | Successor sid is not in the current function body                                                                                                                                        |
| E104 | DuplicateResource |  error   | Duplicate resource name in the same module                                                                                                                                               |
| E105 | DuplicateFunction |  error   | Duplicate function name in the same module                                                                                                                                               |
| E106 | DuplicateSid      |  error   | Duplicate sid within the same function body                                                                                                                                              |
| E107 | UndefinedEntry    |  error   | `entry` is not an FQN, or the FQN is not a defined function                                                                                                                              |
| E108 | ModuleContract    |  error   | Duplicate module name; `provides` names a missing local entity; `requires` is not an FQN or does not resolve to an exported entity; cross-module call not listed in `requires.functions` |
| E109 | DuplicateSeqHoleId |  error  | two `seq_hole` statements in the same function share an `id` |

## E2xx — Type errors

| Code | Name                    | Severity | Description                                                                           |
| ---- | ----------------------- | :------: | ------------------------------------------------------------------------------------- |
| E201 | BranchCondNotBool       |  error   | branch condition is not a comparison expression (missing `==`/`!=`/`>`/`<`/`>=`/`<=`) |
| E202 | SwitchVarNotEnumOrInt   |  error   | switch variable type is not Enum or Int                                               |
| E203 | WriteTypeMismatch       |  error   | `write_shared` value type does not match the Var's base                               |
| E204 | StoreTypeMismatch       |  error   | `atomic_store` value type does not match the Atomic's base                            |
| E205 | CasTypeMismatch         |  error   | `atomic_cas` `expected`/`desired`/`dst` do not match the Atomic's base. `dst` is the pre-CAS old value (that base type), not a Bool success flag. |
| E206 | SendTypeMismatch        |  error   | `channel_send` value or `channel_recv` `dst` type does not match the Channel's `base`. `dst` is the popped payload (that base type); `"_"` discards. |
| E207 | SwitchCaseLabelMismatch |  error   | switch case label is not a valid variant of the target Enum                           |

E203/E204/E205 also fire when a literal value is outside a bounded `Int`
domain (e.g. writing `11` to an `Int{[0,10]}` variable).

## E3xx — Resource–operation compatibility

| Code | Name                  | Severity | Description                                                          |
| ---- | --------------------- | :------: | -------------------------------------------------------------------- |
| E301 | LockOnNonLock         |  error   | `mutex_lock` / `mutex_unlock` on a non-Mutex                         |
| E302 | ReadLockOnNonRwLock   |  error   | `rwlock_*` on a non-RwLock                                           |
| E303 | WaitOnNonCondvar      |  error   | `condvar_*` on a non-Condvar                                         |
| E304 | WaitLockNotExist      |  error   | `condvar_wait`'s `lock` is not a Mutex/RwLock                        |
| E305 | AcquireOnNonSemaphore |  error   | `semaphore_*` on a non-Semaphore                                     |
| E306 | SendOnNonChannel      |  error   | `channel_send` / `channel_recv` on a non-Channel                     |
| E307 | LoadOnNonAtomic       |  error   | `atomic_*` on a non-Atomic                                           |
| E308 | ReadWriteOnNonVar     |  error   | `read_shared` / `write_shared` on a non-Var                          |
| E309 | VarAccessWithoutLock  |  error   | read/write of a protected Var without holding the corresponding lock, including `read_shared`/`write_shared`, `seq_hole` reads/writes, and r-values in `branch`/`switch`/`expr`/`args` |
| E310 | SeqHoleSyncResource   |  error   | `seq_hole` `reads` / `writes` names a Mutex, RwLock, Condvar, Semaphore, or Channel |

**Call / statement–resource compatibility**:

| operation           | Mutex | RwLock | Condvar | Semaphore | Channel | Atomic | Var  |
| ------------------- | :---: | :----: | :-----: | :-------: | :-----: | :----: | :--: |
| `mutex_lock/unlock` |  ok   |  E301  |  E301   |   E301    |  E301   |  E301  | E301 |
| `rwlock_*`          | E302  |   ok   |  E302   |   E302    |  E302   |  E302  | E302 |
| `condvar_*`         | E303  |  E303  |   ok    |   E303    |  E303   |  E303  | E303 |
| `semaphore_*`       | E305  |  E305  |  E305   |    ok     |  E305   |  E305  | E305 |
| `channel_send/recv` | E306  |  E306  |  E306   |   E306    |   ok    |  E306  | E306 |
| `atomic_*`          | E307  |  E307  |  E307   |   E307    |  E307   |   ok   | E307 |
| `read/write_shared` | E308  |  E308  |  E308   |   E308    |  E308   |  E308  |  ok  |

## E4xx — Concurrency pairing

Pairing is by **handle**, not by function name.

| Code | Name                     | Severity | Description                                                                                                                                                                                  |
| ---- | ------------------------ | :------: | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| E401 | SpawnWithoutJoin         | warning  | unstructured `spawn` handle has no matching `join` (`scope` statements join implicitly and do not emit this) |
| E402 | JoinWithoutSpawn         |  error   | join handle has no matching spawn                                                                                                                                                            |
| E403 | SpawnAsyncWithoutAwait   | warning  | async_call handle has no matching await                                                                                                                                                      |
| E404 | AwaitWithoutSpawnAsync   |  error   | await handle has no matching async_call                                                                                                                                                      |
| E405 | SyncSpawnPairedWithAwait |  error   | spawn handle reused as await (should be join)                                                                                                                                                |
| E406 | AsyncSpawnPairedWithJoin |  error   | async_call handle reused as join (should be await)                                                                                                                                           |
| E407 | JoinInAsyncContext       | warning  | join in an async function may block the runtime                                                                                                                                              |
| E408 | AwaitInSyncContext       |  error   | await used in a non-async function                                                                                                                                                           |
| E409 | CondvarWaitNotSelectable |  error   | `condvar_wait` as a `select` guard in a non-async function, or on a Sync-mode Condvar. Sync `Condvar::wait` cannot enter `select!`; async guards are codegen'd as Notify/watch/timeout race. |
| E410 | ScopeEmptyFuncs          |  error   | `scope` `funcs` is empty                                                                                                                                                                     |

## E5xx — Lock safety

| Code | Name                | Severity | Description                                                          |
| ---- | ------------------- | :------: | -------------------------------------------------------------------- |
| E501 | LockWithoutDrop     |  error   | lock without a corresponding unlock on some control-flow path        |
| E502 | DropWithoutLock     |  error   | unlock without a preceding matching lock                             |
| E503 | DoubleLock          |  error   | same resource locked twice on one path without an intervening unlock |
| E504 | SyncLockAcrossAwait |  error   | Sync lock held across an await point in an async function            |
| E505 | LockOrderViolation  |  error   | inconsistent lock acquisition order across paths (ABBA deadlock)     |

## E6xx — Control flow

| Code | Name                 | Severity | Description                                     |
| ---- | -------------------- | :------: | ----------------------------------------------- |
| E601 | UnreachableStatement | warning  | statement unreachable from the entry            |
| E602 | MissingReturn        |  error   | a control-flow path that does not end in return |
| E603 | BranchTargetsSame    | warning  | branch then/else targets are the same           |
| E604 | SwitchNotExhaustive  |  error   | switch does not cover all Enum variants         |
| E605 | InfiniteLoopNoExit   | warning  | loop with no exit and no blocking operation     |

## E7xx — Protection mapping

| Code | Name                   | Severity | Description                                           |
| ---- | ---------------------- | :------: | ----------------------------------------------------- |
| E701 | ProtectionTargetNotVar |  error   | protection left-hand side is not a Var-typed resource |
| E702 | ProtectionLockNotLock  |  error   | protection right-hand side is not a Mutex or RwLock   |
| E703 | AtomicInProtection     |  error   | Atomic resource appears in protection                 |
| E704 | VarWithoutProtection   | warning  | Var resource does not appear in protection            |
| E705 | DuplicateProtection    |  error   | same Var appears more than once in protection         |

## E8xx — Function concurrency interface

Declared `may_block` / `locks` on a function, and imported signatures on
`requires.functions`. See [`syntax/function.md`](syntax/function.md) and
[`syntax/module.md`](syntax/module.md).

| Code | Name | Severity | Description |
| ---- | ---- | :------: | ----------- |
| E801 | LockEffectNotLock | error | `locks.acquires` / `releases` / `requires_held` names a missing resource, a non-Mutex/RwLock, or a duplicate in the same list |
| E802 | MayBlockMismatch | error / warning | `may_block: false` on a body with a blocking op (error); `may_block: true` on a non-blocking body (warning). Nobody functions are not checked |
| E803 | RequiresHeldNotHeld | error | `call` of a function that declares `requires_held` without those locks held at the call site |
| E804 | ImportSigMismatch | error | a `requires.functions` signature object does not match the defining function (`kind`, `may_block`, `locks`, listed `params` / `returns`), or its `name` is not an FQN |

## E9xx — Typed data flow

Implemented against [`syntax/dataflow.md`](syntax/dataflow.md)
(name environment, unified dst, call vs concurrent entry, expression
parser, E309 on r-values). Default program version is `3.5.0`.

| Code | Name | Severity | Description |
| ---- | ---- | :------: | ----------- |
| E910 | ParamNameCollides | error | parameter name collides with a declared resource name |
| E911 | DuplicateParam | error | duplicate parameter name within a function |
| E912 | UnmodeledNameInNetExpr | warning | expression references a `modeled: false` param/local; the net treats it as Unknown |
| E913 | BareReturnWithModeledReturn | warning | function models a return but some `return` statement carries no value (binds Unknown) |
| E914 | LocalNameCollides | error | local name collides with a parameter or resource |
| E915 | DuplicateLocal | error | duplicate local name within a function |
| E920 | CallArityMismatch | error | `call` argument count does not match the callee's modeled parameters |
| E921 | DstNotWritableSlot | error | dst is not a writable slot (local, param, Var, Atomic; `"_"` where allowed) |
| E922 | ModeledParamOnConcurrentEntry | error | `spawn` / `async_call` / `scope` target has modeled parameters |
| E923 | DstWithoutModeledReturn | error | `call` has `dst` but the callee has no modeled return |
| E924 | SpawnArityMismatch | error | non-empty `spawn`/`async_call` `args` do not match unmodeled parameters |
| E931 | ExprParseError | error | expression string does not parse, or names an unknown identifier |
| E932 | ExprTypeMismatch | error | expression type does not match the destination / operands |
| E933 | BadProjection | error | missing, extra, or invalid struct field |
| E934 | NonValueResourceInExpr | error | Mutex/RwLock/Condvar/Semaphore/Channel used as an r-value |
| E935 | SwitchScrutineeNotSlot | error | `switch.var` is not a value slot (local, param, return, Var, Atomic) |
| E936 | AssignLocalToResource | error | `assign_local.target` is not a function local or parameter |
| E937 | ModeledActivationOnConcurrentEntry | warning | spawn/scope/async target has modeled locals or a modeled return |

## Diagnostic output format

Each diagnostic includes the following fields:

```json
{
  "code": "E501",
  "severity": "error",
  "message": "lock 'mtx' not unlocked on return path in function 'worker'",
  "path": "modules[0].functions[1].body[3]",
  "fix_hint": "add mutex_unlock/rwlock_unlock before return"
}
```

| Field      | Description                                                    |
| ---------- | -------------------------------------------------------------- |
| `code`     | Error code (e.g. `E501`)                                       |
| `severity` | `"error"` or `"warning"`; only error affects the `valid` field |
| `message`  | Human-readable error description                               |
| `path`     | JSON path location (optional)                                  |
| `fix_hint` | Suggested fix (optional)                                       |
