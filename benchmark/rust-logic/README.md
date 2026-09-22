# Rust 并发逻辑基准

30 道小题。目标语言是 safe Rust：内存安全由类型系统负责，这里只检查并发协议有没有做完、骨架里有没有死锁或丢失更新。

模型只应看到 `tasks/<id>/prompt.md`。评分对象是降到 ConcIR 之后的程序，用同一目录里的 `contract.json` 跑：

```bash
cargo build --release
./target/release/concir-backend explore tasks/<id>/candidate.json tasks/<id>/contract.json
```

`reference.json` 是正确协议的手写降级（`MutexGuard` 离开作用域写成显式 `mutex_unlock`），必须得到 `PASS`。`bug.json` 是对应的逻辑错误，必须得到 `FAIL`。这两份程序是预言，不要放进提示词。

`PASS` 只在搜索完整且每条性质都成立时出现。撞上状态或深度上界是 `UNKNOWN`，不算通过。

## 题型

| 家族 | 题号 | 正确协议 | 捆绑的错误 |
| --- | --- | --- | --- |
| `lock_order` | 01–05 | 两个线程按同一顺序拿两把锁 | 其中一个线程把顺序倒过来 |
| `double_lock` | 01–05 | 调用会再次加锁的函数之前先解锁 | 持有 `Mutex` 时调用它（不可重入） |
| `condvar` | 01–05 | 持锁检查条件，不成立再 `wait` | 不看条件就 `wait`，通知可能已经发生 |
| `channel` | 01–05 | `sync_channel` 的发送次数等于接收次数 | 接收方多等一条消息 |
| `atomicity` | 01–05 | 在同一段临界区里读改写 | 读和写之间释放了锁 |
| `semaphore` | 01–05 | 每个线程获取 1 个许可并释放 | 忘记释放，或连续获取两次 |

每道题都是两个工作线程、一个模块。不用 `RwLock`、`async`、`Once` 和无界通道。

核对预言：

```bash
cargo test --test rust_logic_bench
```

重新生成 JSON：

```bash
python benchmark/rust-logic/generate.py
```
