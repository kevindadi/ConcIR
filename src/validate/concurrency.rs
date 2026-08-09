use std::collections::HashMap;

use crate::ast::*;
use crate::diagnostic::Diagnostic;

/// E4xx: Concurrency pairing checks.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let mut spawns: HashMap<String, Vec<OpInfo>> = HashMap::new();
    let mut joins: HashMap<String, Vec<OpInfo>> = HashMap::new();
    let mut async_spawns: HashMap<String, Vec<OpInfo>> = HashMap::new();
    let mut awaits: HashMap<String, Vec<OpInfo>> = HashMap::new();

    for (fi, f) in program.functions.iter().enumerate() {
        for (si, stmt) in f.body.iter().enumerate() {
            let info = OpInfo {
                fn_kind: f.kind.clone(),
                fn_name: f.name.clone(),
                path: format!("functions[{fi}].body[{si}].op"),
            };
            match &stmt.op {
                Op::Spawn(t) => spawns.entry(t.clone()).or_default().push(info),
                Op::Join(t) => joins.entry(t.clone()).or_default().push(info),
                Op::SpawnAsync(t) => async_spawns.entry(t.clone()).or_default().push(info),
                Op::Await(t) => awaits.entry(t.clone()).or_default().push(info),
                _ => {}
            }
        }
    }

    // E401: spawn without join
    for (name, infos) in &spawns {
        if !joins.contains_key(name) {
            for info in infos {
                diags.push(
                    Diagnostic::warning(
                        "E401",
                        format!("spawn('{name}') has no matching join('{name}')"),
                    )
                    .with_path(&info.path)
                    .with_fix("add join() or confirm this is fire-and-forget"),
                );
            }
        }
    }

    // E402: join without spawn
    for (name, infos) in &joins {
        if !spawns.contains_key(name) {
            for info in infos {
                diags.push(
                    Diagnostic::error(
                        "E402",
                        format!("join('{name}') has no matching spawn('{name}')"),
                    )
                    .with_path(&info.path)
                    .with_fix("add spawn() before join, or remove the join"),
                );
            }
        }
    }

    // E403: spawn_async without await
    for (name, infos) in &async_spawns {
        if !awaits.contains_key(name) {
            for info in infos {
                diags.push(
                    Diagnostic::warning(
                        "E403",
                        format!("spawn_async('{name}') has no matching await('{name}')"),
                    )
                    .with_path(&info.path)
                    .with_fix("add await() or change to spawn+join"),
                );
            }
        }
    }

    // E404: await without spawn_async
    for (name, infos) in &awaits {
        if !async_spawns.contains_key(name) {
            for info in infos {
                diags.push(
                    Diagnostic::error(
                        "E404",
                        format!("await('{name}') has no matching spawn_async('{name}')"),
                    )
                    .with_path(&info.path)
                    .with_fix("add spawn_async() before await, or remove the await"),
                );
            }
        }
    }

    // E405: spawn paired with await (should be join)
    for (name, infos) in &awaits {
        if spawns.contains_key(name) && !async_spawns.contains_key(name) {
            for info in infos {
                diags.push(
                    Diagnostic::error(
                        "E405",
                        format!("spawn('{name}') is paired with await('{name}'); use join instead"),
                    )
                    .with_path(&info.path)
                    .with_fix("change await() to join()"),
                );
            }
        }
    }

    // E406: spawn_async paired with join (should be await)
    for (name, infos) in &joins {
        if async_spawns.contains_key(name) && !spawns.contains_key(name) {
            for info in infos {
                diags.push(
                    Diagnostic::error(
                        "E406",
                        format!(
                            "spawn_async('{name}') is paired with join('{name}'); use await instead"
                        ),
                    )
                    .with_path(&info.path)
                    .with_fix("change join() to await()"),
                );
            }
        }
    }

    // E407: join in async context
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
                    .with_fix("use spawn_async + await, or use spawn_blocking"),
                );
            }
        }
    }

    // E408: await in sync context
    for infos in awaits.values() {
        for info in infos {
            if info.fn_kind == "normal" {
                diags.push(
                    Diagnostic::error(
                        "E408",
                        format!("await() in non-async function '{}'", info.fn_name),
                    )
                    .with_path(&info.path)
                    .with_fix("change the function to async, or use join instead"),
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
