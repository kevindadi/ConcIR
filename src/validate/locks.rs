use std::collections::{BTreeSet, HashMap, HashSet};

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::env::{NameEnv, SlotKind};
use crate::expr;
use crate::fqn;
use crate::validate::types::{build_resource_type_map, ResType};

/// E5xx (+ E309, E512): Lock safety analysis via CFG path traversal.
///
/// Resource identity is resolved to fully-qualified names (`module::entity`)
/// so that same-named resources in different modules never collide. The
/// `requires_held` contract of a function is used as the **entry condition**
/// of the analysis: a callee that relies on its caller holding a lock is not
/// flagged. `condvar_wait` must hold its paired lock. Destination writes
/// (`atomic_load` / `atomic_cas` / `channel_recv` / `read_shared` / `call`)
/// into a protected `Var` are checked for lock ownership.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let rt_map = build_resource_type_map(program);

    let lock_resources: HashSet<String> = rt_map
        .iter()
        .filter(|(_, v)| matches!(v, ResType::Mutex | ResType::RwLock))
        .filter_map(|(k, _)| canonical_resource(program, k))
        .collect();

    let sync_lock_resources: HashSet<String> = program
        .modules
        .iter()
        .flat_map(|m| m.resources.iter())
        .filter(|r| {
            r.kind == "sync"
                && (r.res_type == "Mutex" || r.res_type == "RwLock")
                && r.mode.as_deref() == Some("Sync")
        })
        .map(|r| r.name.clone())
        .collect();

    // Protection keyed by the protected Var's FQN → lock FQN.
    let protection_map: HashMap<String, String> = program
        .modules
        .iter()
        .flat_map(|m| {
            m.protection.iter().filter_map(move |p| {
                let var = program
                    .lookup_resource(&m.name, &p.var)
                    .map(|(owner, r)| fqn::fqn(&owner.name, &r.name))?;
                let lock = program
                    .lookup_resource(&m.name, &p.lock)
                    .map(|(owner, r)| fqn::fqn(&owner.name, &r.name))?;
                Some((var, lock))
            })
        })
        .collect();

    for (mi, m) in program.modules.iter().enumerate() {
        for (fi, f) in m.functions.iter().enumerate() {
            if f.body.is_empty() {
                continue;
            }

            let cfg = build_cfg(f);
            let fn_path = Program::fn_path(mi, fi);
            let entry_held: BTreeSet<String> = f
                .locks
                .requires_held
                .iter()
                .map(|name| {
                    canonical_resource(program, &fqn::qualify(&m.name, name))
                        .unwrap_or_else(|| name.clone())
                })
                .collect();

            check_lock_drop_pairing(
                program,
                m,
                f,
                &cfg,
                &lock_resources,
                &entry_held,
                &fn_path,
                diags,
            );
            check_sync_lock_across_await(
                program,
                m,
                f,
                &cfg,
                &sync_lock_resources,
                &entry_held,
                &fn_path,
                diags,
            );
            check_lock_ordering(
                program,
                m,
                f,
                &cfg,
                &lock_resources,
                &entry_held,
                &fn_path,
                diags,
            );
            check_var_access_without_lock(
                program,
                m,
                f,
                &cfg,
                &protection_map,
                &entry_held,
                &fn_path,
                diags,
            );
            check_requires_held(program, m, f, &cfg, &entry_held, &fn_path, diags);
            check_condvar_ownership(program, m, f, &cfg, &entry_held, &fn_path, diags);
        }
    }
}

/// Resolve a resource name (short or FQN) to its canonical FQN.
fn canonical_resource(program: &Program, name: &str) -> Option<String> {
    let (owner, r) = program.lookup_resource("", name)?;
    Some(fqn::fqn(&owner.name, &r.name))
}

fn canonical_from(program: &Program, module: &str, name: &str) -> String {
    program
        .lookup_resource(module, name)
        .map(|(owner, r)| fqn::fqn(&owner.name, &r.name))
        .unwrap_or_else(|| name.to_string())
}

struct Cfg {
    successors: Vec<Vec<usize>>,
}

fn build_cfg(f: &Function) -> Cfg {
    let sid_to_idx: HashMap<&str, usize> = f
        .body
        .iter()
        .enumerate()
        .map(|(i, s)| (s.sid.as_str(), i))
        .collect();

    let n = f.body.len();
    let mut successors = vec![Vec::new(); n];

    for (i, _) in f.body.iter().enumerate() {
        for t in f.successors(i) {
            if let Some(&ti) = sid_to_idx.get(t) {
                successors[i].push(ti);
            }
        }
    }

    Cfg { successors }
}

/// E501, E502, E503: lock/drop pairing via worklist algorithm.
fn check_lock_drop_pairing(
    program: &Program,
    module: &Module,
    f: &Function,
    cfg: &Cfg,
    lock_resources: &HashSet<String>,
    entry_held: &BTreeSet<String>,
    fn_path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let n = f.body.len();
    if n == 0 {
        return;
    }

    let mut visited: Vec<HashSet<BTreeSet<String>>> = vec![HashSet::new(); n];
    let mut worklist: Vec<(usize, BTreeSet<String>)> = vec![(0, entry_held.clone())];

    while let Some((idx, mut held)) = worklist.pop() {
        if visited[idx].contains(&held) {
            continue;
        }
        visited[idx].insert(held.clone());

        let stmt = &f.body[idx];
        let s = &stmt.op;
        if let Some(resource) = s.is_lock_acquire() {
            let canon = canonical_from(program, &module.name, resource);
            if lock_resources.contains(&canon) {
                if held.contains(&canon) {
                    diags.push(
                        Diagnostic::error(
                            "E503",
                            format!(
                                "double lock on '{resource}' in function '{}' without prior unlock",
                                f.name
                            ),
                        )
                        .with_path(format!("{fn_path}.body[{idx}]"))
                        .with_location(Program::stmt_location(module, f, stmt))
                        .with_fix("unlock before re-locking"),
                    );
                }
                held.insert(canon);
            }
        } else if let Some(resource) = s.is_lock_release() {
            let canon = canonical_from(program, &module.name, resource);
            if lock_resources.contains(&canon) {
                if !held.contains(&canon) {
                    diags.push(
                        Diagnostic::error(
                            "E502",
                            format!(
                                "unlock without lock for '{resource}' in function '{}'",
                                f.name
                            ),
                        )
                        .with_path(format!("{fn_path}.body[{idx}]"))
                        .with_location(Program::stmt_location(module, f, stmt))
                        .with_fix("lock before unlock, or remove the unlock"),
                    );
                }
                held.remove(&canon);
            }
        }

        if stmt.is_return() {
            // Only locks acquired inside the body are required to be released;
            // `requires_held` locks may be intentionally transferred.
            for lock in held.difference(entry_held) {
                diags.push(
                    Diagnostic::error(
                        "E501",
                        format!(
                            "lock '{lock}' not unlocked on return path in function '{}'",
                            f.name
                        ),
                    )
                    .with_path(format!("{fn_path}.body[{idx}]"))
                    .with_location(Program::stmt_location(module, f, stmt))
                    .with_fix("add mutex_unlock/rwlock_unlock before return, or release the lock"),
                );
            }
        }

        for &succ in &cfg.successors[idx] {
            worklist.push((succ, held.clone()));
        }
    }
}

/// E504: Sync-mode lock held across await point in async function.
fn check_sync_lock_across_await(
    program: &Program,
    module: &Module,
    f: &Function,
    cfg: &Cfg,
    sync_locks: &HashSet<String>,
    entry_held: &BTreeSet<String>,
    fn_path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    if f.kind != "async" || sync_locks.is_empty() {
        return;
    }

    let n = f.body.len();
    if n == 0 {
        return;
    }

    let mut visited: Vec<HashSet<BTreeSet<String>>> = vec![HashSet::new(); n];
    let mut worklist: Vec<(usize, BTreeSet<String>)> = vec![(0, entry_held.clone())];

    while let Some((idx, mut held)) = worklist.pop() {
        if visited[idx].contains(&held) {
            continue;
        }
        visited[idx].insert(held.clone());

        let stmt = &f.body[idx];

        let s = &stmt.op;
        if let Some(resource) = s.is_lock_acquire() {
            let canon = canonical_from(program, &module.name, resource);
            if sync_locks.contains(&canon) {
                held.insert(canon);
            }
        } else if let Some(resource) = s.is_lock_release() {
            let canon = canonical_from(program, &module.name, resource);
            if sync_locks.contains(&canon) {
                held.remove(&canon);
            }
        }

        if s.is_await_like() && !held.is_empty() {
            for lock in &held {
                diags.push(
                        Diagnostic::error(
                            "E504",
                            format!(
                                "Sync-mode lock '{lock}' held across await point in async function '{}'",
                                f.name
                            ),
                        )
                        .with_path(format!("{fn_path}.body[{idx}]"))
                        .with_location(Program::stmt_location(module, f, stmt))
                        .with_fix("unlock before await and re-acquire after, or use Async-mode lock"),
                    );
            }
        }

        for &succ in &cfg.successors[idx] {
            worklist.push((succ, held.clone()));
        }
    }
}

/// E505: Lock ordering violation.
fn check_lock_ordering(
    program: &Program,
    module: &Module,
    f: &Function,
    cfg: &Cfg,
    lock_resources: &HashSet<String>,
    entry_held: &BTreeSet<String>,
    fn_path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let n = f.body.len();
    if n == 0 {
        return;
    }

    let mut all_orders: Vec<Vec<String>> = Vec::new();
    let mut visited: HashSet<(usize, Vec<String>, BTreeSet<String>)> = HashSet::new();
    let initial_order: Vec<String> = entry_held.iter().cloned().collect();
    let mut stack: Vec<(usize, Vec<String>, BTreeSet<String>)> =
        vec![(0, initial_order, entry_held.clone())];

    let max_iterations = n * 100;
    let mut iterations = 0;

    while let Some((idx, mut order, mut held)) = stack.pop() {
        iterations += 1;
        if iterations > max_iterations {
            break;
        }

        let key = (idx, order.clone(), held.clone());
        if visited.contains(&key) {
            continue;
        }
        visited.insert(key);

        let stmt = &f.body[idx];
        let s = &stmt.op;
        if let Some(resource) = s.is_lock_acquire() {
            let canon = canonical_from(program, &module.name, resource);
            if lock_resources.contains(&canon) && !held.contains(&canon) {
                order.push(canon.clone());
                held.insert(canon);
            }
        } else if let Some(resource) = s.is_lock_release() {
            let canon = canonical_from(program, &module.name, resource);
            if lock_resources.contains(&canon) {
                held.remove(&canon);
            }
        }

        if stmt.is_return() {
            if order.len() >= 2 {
                all_orders.push(order.clone());
            }
            continue;
        }

        if cfg.successors[idx].is_empty() && order.len() >= 2 {
            all_orders.push(order.clone());
        }

        for &succ in &cfg.successors[idx] {
            stack.push((succ, order.clone(), held.clone()));
        }
    }

    let mut reported = HashSet::new();
    for i in 0..all_orders.len() {
        for j in (i + 1)..all_orders.len() {
            if has_order_conflict(&all_orders[i], &all_orders[j]) {
                let key = (
                    all_orders[i].clone().into_iter().collect::<BTreeSet<_>>(),
                    all_orders[j].clone().into_iter().collect::<BTreeSet<_>>(),
                );
                if reported.insert(key) {
                    diags.push(
                        Diagnostic::error(
                            "E505",
                            format!(
                                "lock order violation in function '{}': path acquires [{}] but another acquires [{}]",
                                f.name,
                                all_orders[i].join(", "),
                                all_orders[j].join(", "),
                            ),
                        )
                        .with_path(fn_path.to_string())
                        .with_location(Program::fn_location(module, f))
                        .with_fix("use a consistent lock acquisition order across all paths"),
                    );
                }
            }
        }
    }
}

/// E309: Var read/write without holding the required protection lock.
///
/// Covers `read_shared` / `write_shared` of the Var itself, parsed r-values
/// (guards, write exprs, call/spawn args, …), `switch.var`, and every
/// destination write into a protected Var.
fn check_var_access_without_lock(
    program: &Program,
    module: &Module,
    f: &Function,
    cfg: &Cfg,
    protection_map: &HashMap<String, String>,
    entry_held: &BTreeSet<String>,
    fn_path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let n = f.body.len();
    if n == 0 {
        return;
    }

    let env = NameEnv::build(program, module, f);
    let mut visited: Vec<HashSet<BTreeSet<String>>> = vec![HashSet::new(); n];
    let mut worklist: Vec<(usize, BTreeSet<String>)> = vec![(0, entry_held.clone())];
    let mut reported: HashSet<(usize, String)> = HashSet::new();

    while let Some((idx, mut held)) = worklist.pop() {
        if visited[idx].contains(&held) {
            continue;
        }
        visited[idx].insert(held.clone());

        let stmt = &f.body[idx];

        for resource in protected_var_accesses(&stmt.op, &env) {
            let var_fqn = canonical_from(program, &module.name, &resource);
            let Some(required_lock) = protection_map.get(&var_fqn) else {
                continue;
            };
            if held.contains(required_lock) {
                continue;
            }
            let key = (idx, var_fqn.clone());
            if !reported.insert(key) {
                continue;
            }
            diags.push(
                Diagnostic::error(
                    "E309",
                    format!(
                        "access to protected Var '{resource}' without holding lock '{required_lock}' in function '{}'",
                        f.name
                    ),
                )
                .with_path(format!("{fn_path}.body[{idx}]"))
                .with_location(Program::stmt_location(module, f, stmt))
                .with_fix("acquire the lock before accessing this variable"),
            );
        }

        let s = &stmt.op;
        if let Some(resource) = s.is_lock_acquire() {
            held.insert(canonical_from(program, &module.name, resource));
        } else if let Some(resource) = s.is_lock_release() {
            held.remove(&canonical_from(program, &module.name, resource));
        }

        for &succ in &cfg.successors[idx] {
            worklist.push((succ, held.clone()));
        }
    }
}

/// E803: `call` of a function that declares `requires_held` must hold those locks.
fn check_requires_held(
    program: &Program,
    module: &Module,
    f: &Function,
    cfg: &Cfg,
    entry_held: &BTreeSet<String>,
    fn_path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let n = f.body.len();
    if n == 0 {
        return;
    }

    let mut visited: Vec<HashSet<BTreeSet<String>>> = vec![HashSet::new(); n];
    let mut worklist: Vec<(usize, BTreeSet<String>)> = vec![(0, entry_held.clone())];
    let mut reported: HashSet<(usize, String)> = HashSet::new();

    while let Some((idx, mut held)) = worklist.pop() {
        if visited[idx].contains(&held) {
            continue;
        }
        visited[idx].insert(held.clone());

        let stmt = &f.body[idx];
        if let Op::Func { func, .. } = &stmt.op {
            if let Some((owner, callee)) = program.lookup_function(&module.name, func) {
                for required in &callee.locks.requires_held {
                    let req_fqn = fqn::fqn(&owner.name, required);
                    if held.contains(&req_fqn) {
                        continue;
                    }
                    let key = (idx, required.clone());
                    if !reported.insert(key) {
                        continue;
                    }
                    diags.push(
                        Diagnostic::error(
                            "E803",
                            format!(
                                "call to '{}' requires lock '{required}' held, but it is not held \
                                 in function '{}'",
                                callee.name, f.name
                            ),
                        )
                        .with_path(format!("{fn_path}.body[{idx}]"))
                        .with_location(Program::stmt_location(module, f, stmt))
                        .with_fix("acquire the lock before this call, or drop requires_held on the callee"),
                    );
                }
            }
        }

        if let Some(resource) = stmt.op.is_lock_acquire() {
            held.insert(canonical_from(program, &module.name, resource));
        } else if let Some(resource) = stmt.op.is_lock_release() {
            held.remove(&canonical_from(program, &module.name, resource));
        }

        for &succ in &cfg.successors[idx] {
            worklist.push((succ, held.clone()));
        }
    }
}

/// E512: `condvar_wait` requires the paired lock to be held.
fn check_condvar_ownership(
    program: &Program,
    module: &Module,
    f: &Function,
    cfg: &Cfg,
    entry_held: &BTreeSet<String>,
    fn_path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let n = f.body.len();
    if n == 0 {
        return;
    }

    let mut visited: Vec<HashSet<BTreeSet<String>>> = vec![HashSet::new(); n];
    let mut worklist: Vec<(usize, BTreeSet<String>)> = vec![(0, entry_held.clone())];
    let mut reported: HashSet<(usize, String)> = HashSet::new();

    while let Some((idx, mut held)) = worklist.pop() {
        if visited[idx].contains(&held) {
            continue;
        }
        visited[idx].insert(held.clone());

        let stmt = &f.body[idx];
        if let Op::CondvarWait { lock, .. } = &stmt.op {
            let canon = canonical_from(program, &module.name, lock);
            if !held.contains(&canon) {
                let key = (idx, lock.clone());
                if reported.insert(key) {
                    diags.push(
                        Diagnostic::error(
                            "E512",
                            format!(
                                "condvar_wait in function '{}' requires lock '{lock}' to be held",
                                f.name
                            ),
                        )
                        .with_path(format!("{fn_path}.body[{idx}]"))
                        .with_location(Program::stmt_location(module, f, stmt))
                        .with_fix("acquire the paired lock before waiting on the condvar"),
                    );
                }
            }
        }

        if let Some(resource) = stmt.op.is_lock_acquire() {
            held.insert(canonical_from(program, &module.name, resource));
        } else if let Some(resource) = stmt.op.is_lock_release() {
            held.remove(&canonical_from(program, &module.name, resource));
        }

        for &succ in &cfg.successors[idx] {
            worklist.push((succ, held.clone()));
        }
    }
}

/// Names of `Var`/`Atomic` slots read by a statement, plus destination writes
/// into protected Vars.
fn protected_var_accesses(op: &Op, env: &NameEnv) -> Vec<String> {
    let mut names = Vec::new();
    if let Some((resource, _)) = op.shared_var_access() {
        names.push(resource.to_string());
    }
    if let Op::SeqHole { reads, writes, .. } = op {
        names.extend(reads.iter().cloned());
        names.extend(writes.iter().cloned());
    }
    if let Op::Switch { var, .. } = op {
        if env.get(var).is_some_and(|s| s.kind == SlotKind::Var) {
            names.push(var.clone());
        }
    }
    for text in op.rvalue_exprs() {
        if let Ok(expr) = expr::parse(text, env) {
            names.extend(expr.value_resource_names(env));
        }
    }
    names.extend(dst_writes(op));
    names
}

/// Explicit destination writes (`dst` / destination fields).
fn dst_writes(op: &Op) -> Vec<String> {
    match op {
        Op::AtomicLoad { dst, .. } => wr(dst),
        Op::AtomicCas { dst, .. } => wr(dst),
        Op::ChannelRecv { dst, .. } => wr(dst),
        Op::ReadShared { dst: Some(dst), .. } => wr(dst),
        Op::Func { dst: Some(dst), .. } => wr(dst),
        Op::Select { branches, .. } => branches
            .iter()
            .filter_map(|b| match &b.guard {
                SelectGuard::ChannelRecv { dst, .. } => Some(dst.clone()),
                _ => None,
            })
            .filter(|d| d != "_")
            .collect(),
        _ => Vec::new(),
    }
}

fn wr(dst: &str) -> Vec<String> {
    if dst == "_" {
        Vec::new()
    } else {
        vec![dst.to_string()]
    }
}

fn has_order_conflict(a: &[String], b: &[String]) -> bool {
    for i in 0..a.len() {
        for j in (i + 1)..a.len() {
            let l1 = &a[i];
            let l2 = &a[j];
            let pos_b1 = b.iter().position(|x| x == l1);
            let pos_b2 = b.iter().position(|x| x == l2);
            if let (Some(p1), Some(p2)) = (pos_b1, pos_b2) {
                if p2 < p1 {
                    return true;
                }
            }
        }
    }
    false
}
