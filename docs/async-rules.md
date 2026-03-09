# Rust 异步相关规则清单

## 1. rustc

| lint 名称 | 默认级别 | 作用 | 官方文档 |
|---|---|---|---|
| `must-not-suspend` | `allow` | 检测带 `#[must_not_suspend]` 的值跨 `await/yield` 持有 | [allowed-by-default#must-not-suspend](https://doc.rust-lang.org/rustc/lints/listing/allowed-by-default.html#must-not-suspend) |
| `closure-returning-async-block` | `allow` | 检测可重写为 async closure 的 `|| async { ... }` 形式 | [allowed-by-default#closure-returning-async-block](https://doc.rust-lang.org/rustc/lints/listing/allowed-by-default.html#closure-returning-async-block) |
| `async-fn-in-trait` | `warn` | 检测公开 trait 中的 `async fn` 定义（可移植性/语义约束提醒） | [warn-by-default#async-fn-in-trait](https://doc.rust-lang.org/rustc/lints/listing/warn-by-default.html#async-fn-in-trait) |
| `ungated-async-fn-track-caller` | `warn` | 检测在未启用对应 feature 时对 async fn 使用 `track_caller`（无效） | [warn-by-default#ungated-async-fn-track-caller](https://doc.rust-lang.org/rustc/lints/listing/warn-by-default.html#ungated-async-fn-track-caller) |
| `rtsan-nonblocking-async` | `warn` | 检测 `#[sanitize(realtime = "nonblocking")]` 与 async 使用不兼容场景 | [warn-by-default#rtsan-nonblocking-async](https://doc.rust-lang.org/rustc/lints/listing/warn-by-default.html#rtsan-nonblocking-async) |

## 2. Clippy

| lint 名称 | 默认级别 | 作用 | 官方文档 |
|---|---|---|---|
| `clippy::await-holding-lock` | `warn` | 在 async 函数中持有锁 guard 后再 `await` | [await_holding_lock](https://rust-lang.github.io/rust-clippy/master/index.html#await_holding_lock) |
| `clippy::await-holding-refcell-ref` | `warn` | 在 async 函数中持有 `RefCell` 借用后再 `await` | [await_holding_refcell_ref](https://rust-lang.github.io/rust-clippy/master/index.html#await_holding_refcell_ref) |
| `clippy::await-holding-invalid-type` | `warn` | 在 `await` 点跨越持有“配置为不允许”的类型 | [await_holding_invalid_type](https://rust-lang.github.io/rust-clippy/master/index.html#await_holding_invalid_type) |
| `clippy::let-underscore-future` | `warn` | `let _ = some_future();` 导致 Future 被忽略 | [let_underscore_future](https://rust-lang.github.io/rust-clippy/master/index.html#let_underscore_future) |
| `clippy::manual-async-fn` | `warn` | 手写返回 `impl Future` 的模式可改为 `async fn` | [manual_async_fn](https://rust-lang.github.io/rust-clippy/master/index.html#manual_async_fn) |
| `clippy::redundant-async-block` | `warn` | `async { fut.await }` 等冗余 async block | [redundant_async_block](https://rust-lang.github.io/rust-clippy/master/index.html#redundant_async_block) |
| `clippy::async-yields-async` | `deny` | async block 返回仍可 await 的类型（通常是错误封装） | [async_yields_async](https://rust-lang.github.io/rust-clippy/master/index.html#async_yields_async) |
| `clippy::future-not-send` | `allow` | 对外暴露的 Future 不是 `Send` | [future_not_send](https://rust-lang.github.io/rust-clippy/master/index.html#future_not_send) |
| `clippy::large-futures` | `allow` | Future 体积过大，可能导致栈/性能问题 | [large_futures](https://rust-lang.github.io/rust-clippy/master/index.html#large_futures) |
| `clippy::unused-async` | `allow` | `async fn` 内无 `await` | [unused_async](https://rust-lang.github.io/rust-clippy/master/index.html#unused_async) |
| `clippy::waker-clone-wake` | `warn` | 仅为 `wake` 而克隆 `Waker` | [waker_clone_wake](https://rust-lang.github.io/rust-clippy/master/index.html#waker_clone_wake) |

## 3. 实务建议（给异步静态分析框架）

1. 先把 `await-holding-*`、`let-underscore-future`、`future-not-send` 作为第一批对标规则。
2. 对 `allow` 级别但风险高的 lint（如 `future-not-send`、`unused-async`）在你的框架里提升为 `warn`。
3. 你的 `after_analysis` 自定义规则可重点覆盖 Clippy/rustc 暂未细化的业务语义（比如 runtime-specific 约束、跨 crate 调用约束、executor 绑定规则）。

## 4. 复现命令

```bash
rustc +nightly --version --verbose
cargo +nightly clippy -V
cargo +nightly clippy -- -Whelp | rg -i "async|await|future|suspend|waker"
```
