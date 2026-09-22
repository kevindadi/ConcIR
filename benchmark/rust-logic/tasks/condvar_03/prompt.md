# 水位：先看条件再等待

用 safe Rust 写一个小程序，只使用标准库的 `std::thread` 和 `std::sync`。

## 场景

`producer` 在 `mtx` 下把 `level` 写成 1，然后通知 `cv`。`consumer` 在同一把锁下等待，直到 `level == 1` 成立，然后返回。通知可能发生在等待之前，所以等待前必须先看共享条件。

## 约束

- 恰好一个生产者线程和一个消费者线程。
- 用 `Condvar`，等待时带上 `mtx` 的守卫。
- 不要依赖虚假唤醒。
- 不要使用 `unsafe`、`async`/`await`、`RwLock`、`Once`，也不要使用无界的 `mpsc::channel`。有界通信用 `mpsc::sync_channel`。
- 锁守卫离开作用域就释放。不要在仍持有某把 `Mutex` 时再次获取它。
- 两个工作线程用 `thread::scope` 启动，并在入口线程里等待它们结束。

## 完成标准

- 不会死锁。
- 两个工作线程都返回。
- `level` 最终为 1，并且消费者已经观察到它。
