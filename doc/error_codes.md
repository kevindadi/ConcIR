# ConcIR Error Code Reference

The validator emits structured diagnostics. All errors are located by JSON path, e.g. `functions[1].body[3].op`.

See [`syntax.md`](syntax.md) for the grammar and [`todo.md`](todo.md) for the roadmap.

## E0xx — Structural errors

Supplemental structural checks after successful JSON deserialization.

| Code | Name                  | Severity | Description                                                                                |
| ---- | --------------------- | :------: | ------------------------------------------------------------------------------------------ |
| E000 | JsonParseError        |  error   | JSON syntax error or invalid top-level structure; deserialization failed                   |
| E001 | MissingField          |  error   | Resource declaration missing a field required by its type (e.g. Semaphore missing `count`) |
| E005 | InvalidSidFormat      |  error   | sid format is not `"s"` + digits (e.g. `"s1"`, `"s10"`)                                    |
| E008 | InvalidKind           |  error   | Resource `kind` is not `"sync"` / `"var"`, or sync `type` value is illegal                 |
| E009 | InvalidMode           |  error   | `mode` is not `"Sync"` / `"Async"`                                                         |
| E010 | InvalidFnKind         |  error   | Function `kind` is not `"normal"` / `"async"` / `"closure"`                                |
| E208 | InitValueTypeMismatch |  error   | Resource initial value type does not match the declared `base`                             |

## E1xx — Name resolution

| Code | Name              | Severity | Description                                                            |
| ---- | ----------------- | :------: | ---------------------------------------------------------------------- |
| E101 | UndefinedResource |  error   | Resource name referenced in an op is not in resources                  |
| E102 | UndefinedFunction |  error   | Function name referenced by spawn/call/join/await has no fn definition |
| E103 | UndefinedSid      |  error   | Transfer target sid is not in the current function body                |
| E104 | DuplicateResource |  error   | Duplicate resource name in resources                                   |
| E105 | DuplicateFunction |  error   | Duplicate function name in functions                                   |
| E106 | DuplicateSid      |  error   | Duplicate sid within the same function body                            |
| E107 | UndefinedEntry    |  error   | Entry function name does not exist in functions                        |

## E2xx — Type errors

| Code | Name                    | Severity | Description                                                                           |
| ---- | ----------------------- | :------: | ------------------------------------------------------------------------------------- |
| E201 | BranchCondNotBool       |  error   | branch condition is not a comparison expression (missing `==`/`!=`/`>`/`<`/`>=`/`<=`) |
| E202 | SwitchVarNotEnumOrInt   |  error   | switch variable type is not Enum or Int                                               |
| E203 | WriteTypeMismatch       |  error   | write value type does not match the Var's base                                        |
| E204 | StoreTypeMismatch       |  error   | store value type does not match the Atomic's base                                     |
| E205 | CasTypeMismatch         |  error   | cas argument types do not match the Atomic's base                                     |
| E206 | SendTypeMismatch        |  error   | send value type does not match the Channel's base                                     |
| E207 | SwitchCaseLabelMismatch |  error   | switch case label is not a valid variant of the target Enum                           |

## E3xx — Resource–operation compatibility

| Code | Name                  | Severity | Description                                                          |
| ---- | --------------------- | :------: | -------------------------------------------------------------------- |
| E301 | LockOnNonLock         |  error   | lock/drop on a non-Mutex/RwLock resource                             |
| E302 | ReadLockOnNonRwLock   |  error   | read on a Mutex (should use lock)                                    |
| E303 | WaitOnNonCondvar      |  error   | wait/notify/notify_all on a non-Condvar resource                     |
| E304 | WaitLockNotExist      |  error   | wait's lock_name is not a declared Mutex/RwLock                      |
| E305 | AcquireOnNonSemaphore |  error   | acquire/release on a non-Semaphore resource                          |
| E306 | SendOnNonChannel      |  error   | send/recv on a non-Channel resource                                  |
| E307 | LoadOnNonAtomic       |  error   | load/store/cas on a non-Atomic resource                              |
| E308 | ReadWriteOnNonVar     |  error   | read (value) / write on a non-Var resource                           |
| E309 | VarAccessWithoutLock  |  error   | read/write of a protected Var without holding the corresponding lock |
| E310 | UnknownResourceAction |  error   | `res_op` uses an action not in the ConcIR contract                   |
| E311 | ResourceActionArity   |  error   | `res_op` action argument count does not match the ConcIR contract    |

**Operation–resource compatibility matrix**:

| action     | Mutex |  RwLock   | Condvar | Semaphore | Channel | Atomic |    Var    |
| ---------- | :---: | :-------: | :-----: | :-------: | :-----: | :----: | :-------: |
| lock       |  ok   | ok(write) |  E303   |   E305    |  E306   |  E307  |   E308    |
| read       | E302  | ok(read)  |  E303   |   E305    |  E306   |  E307  | ok(value) |
| write      | E301  |   E301    |  E303   |   E305    |  E306   |  E307  | ok(value) |
| drop       |  ok   |    ok     |  E303   |   E305    |  E306   |  E307  |   E308    |
| wait       | E303  |   E303    |   ok    |   E305    |  E306   |  E307  |   E308    |
| notify     | E303  |   E303    |   ok    |   E305    |  E306   |  E307  |   E308    |
| notify_all | E303  |   E303    |   ok    |   E305    |  E306   |  E307  |   E308    |
| acquire    | E301  |   E301    |  E303   |    ok     |  E306   |  E307  |   E308    |
| release    | E301  |   E301    |  E303   |    ok     |  E306   |  E307  |   E308    |
| send       | E301  |   E301    |  E303   |   E305    |   ok    |  E307  |   E308    |
| recv       | E301  |   E301    |  E303   |   E305    |   ok    |  E307  |   E308    |
| load       | E301  |   E301    |  E303   |   E305    |  E306   |   ok   |   E308    |
| store      | E301  |   E301    |  E303   |   E305    |  E306   |   ok   |   E308    |
| cas        | E301  |   E301    |  E303   |   E305    |  E306   |   ok   |   E308    |

## E4xx — Concurrency pairing

| Code | Name                     | Severity | Description                                     |
| ---- | ------------------------ | :------: | ----------------------------------------------- |
| E401 | SpawnWithoutJoin         | warning  | spawn without a corresponding join              |
| E402 | JoinWithoutSpawn         |  error   | join without a corresponding spawn              |
| E403 | SpawnAsyncWithoutAwait   | warning  | spawn_async without a corresponding await       |
| E404 | AwaitWithoutSpawnAsync   |  error   | await without a corresponding spawn_async       |
| E405 | SyncSpawnPairedWithAwait |  error   | spawn paired with await (should be join)        |
| E406 | AsyncSpawnPairedWithJoin |  error   | spawn_async paired with join (should be await)  |
| E407 | JoinInAsyncContext       | warning  | join in an async function may block the runtime |
| E408 | AwaitInSyncContext       |  error   | await used in a normal function                 |

## E5xx — Lock safety

| Code | Name                | Severity | Description                                                        |
| ---- | ------------------- | :------: | ------------------------------------------------------------------ |
| E501 | LockWithoutDrop     |  error   | lock without a corresponding drop on some control-flow path        |
| E502 | DropWithoutLock     |  error   | drop without a preceding matching lock                             |
| E503 | DoubleLock          |  error   | same resource locked twice on one path without an intervening drop |
| E504 | SyncLockAcrossAwait |  error   | Sync lock held across an await point in an async function          |
| E505 | LockOrderViolation  |  error   | inconsistent lock acquisition order across paths (ABBA deadlock)   |

## E6xx — Control flow

| Code | Name                 | Severity | Description                                     |
| ---- | -------------------- | :------: | ----------------------------------------------- |
| E601 | UnreachableStatement | warning  | statement unreachable from the entry            |
| E602 | MissingReturn        |  error   | a control-flow path that does not end in return |
| E603 | BranchTargetsSame    | warning  | branch true/false targets are the same          |
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

## E9xx — Typed data flow

| Code | Name                    | Severity | Description                                                                                        |
| ---- | ----------------------- | :------: | -------------------------------------------------------------------------------------------------- |
| E910 | ParamNameCollides       |  error   | parameter name collides with a declared resource name                                              |
| E911 | DuplicateParam          |  error   | duplicate parameter name within a function                                                          |
| E912 | UnmodeledParamReferenced|  error   | expression references a `modeled: false` parameter (it is not in the CVN variable store)           |
| E913 | BareReturnWithModeledReturn | warning | function models a return but some `return` statement carries no value (binds Unknown)             |
| E920 | CallArityMismatch        |  error   | `call` argument count does not match the callee's modeled parameters                                |
| E921 | CallCaptureNotVar       |  error   | `call` out-var is not a writable Var/Atomic resource                                               |

## Diagnostic output format

Each diagnostic includes the following fields:

```json
{
  "code": "E501",
  "severity": "error",
  "message": "lock 'mtx' not dropped on return path in function 'worker'",
  "path": "functions[1].body[3]",
  "fix_hint": "add drop() before return"
}
```

| Field      | Description                                                    |
| ---------- | -------------------------------------------------------------- |
| `code`     | Error code (e.g. `E501`)                                       |
| `severity` | `"error"` or `"warning"`; only error affects the `valid` field |
| `message`  | Human-readable error description                               |
| `path`     | JSON path location (optional)                                  |
| `fix_hint` | Suggested fix (optional)                                       |
