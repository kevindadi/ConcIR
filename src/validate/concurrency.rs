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
        let Some(call) = &block.call else {
            return;
        };
        let info = OpInfo {
            fn_kind: f.kind.clone(),
            fn_name: f.name.clone(),
            path: format!("{}.call", Program::block_path(mi, fi, si)),
        };
        match call {
            Call::Spawn { handle, .. } | Call::SpawnBatch { handle, .. } => {
                spawns.entry(handle.clone()).or_default().push(info);
            }
            Call::Join { handle, .. } | Call::JoinAll { handle, .. } => {
                joins.entry(handle.clone()).or_default().push(info);
            }
            Call::AsyncCall { handle, .. } => {
                async_spawns.entry(handle.clone()).or_default().push(info);
            }
            Call::Await { handle, .. } => {
                awaits.entry(handle.clone()).or_default().push(info);
            }
            _ => {}
        }
    });

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
                        format!("async_call handle '{name}' is paired with join; use await instead"),
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
