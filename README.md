# async-bugs

Rust `after_analysis` 静态分析框架骨架。你可以专注实现分析算法，框架负责接入 `rustc_driver`。

## 环境要求

1. nightly 工具链（仓库已提供 `rust-toolchain.toml`）
2. 组件：`rustc-dev`、`rust-src`、`llvm-tools-preview`

## 快速开始

```bash
cargo +nightly check
```

通过 Cargo wrapper 方式运行分析（推荐）：

```bash
RUSTC_WORKSPACE_WRAPPER=/Users/kevin/local-repos/async-bugs/target/debug/async-bugs cargo +nightly check
```

## 入口结构

- `src/main.rs`：`rustc_driver` 回调接入，`after_analysis` 生命周期钩子
- `src/analysis.rs`：规则引擎、诊断模型、JSON 报告器

## 规则扩展方式

1. 在 `src/analysis.rs` 实现 `AsyncBugRule`
2. 在 `default_async_bug_pass` 里 `registry.register(YourRule)`
3. 在 `check` 方法里基于 `RuleContext` 和 `TyCtxt` 产出 `BugDiagnostic`

## JSON 报告

支持两种方式配置输出路径：

- CLI 参数：`--async-bugs-report /abs/path/report.json`
- 环境变量：`ASYNC_BUGS_REPORT_JSON=/abs/path/report.json`

报告格式：

```json
{
  "crate_name": "your_crate",
  "diagnostics": [
    {
      "rule_id": "demo.list_async_fns",
      "severity": "info",
      "message": "found async fn `foo`",
      "location": {
        "file": "src/lib.rs",
        "line": 10,
        "column": 5
      }
    }
  ]
}
```
