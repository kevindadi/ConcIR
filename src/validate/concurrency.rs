use std::collections::HashMap;

use crate::ast::*;
use crate::diagnostic::Diagnostic;

/// E4xx: Concurrency pairing — spawn/join and async_call/await pair on handles.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let mut spawns: HashMap<String, Vec<OpInfo>> = HashMap::new();
    let mut joins: HashMap<String, Vec<OpInfo>> = HashMap::new();
    let mut async_spawns: HashMap<String, Vec<OpInfo>> = HashMap::new();
    let mut awaits: HashMap<String, Vec<OpInfo>> = HashMap::new();

    program.walk_blocks(|mi, fi, si, _, f, block| {
        let path = format!("{}.statements", Program::block_path(mi, fi, si));
        for stmt in &block.statements {
            let info = OpInfo {
                fn_kind: f.kind.clone(),
                fn_name: f.name.clone(),
                path: path.clone(),
            };
            match stmt {
                Stmt::Spawn { handle, .. } | Stmt::SpawnBatch { handle, .. } => {
                    spawns.entry(handle.clone()).or_default().push(info);
                }
                Stmt::Join { handle, .. } | Stmt::JoinAll { handle, .. } => {
                    joins.entry(handle.clone()).or_default().push(info);
                }
                Stmt::AsyncCall { handle, .. } => {
                    async_spawns.entry(handle.clone()).or_default().push(info);
                }
                Stmt::Await { handle, .. } => {
                    awaits.entry(handle.clone()).or_default().push(info);
                }
                _ => {}
            }
        }
    });

    check_select_guards(program, diags);

    for (name, infos) in &spawns {
        if !joins.contains_key(name) {
            for info in infos {
                diags.push(
                    Diagnostic::warning(
                        "E401",
                        format!("spawn handle '{name}' has no matching join"),
                    )
                    .with_path(&info.path)
                    .with_fix("add join/join_all on this handle or confirm fire-and-forget"),
                );
            }
        }
    }
    for (name, infos) in &joins {
        if !spawns.contains_key(name) {
            for info in infos {
                diags.push(
                    Diagnostic::error(
                        "E402",
                        format!("join handle '{name}' has no matching spawn"),
                    )
                    .with_path(&info.path)
                    .with_fix("spawn onto this handle before join"),
                );
            }
        }
    }
    for (name, infos) in &async_spawns {
        if !awaits.contains_key(name) {
            for info in infos {
                diags.push(
                    Diagnostic::warning(
                        "E403",
                        format!("async_call handle '{name}' has no matching await"),
                    )
                    .with_path(&info.path)
                    .with_fix("add await on this handle"),
                );
            }
        }
    }
    for (name, infos) in &awaits {
        if !async_spawns.contains_key(name) {
            for info in infos {
                diags.push(
                    Diagnostic::error(
                        "E404",
                        format!("await handle '{name}' has no matching async_call"),
                    )
                    .with_path(&info.path)
                    .with_fix("async_call onto this handle before await"),
                );
            }
        }
    }
    for (name, infos) in &awaits {
        if spawns.contains_key(name) && !async_spawns.contains_key(name) {
            for info in infos {
                diags.push(
                    Diagnostic::error(
                        "E405",
                        format!("spawn handle '{name}' is paired with await; use join instead"),
                    )
                    .with_path(&info.path)
                    .with_fix("change await to join"),
                );
            }
        }
    }
    for (name, infos) in &joins {
        if async_spawns.contains_key(name) && !spawns.contains_key(name) {
            for info in infos {
                diags.push(
                    Diagnostic::error(
                        "E406",
                        format!(
                            "async_call handle '{name}' is paired with join; use await instead"
                        ),
                    )
                    .with_path(&info.path)
                    .with_fix("change join to await"),
                );
            }
        }
    }
    for infos in joins.values() {
        for info in infos {
            if info.fn_kind == "async" {
                diags.push(
                    Diagnostic::warning(
                        "E407",
                        format!(
                            "join() in async function '{}' may block the runtime",
                            info.fn_name
                        ),
                    )
                    .with_path(&info.path)
                    .with_fix("use async_call + await"),
                );
            }
        }
    }
    for infos in awaits.values() {
        for info in infos {
            if info.fn_kind == "normal" {
                diags.push(
                    Diagnostic::error(
                        "E408",
                        format!("await() in non-async function '{}'", info.fn_name),
                    )
                    .with_path(&info.path)
                    .with_fix("change the function to async, or use join"),
                );
            }
        }
    }
}

struct OpInfo {
    fn_kind: String,
    fn_name: String,
    path: String,
}

/// E409: `condvar_wait` is not a `select!` candidate in sync Rust.
/// Allowed only in an `async` function on an `Async`-mode Condvar; the
/// translator maps that guard to `Notify` / `watch` or a timeout race.
fn check_select_guards(program: &Program, diags: &mut Vec<Diagnostic>) {
    program.walk_blocks(|mi, fi, si, m, f, block| {
        let Terminator::Select { branches, .. } = &block.terminator else {
            return;
        };
        let path = format!("{}.terminator", Program::block_path(mi, fi, si));
        for branch in branches {
            let SelectGuard::CondvarWait { condvar, .. } = &branch.guard else {
                continue;
            };
            if f.kind != "async" {
                diags.push(
                    Diagnostic::error(
                        "E409",
                        format!(
                            "condvar_wait on '{condvar}' cannot be a select guard in non-async \
                             function '{}'; Condvar::wait is a blocking primitive",
                            f.name
                        ),
                    )
                    .with_path(path.clone())
                    .with_fix(
                        "use condvar_wait as a statement in a wait loop, or make the function \
                         async and use an Async-mode Condvar (codegen: Notify/watch)",
                    ),
                );
                continue;
            }
            if let Some((_, res)) = program.lookup_resource(&m.name, condvar) {
                if res.mode.as_deref() != Some("Async") {
                    diags.push(
                        Diagnostic::error(
                            "E409",
                            format!(
                                "select guard condvar_wait on '{condvar}' requires mode Async \
                                 (translator maps it to Notify/watch or a timeout race)"
                            ),
                        )
                        .with_path(path.clone())
                        .with_fix("set the Condvar's mode to Async, or wait outside select"),
                    );
                }
            }
        }
    });
}
