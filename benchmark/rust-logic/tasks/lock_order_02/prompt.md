# 库存：两把锁同一顺序

用 safe Rust 写一个小程序，只使用标准库的 `std::thread` 和 `std::sync`。

## 场景

这是一个库存更新。`left` 把 `stock` 增加 1，`right` 增加 3。更新时必须同时持有 `outer` 和 `inner`，并且两个线程都先获取 `outer`，再获取 `inner`。

## 约束

- 恰好两个工作线程，分别叫 `left` 和 `right`。
- 每把锁只表示协议顺序，数据放在单独的共享单元里。
- 不要使用 `unsafe`、`async`/`await`、`RwLock`、`Once`，也不要使用无界的 `mpsc::channel`。有界通信用 `mpsc::sync_channel`。
- 锁守卫离开作用域就释放。不要在仍持有某把 `Mutex` 时再次获取它。
- 两个工作线程用 `thread::scope` 启动，并在入口线程里等待它们结束。

## 完成标准

- 不会死锁。
- 两个工作线程都返回。
- `stock` 在两边都返回后等于 4。
