# CIR Validator

Static validator for CIR (Concurrency Intermediate Representation). Reads a CIR JSON file, runs 9 validation passes, and emits a structured diagnostic report.

## Quick Start

```bash
cargo build --release
./target/release/cir examples/producer_consumer.json
```

Output is a JSON `ValidationReport`:

```json
{
  "valid": true,
  "diagnostics": []
}
```

If there are errors, `valid` is `false`, `diagnostics` contains all diagnostic items, and the process exits with exit code 1.

---

## CIR JSON Format Specification

### Top-level structure

```json
{
  "program": "<program name>",
  "resources": [ ... ],
  "protection": [ ... ],
  "functions": [ ... ],
  "entry": "<entry function name>",
  "goals": [ ... ]
}
```

| Field          | Type   | Required | Description                                                                            |
| -------------- | ------ | :------: | -------------------------------------------------------------------------------------- |
| `program`      | string |   yes    | Program name                                                                           |
| `resources`    | array  |   yes    | Shared resource declarations                                                           |
| `protection`   | array  |   yes    | Protection mapping (may be empty)                                                      |
| `functions`    | array  |   yes    | Function definitions; must include at least the entry function                         |
| `entry`        | string |   yes    | Entry function name                                                                    |
| `goals`        | array  |    no    | Reachability and variable postcondition goals; defaults to an empty array when omitted |

### Resource

**Synchronization primitives** (`kind: "sync"`):

```json
{"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}
{"name": "sem", "kind": "sync", "type": "Semaphore", "mode": "Async", "count": 3}
{"name": "tx",  "kind": "sync", "type": "Channel", "mode": "Async", "base": "Int"}
```

| type      |   mode   |  count   |   base   |
| --------- | :------: | :------: | :------: |
| Mutex     | required |    —     |    —     |
| RwLock    | required |    —     |    —     |
| Condvar   | required |    —     |    —     |
| Semaphore | required | required |    —     |
| Channel   | required |    —     | required |

Channel currently has no capacity field; the translator abstracts it as a resource that starts empty, where `send` produces one message token and `recv` consumes one message token. Capacity, message contents, and FIFO ordering are not modeled in the current CIR/CVN semantics.

**Shared variables** (`kind: "var"`):

```json
{"name": "count", "kind": "var", "type": "Var",    "base": "Int", "init": 0}
{"name": "flag",  "kind": "var", "type": "Atomic", "base": "Bool", "init": false}
```

**`base_type` values**:

| Value                                | Description        | init example |
| ------------------------------------ | ------------------ | ------------ |
| `"Bool"`                             | Boolean            | `true`       |
| `"Int"`                              | Integer            | `0`          |
| `"Float"`                            | Floating-point     | `3.14`       |
| `"String"`                           | String             | `""`         |
| `{"Enum": ["A","B"]}`                | Enum               | `"A"`        |
| `{"Struct": {"x":"Int"}}`            | Struct             | `{"x": 0}`   |
| `{"Array": {"elem":"Int","len":10}}` | Fixed-length array | `[]`         |

### Protection

```json
{ "var": "counter", "lock": "mtx" }
```

Each `Var` may appear at most once. `Atomic` resources do not appear in protection.

### Function

```json
{
  "name": "main",
  "kind": "normal",
  "body": [
    { "sid": "s1", "op": ["spawn", "worker"], "transfer": ["next", "s2"] },
    { "sid": "s2", "op": "return", "transfer": "return" }
  ]
}
```

`kind` values: `"normal"` / `"async"` / `"closure"`

**Body-less ("nobody") functions**: an empty `body` array marks a function with no
control flow and no callsites. When it is referenced by `spawn`/`join`/`call`, the
translator models it as a trivial skeleton (entry → single transition → return).
Optionally attach an `effects` object to hint the data footprint for codegen:

```json
{
  "name": "compute",
  "kind": "normal",
  "body": [],
  "effects": { "reads": ["counter"], "writes": ["result"] }
}
```

`effects` carries `reads`/`writes` (both default to `[]`). The write values are
modeled as unknown in the CVN. Functions referenced only by `call` resolve after
the (optional modular) merge step, so calling a bodied function — including one
with synchronization operations — is legitimate.

### Operation (op)

| Format                                      | Description                                    |
| ------------------------------------------- | ---------------------------------------------- |
| `["res_op", "<resource>", "<action>", ...]` | Shared resource operation                      |
| `["spawn", "<function name>"]`              | Create an OS thread                            |
| `["spawn_async", "<function name>"]`        | Create an async task                           |
| `["join", "<function name>"]`               | Wait for a thread                              |
| `["await", "<function name>"]`              | Wait for an async task                         |
| `["call", "<function name>"]`               | Synchronous call                               |
| `"return"`                                  | Function return (string, not an array)         |
| `"nop"`                                     | No-op; useful as an explicit control-flow node |

**`res_op` action list**:

| action       | Arguments         | Applicable types                     |
| ------------ | ----------------- | ------------------------------------ |
| `lock`       | none              | Mutex, RwLock                        |
| `read`       | none              | RwLock (read lock), Var (read value) |
| `write`      | val               | Var                                  |
| `drop`       | none              | Mutex, RwLock                        |
| `wait`       | lock_name         | Condvar                              |
| `notify`     | none              | Condvar                              |
| `notify_all` | none              | Condvar                              |
| `acquire`    | none              | Semaphore                            |
| `release`    | none              | Semaphore                            |
| `send`       | val               | Channel                              |
| `recv`       | none              | Channel                              |
| `load`       | none              | Atomic                               |
| `store`      | val               | Atomic                               |
| `cas`        | expected, desired | Atomic                               |

### Transfer

| Format                                                   | Description                         |
| -------------------------------------------------------- | ----------------------------------- |
| `["next", "<sid>"]`                                      | Sequential transfer                 |
| `["branch", "<condition>", "<true_sid>", "<false_sid>"]` | Conditional branch                  |
| `["switch", "<variable>", {"<label>": "<sid>", ...}]`    | Multi-way branch                    |
| `"return"`                                               | Function end (string, not an array) |

### BusinessGoal

```json
{
  "id": "workers_return",
  "desc": "Both workers reach return",
  "marking": { "worker.s5": 1, "mtx": 1 },
  "variables": { "ready": true }
}
```

`desc`, `marking`, and `variables` may be omitted. Keys in `marking` may be: a declared resource name; a control location of the form `function.sid`; or a raw CVN place id starting with `cp_`, `rp_`, `wp_`, or `ra_`. Do not use display forms such as `cp(worker, ret)` or `rp(mtx)`. Goal token counts mean the minimum number that must be reached; for Channel/Condvar resources that start empty, use 0 for an emptiness check. `variables` uses CVN variable names and JSON scalar values.

---

## Validation Pipeline

The validator runs 8 passes in a fixed order; each pass emits diagnostics independently:

```
structure  →  names  →  types  →  compat  →  protection
    E0xx       E1xx      E2xx     E3xx        E7xx

→  concurrency  →  locks  →  control
       E4xx        E5xx      E6xx
```

---

## Error Code Reference

All errors are located by JSON path, e.g. `functions[1].body[3].op`.

### E0xx — Structural errors

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

### E1xx — Name resolution

| Code | Name              | Severity | Description                                                                                      |
| ---- | ----------------- | :------: | ------------------------------------------------------------------------------------------------ |
| E101 | UndefinedResource |  error   | Resource name referenced in an op is not in resources                                            |
| E102 | UndefinedFunction |  error   | Function name referenced by spawn/call/join/await has no fn definition |
| E103 | UndefinedSid      |  error   | Transfer target sid is not in the current function body                                          |
| E104 | DuplicateResource |  error   | Duplicate resource name in resources                                                             |
| E105 | DuplicateFunction |  error   | Duplicate function name in functions                                    |
| E106 | DuplicateSid      |  error   | Duplicate sid within the same function body                                                      |
| E107 | UndefinedEntry    |  error   | Entry function name does not exist in functions                                                  |

### E2xx — Type errors

| Code | Name                    | Severity | Description                                                                           |
| ---- | ----------------------- | :------: | ------------------------------------------------------------------------------------- |
| E201 | BranchCondNotBool       |  error   | branch condition is not a comparison expression (missing `==`/`!=`/`>`/`<`/`>=`/`<=`) |
| E202 | SwitchVarNotEnumOrInt   |  error   | switch variable type is not Enum or Int                                               |
| E203 | WriteTypeMismatch       |  error   | write value type does not match the Var's base                                        |
| E204 | StoreTypeMismatch       |  error   | store value type does not match the Atomic's base                                     |
| E205 | CasTypeMismatch         |  error   | cas argument types do not match the Atomic's base                                     |
| E206 | SendTypeMismatch        |  error   | send value type does not match the Channel's base                                     |
| E207 | SwitchCaseLabelMismatch |  error   | switch case label is not a valid variant of the target Enum                           |

### E3xx — Resource–operation compatibility

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
| E310 | UnknownResourceAction |  error   | `res_op` uses an action not in the CIR contract                      |
| E311 | ResourceActionArity   |  error   | `res_op` action argument count does not match the CIR contract       |

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

### E4xx — Concurrency pairing

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

### E5xx — Lock safety

| Code | Name                | Severity | Description                                                        |
| ---- | ------------------- | :------: | ------------------------------------------------------------------ |
| E501 | LockWithoutDrop     |  error   | lock without a corresponding drop on some control-flow path        |
| E502 | DropWithoutLock     |  error   | drop without a preceding matching lock                             |
| E503 | DoubleLock          |  error   | same resource locked twice on one path without an intervening drop |
| E504 | SyncLockAcrossAwait |  error   | Sync lock held across an await point in an async function          |
| E505 | LockOrderViolation  |  error   | inconsistent lock acquisition order across paths (ABBA deadlock)   |

### E6xx — Control flow

| Code | Name                 | Severity | Description                                     |
| ---- | -------------------- | :------: | ----------------------------------------------- |
| E601 | UnreachableStatement | warning  | statement unreachable from the entry            |
| E602 | MissingReturn        |  error   | a control-flow path that does not end in return |
| E603 | BranchTargetsSame    | warning  | branch true/false targets are the same          |
| E604 | SwitchNotExhaustive  |  error   | switch does not cover all Enum variants         |
| E605 | InfiniteLoopNoExit   | warning  | loop with no exit and no blocking operation     |

### E7xx — Protection mapping

| Code | Name                   | Severity | Description                                           |
| ---- | ---------------------- | :------: | ----------------------------------------------------- |
| E701 | ProtectionTargetNotVar |  error   | protection left-hand side is not a Var-typed resource |
| E702 | ProtectionLockNotLock  |  error   | protection right-hand side is not a Mutex or RwLock   |
| E703 | AtomicInProtection     |  error   | Atomic resource appears in protection                 |
| E704 | VarWithoutProtection   | warning  | Var resource does not appear in protection            |
| E705 | DuplicateProtection    |  error   | same Var appears more than once in protection         |

---

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

---

## Project structure

```
src/
  main.rs              Entry: read JSON → deserialize → validate → emit report
  lib.rs               Module declarations
  ast.rs               IR type definitions (Program, Resource, Op, Transfer, etc.)
  diagnostic.rs        Diagnostic types (Diagnostic, ValidationReport)
  validate/
    mod.rs             Validation entry: chains the 8 passes
    structure.rs       E0xx  Structural validity
    names.rs           E1xx  Name resolution
    types.rs           E2xx  Type checking
    compat.rs          E3xx  Resource–operation compatibility
    protection.rs      E7xx  Protection mapping
    concurrency.rs     E4xx  Concurrency pairing
    locks.rs           E5xx  Lock safety (includes E309)
    control.rs         E6xx  Control flow
examples/
  producer_consumer.json    Producer–consumer
  async_workers.json        Async tasks + semaphore + Channel
  with_summary.json         Body-less function call chain
  state_machine.json        State machine + Switch
  complex_rwlock.json       RwLock + Condvar combined example
```

---

## wait semantics

CIR semantics of `wait(cv, lock_name)`: release the associated lock, block until woken, then re-acquire the lock.

Therefore, in lock-safety analysis, the net effect of `wait` is that lock state is unchanged (release followed immediately by re-acquire). When modeling a condvar wait loop, write it as:

```
s1: lock(mtx)            -- acquire lock
s2: read(cond)           -- check condition
    branch(cond, s4, s3)
s3: wait(cv, mtx)        -- release lock, wait, re-acquire lock
    next(s2)             -- back to condition check, not back to lock
s4: ...                  -- condition satisfied; continue (lock still held)
```
