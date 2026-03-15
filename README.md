# CEIR — CIR Validator

CIR (Concurrency Intermediate Representation) 并发中间表示的静态验证器。读取 CIR JSON 文件，执行 9 轮校验，输出结构化诊断报告。

## 快速开始

```bash
cargo build --release
./target/release/ceir examples/producer_consumer.json
```

输出为 JSON 格式的 `ValidationReport`：

```json
{
  "valid": true,
  "diagnostics": []
}
```

若存在错误，`valid` 为 `false`，`diagnostics` 包含所有诊断项，进程以 exit code 1 退出。

---

## CIR JSON 格式规范

### 顶层结构

```json
{
  "program": "<程序名>",
  "resources": [ ... ],
  "protection": [ ... ],
  "functions": [ ... ],
  "fn_summaries": [ ... ],
  "entry": "<入口函数名>"
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|:----:|------|
| `program` | string | 是 | 程序名称 |
| `resources` | array | 是 | 共享资源声明 |
| `protection` | array | 是 | 保护映射（可为空） |
| `functions` | array | 是 | 函数定义，至少包含 entry 函数 |
| `fn_summaries` | array | 否 | 未建模函数的摘要 |
| `entry` | string | 是 | 入口函数名 |

### Resource

**同步原语** (`kind: "sync"`)：

```json
{"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}
{"name": "sem", "kind": "sync", "type": "Semaphore", "mode": "Async", "count": 3}
{"name": "tx",  "kind": "sync", "type": "Channel", "mode": "Async", "base": "Int"}
```

| type | mode | count | base |
|------|:----:|:-----:|:----:|
| Mutex | 必填 | — | — |
| RwLock | 必填 | — | — |
| Condvar | 必填 | — | — |
| Semaphore | 必填 | 必填 | — |
| Channel | 必填 | — | 必填 |

**共享变量** (`kind: "var"`)：

```json
{"name": "count", "kind": "var", "type": "Var",    "base": "Int", "init": 0}
{"name": "flag",  "kind": "var", "type": "Atomic", "base": "Bool", "init": false}
```

**base_type 取值**：

| 值 | 说明 | init 示例 |
|----|------|-----------|
| `"Bool"` | 布尔 | `true` |
| `"Int"` | 整数 | `0` |
| `"Float"` | 浮点 | `3.14` |
| `"String"` | 字符串 | `""` |
| `{"Enum": ["A","B"]}` | 枚举 | `"A"` |
| `{"Struct": {"x":"Int"}}` | 结构体 | `{"x": 0}` |
| `{"Array": {"elem":"Int","len":10}}` | 定长数组 | `[]` |

### Protection

```json
{"var": "counter", "lock": "mtx"}
```

每个 `Var` 最多出现一次。`Atomic` 不出现在 protection 中。

### Function

```json
{
  "name": "main",
  "kind": "normal",
  "body": [
    {"sid": "s1", "op": ["spawn", "worker"], "transfer": ["next", "s2"]},
    {"sid": "s2", "op": "return",            "transfer": "return"}
  ]
}
```

`kind` 取值：`"normal"` / `"async"` / `"closure"`

### Operation (op)

| 格式 | 说明 |
|------|------|
| `["res_op", "<资源>", "<action>", ...]` | 共享资源操作 |
| `["spawn", "<函数名>"]` | 创建 OS 线程 |
| `["spawn_async", "<函数名>"]` | 创建异步任务 |
| `["join", "<函数名>"]` | 等待线程 |
| `["await", "<函数名>"]` | 等待异步任务 |
| `["call", "<函数名>"]` | 同步调用 |
| `"return"` | 函数返回（字符串，非数组） |

**res_op action 清单**：

| action | 参数 | 适用类型 |
|--------|------|----------|
| `lock` | 无 | Mutex, RwLock |
| `read` | 无 | RwLock(读锁), Var(读值) |
| `write` | val | Var |
| `drop` | 无 | Mutex, RwLock |
| `wait` | lock_name | Condvar |
| `notify` | 无 | Condvar |
| `notify_all` | 无 | Condvar |
| `acquire` | 无 | Semaphore |
| `release` | 无 | Semaphore |
| `send` | val | Channel |
| `recv` | 无 | Channel |
| `load` | 无 | Atomic |
| `store` | val | Atomic |
| `cas` | expected, desired | Atomic |

### Transfer

| 格式 | 说明 |
|------|------|
| `["next", "<sid>"]` | 顺序转移 |
| `["branch", "<条件>", "<true_sid>", "<false_sid>"]` | 条件分支 |
| `["switch", "<变量>", {"<label>": "<sid>", ...}]` | 多路分支 |
| `"return"` | 函数结束（字符串，非数组） |

### FnSummary

```json
{
  "name": "validate",
  "reads": ["counter"],
  "writes": [],
  "callees": ["helper"],
  "has_concurrency": false
}
```

---

## 验证流程

验证器按固定顺序执行 9 轮校验，每轮独立产出诊断：

```
structure  →  names  →  types  →  compat  →  protection
    E0xx       E1xx      E2xx     E3xx        E7xx

→  concurrency  →  locks  →  control  →  summary
       E4xx        E5xx      E6xx        E8xx
```

---

## 错误码参考

所有错误通过 JSON path 定位，如 `functions[1].body[3].op`。

### E0xx — 结构错误

JSON 反序列化成功后，对结构合法性的补充检查。

| 码 | 名称 | 严重性 | 说明 |
|----|------|:------:|------|
| E000 | JsonParseError | error | JSON 语法错误或顶层结构不合法，无法反序列化 |
| E001 | MissingField | error | 资源声明缺少按 type 必填的字段（如 Semaphore 缺 `count`） |
| E004 | EmptyBody | error | 函数 body 为空数组 |
| E005 | InvalidSidFormat | error | sid 格式不是 `"s"` + 数字（如 `"s1"`、`"s10"`） |
| E008 | InvalidKind | error | 资源 `kind` 不是 `"sync"` / `"var"`，或 sync `type` 值非法 |
| E009 | InvalidMode | error | `mode` 不是 `"Sync"` / `"Async"` |
| E010 | InvalidFnKind | error | 函数 `kind` 不是 `"normal"` / `"async"` / `"closure"` |
| E208 | InitValueTypeMismatch | error | 资源初始值类型与声明的 `base` 不匹配 |

### E1xx — 名称解析

| 码 | 名称 | 严重性 | 说明 |
|----|------|:------:|------|
| E101 | UndefinedResource | error | op 中引用的资源名不在 resources 中 |
| E102 | UndefinedFunction | error | spawn/call/join/await 引用的函数名无 fn 定义也无 fn_summary |
| E103 | UndefinedSid | error | transfer 目标 sid 不在当前函数 body 中 |
| E104 | DuplicateResource | error | resources 中出现重复的资源名 |
| E105 | DuplicateFunction | error | functions / fn_summaries 中出现重复的函数名 |
| E106 | DuplicateSid | error | 同一函数 body 内出现重复的 sid |
| E107 | UndefinedEntry | error | entry 指向的函数名在 functions 中不存在 |

### E2xx — 类型错误

| 码 | 名称 | 严重性 | 说明 |
|----|------|:------:|------|
| E201 | BranchCondNotBool | error | branch 条件不是比较表达式（缺少 `==`/`!=`/`>`/`<`/`>=`/`<=`） |
| E202 | SwitchVarNotEnumOrInt | error | switch 变量类型不是 Enum 或 Int |
| E203 | WriteTypeMismatch | error | write 值类型与 Var 的 base 不匹配 |
| E204 | StoreTypeMismatch | error | store 值类型与 Atomic 的 base 不匹配 |
| E205 | CasTypeMismatch | error | cas 参数类型与 Atomic 的 base 不匹配 |
| E206 | SendTypeMismatch | error | send 值类型与 Channel 的 base 不匹配 |
| E207 | SwitchCaseLabelMismatch | error | switch case label 不是目标 Enum 的合法变体 |

### E3xx — 资源-操作兼容性

| 码 | 名称 | 严重性 | 说明 |
|----|------|:------:|------|
| E301 | LockOnNonLock | error | 对非 Mutex/RwLock 资源执行 lock/drop |
| E302 | ReadLockOnNonRwLock | error | 对 Mutex 执行 read（应使用 lock） |
| E303 | WaitOnNonCondvar | error | 对非 Condvar 资源执行 wait/notify/notify_all |
| E304 | WaitLockNotExist | error | wait 的 lock_name 不是已声明的 Mutex/RwLock |
| E305 | AcquireOnNonSemaphore | error | 对非 Semaphore 资源执行 acquire/release |
| E306 | SendOnNonChannel | error | 对非 Channel 资源执行 send/recv |
| E307 | LoadOnNonAtomic | error | 对非 Atomic 资源执行 load/store/cas |
| E308 | ReadWriteOnNonVar | error | 对非 Var 资源执行 read(读值)/write |
| E309 | VarAccessWithoutLock | error | 对受保护的 Var 读写时未持有对应锁 |

**操作-资源兼容矩阵**：

| action | Mutex | RwLock | Condvar | Semaphore | Channel | Atomic | Var |
|--------|:-----:|:------:|:-------:|:---------:|:-------:|:------:|:---:|
| lock | ok | ok(写) | E303 | E305 | E306 | E307 | E308 |
| read | E302 | ok(读) | E303 | E305 | E306 | E307 | ok(值) |
| write | E301 | E301 | E303 | E305 | E306 | E307 | ok(值) |
| drop | ok | ok | E303 | E305 | E306 | E307 | E308 |
| wait | E303 | E303 | ok | E305 | E306 | E307 | E308 |
| notify | E303 | E303 | ok | E305 | E306 | E307 | E308 |
| notify_all | E303 | E303 | ok | E305 | E306 | E307 | E308 |
| acquire | E301 | E301 | E303 | ok | E306 | E307 | E308 |
| release | E301 | E301 | E303 | ok | E306 | E307 | E308 |
| send | E301 | E301 | E303 | E305 | ok | E307 | E308 |
| recv | E301 | E301 | E303 | E305 | ok | E307 | E308 |
| load | E301 | E301 | E303 | E305 | E306 | ok | E308 |
| store | E301 | E301 | E303 | E305 | E306 | ok | E308 |
| cas | E301 | E301 | E303 | E305 | E306 | ok | E308 |

### E4xx — 并发配对

| 码 | 名称 | 严重性 | 说明 |
|----|------|:------:|------|
| E401 | SpawnWithoutJoin | warning | spawn 无对应 join |
| E402 | JoinWithoutSpawn | error | join 无对应 spawn |
| E403 | SpawnAsyncWithoutAwait | warning | spawn_async 无对应 await |
| E404 | AwaitWithoutSpawnAsync | error | await 无对应 spawn_async |
| E405 | SyncSpawnPairedWithAwait | error | spawn 与 await 配对（应为 join） |
| E406 | AsyncSpawnPairedWithJoin | error | spawn_async 与 join 配对（应为 await） |
| E407 | JoinInAsyncContext | warning | async 函数中使用 join 可能阻塞运行时 |
| E408 | AwaitInSyncContext | error | normal 函数中使用 await |

### E5xx — 锁安全

| 码 | 名称 | 严重性 | 说明 |
|----|------|:------:|------|
| E501 | LockWithoutDrop | error | 某条控制流路径上 lock 无对应 drop |
| E502 | DropWithoutLock | error | drop 前无对应 lock |
| E503 | DoubleLock | error | 同一路径上同一资源 lock 两次未先 drop |
| E504 | SyncLockAcrossAwait | error | async 函数中持有 Sync 锁跨越 await 点 |
| E505 | LockOrderViolation | error | 多把锁在不同路径上获取顺序不一致（ABBA 死锁） |

### E6xx — 控制流

| 码 | 名称 | 严重性 | 说明 |
|----|------|:------:|------|
| E601 | UnreachableStatement | warning | 从入口不可达的语句 |
| E602 | MissingReturn | error | 存在不以 return 结尾的控制流路径 |
| E603 | BranchTargetsSame | warning | branch 的 true/false 目标相同 |
| E604 | SwitchNotExhaustive | error | switch 未覆盖 Enum 的所有变体 |
| E605 | InfiniteLoopNoExit | warning | 无出口且无阻塞操作的循环 |

### E7xx — 保护映射

| 码 | 名称 | 严重性 | 说明 |
|----|------|:------:|------|
| E701 | ProtectionTargetNotVar | error | protection 左侧不是 Var 类型资源 |
| E702 | ProtectionLockNotLock | error | protection 右侧不是 Mutex 或 RwLock |
| E703 | AtomicInProtection | error | Atomic 资源出现在 protection 中 |
| E704 | VarWithoutProtection | warning | Var 资源未出现在 protection 中 |
| E705 | DuplicateProtection | error | 同一 Var 在 protection 中重复出现 |

### E8xx — FnSummary

| 码 | 名称 | 严重性 | 说明 |
|----|------|:------:|------|
| E801 | SummaryResourceNotExist | error | reads/writes 中的资源名不在 resources 中 |
| E802 | SummaryCalleeNotExist | error | callees 中的函数名无定义 |
| E803 | SummaryConflictWithBody | error | 同一函数既有 fn body 又有 fn_summary |
| E804 | SummaryMissingConcurrency | error | has_concurrency=false 但 callee 有并发 |

---

## 诊断输出格式

每条诊断包含以下字段：

```json
{
  "code": "E501",
  "severity": "error",
  "message": "lock 'mtx' not dropped on return path in function 'worker'",
  "path": "functions[1].body[3]",
  "fix_hint": "add drop() before return"
}
```

| 字段 | 说明 |
|------|------|
| `code` | 错误码（如 `E501`） |
| `severity` | `"error"` 或 `"warning"`，仅 error 影响 `valid` 字段 |
| `message` | 人类可读的错误描述 |
| `path` | JSON path 定位（可选） |
| `fix_hint` | 修复建议（可选） |

---

## 项目结构

```
src/
  main.rs              入口：读取 JSON → 反序列化 → 验证 → 输出报告
  lib.rs               模块声明
  ast.rs               IR 类型定义（Program, Resource, Op, Transfer 等）
  diagnostic.rs        诊断类型（Diagnostic, ValidationReport）
  validate/
    mod.rs             验证入口：串联 9 轮 pass
    structure.rs       E0xx  结构合法性
    names.rs           E1xx  名称解析
    types.rs           E2xx  类型检查
    compat.rs          E3xx  资源-操作兼容性
    protection.rs      E7xx  保护映射
    concurrency.rs     E4xx  并发配对
    locks.rs           E5xx  锁安全（含 E309）
    control.rs         E6xx  控制流
    summary.rs         E8xx  FnSummary 一致性
examples/
  producer_consumer.json    生产者-消费者
  async_workers.json        异步任务 + 信号量 + Channel
  with_summary.json         FnSummary 调用链
  state_machine.json        状态机 + Switch
  complex_rwlock.json       读写锁 + Condvar 综合
```

---

## wait 语义说明

`wait(cv, lock_name)` 的 CIR 语义：释放关联锁，阻塞等待，唤醒后重新获取锁。

因此在锁安全分析中，`wait` 的净效果是锁状态不变（释放后立即重新获取）。建模时 condvar wait 循环应写为：

```
s1: lock(mtx)            -- 获取锁
s2: read(cond)           -- 检查条件
    branch(cond, s4, s3)
s3: wait(cv, mtx)        -- 释放锁、等待、重获锁
    next(s2)             -- 回到条件检查，不是回到 lock
s4: ...                  -- 条件满足，继续执行（锁仍持有）
```
