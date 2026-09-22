# 队列：调用期间不要持有同一把锁

用 safe Rust 写一个小程序，只使用标准库的 `std::thread` 和 `std::sync`。

## 场景

`push` 先把 `len` 加 1，然后调用 `fix`。`fix` 自己再获取 `head`，把 `len` 再加 1。`pop` 也在 `head` 下把 `len` 加 1。三个加一都发生后，`len` 等于 3。`push` 调用 `fix` 时必须已经释放 `head`，因为 Rust 的 `Mutex` 不可重入。

## 约束

- 工作线程是 `push` 和 `pop`。`fix` 是普通函数，不是第三个线程。
- 共享单元 `len` 只由 `head` 保护。
- 不要使用 `unsafe`、`async`/`await`、`RwLock`、`Once`，也不要使用无界的 `mpsc::channel`。有界通信用 `mpsc::sync_channel`。
- 锁守卫离开作用域就释放。不要在仍持有某把 `Mutex` 时再次获取它。
- 两个工作线程用 `thread::scope` 启动，并在入口线程里等待它们结束。

## 完成标准

- 不会死锁。
- `push` 和 `pop` 都返回。
- `len` 在两边都返回后等于 3。
