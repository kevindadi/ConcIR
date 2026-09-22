# 文档：调用期间不要持有同一把锁

用 safe Rust 写一个小程序，只使用标准库的 `std::thread` 和 `std::sync`。

## 场景

`editor` 先把 `rev` 加 1，然后调用 `save`。`save` 自己再获取 `doc`，把 `rev` 再加 1。`reader` 也在 `doc` 下把 `rev` 加 1。三个加一都发生后，`rev` 等于 3。`editor` 调用 `save` 时必须已经释放 `doc`，因为 Rust 的 `Mutex` 不可重入。

## 约束

- 工作线程是 `editor` 和 `reader`。`save` 是普通函数，不是第三个线程。
- 共享单元 `rev` 只由 `doc` 保护。
- 不要使用 `unsafe`、`async`/`await`、`RwLock`、`Once`，也不要使用无界的 `mpsc::channel`。有界通信用 `mpsc::sync_channel`。
- 锁守卫离开作用域就释放。不要在仍持有某把 `Mutex` 时再次获取它。
- 两个工作线程用 `thread::scope` 启动，并在入口线程里等待它们结束。

## 完成标准

- 不会死锁。
- `editor` 和 `reader` 都返回。
- `rev` 在两边都返回后等于 3。
