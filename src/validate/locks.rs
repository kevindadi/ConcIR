use std::collections::{BTreeSet, HashMap, HashSet};

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::env::{NameEnv, SlotKind};
use crate::expr;
use crate::fqn;
use crate::validate::types::{build_resource_type_map, ResType};

/// E5xx (+ E309): Lock safety analysis via CFG path traversal.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let rt_map = build_resource_type_map(program);

    let lock_resources: HashSet<&str> = rt_map
        .iter()
        .filter(|(_, v)| matches!(v, ResType::Mutex | ResType::RwLock))
        .map(|(k, _)| k.as_str())
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
    let sync_lock_refs: HashSet<&str> = sync_lock_resources.iter().map(String::as_str).collect();

    let protection_map: HashMap<String, String> = program
        .modules
        .iter()
        .flat_map(|m| m.protection.iter())
        .map(|p| (p.var.clone(), p.lock.clone()))
        .collect();

    for (mi, m) in program.modules.iter().enumerate() {
        for (fi, f) in m.functions.iter().enumerate() {
            if f.body.is_empty() {
                continue;
            }

            let cfg = build_cfg(f);
            let fn_path = Program::fn_path(mi, fi);

            check_lock_drop_pairing(f, &cfg, &lock_resources, &fn_path, diags);
            check_sync_lock_across_await(f, &cfg, &sync_lock_refs, &fn_path, diags);
            check_lock_ordering(f, &cfg, &lock_resources, &fn_path, diags);
            check_var_access_without_lock(program, m, f, &cfg, &protection_map, &fn_path, diags);
            check_requires_held(program, m, f, &cfg, &fn_path, diags);
        }
    }
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
    f: &Function,
    cfg: &Cfg,
    lock_resources: &HashSet<&str>,
    fn_path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let n = f.body.len();
    if n == 0 {
        return;
    }

    let mut visited: Vec<HashSet<BTreeSet<String>>> = vec![HashSet::new(); n];
    let mut worklist: Vec<(usize, BTreeSet<String>)> = vec![(0, BTreeSet::new())];

    while let Some((idx, mut held)) = worklist.pop() {
        if visited[idx].contains(&held) {
            continue;
        }
        visited[idx].insert(held.clone());

        let stmt = &f.body[idx];
        let s = &stmt.op;
        if let Some(resource) = s.is_lock_acquire() {
            if lock_resources.contains(resource) {
                if held.contains(resource) {
                    diags.push(
                        Diagnostic::error(
                            "E503",
                            format!(
                                "double lock on '{resource}' in function '{}' without prior unlock",
                                f.name
                            ),
                        )
                        .with_path(format!("{fn_path}.body[{idx}]"))
                        .with_fix("unlock before re-locking"),
                    );
                }
                held.insert(resource.to_string());
            }
        } else if let Some(resource) = s.is_lock_release() {
            if lock_resources.contains(resource) {
                if !held.contains(resource) {
                    diags.push(
                        Diagnostic::error(
                            "E502",
                            format!(
                                "unlock without lock for '{resource}' in function '{}'",
                                f.name
                            ),
                        )
                        .with_path(format!("{fn_path}.body[{idx}]"))
                        .with_fix("lock before unlock, or remove the unlock"),
                    );
                }
                held.remove(resource);
            }
        }

        if stmt.is_return() {
            for lock in &held {
                diags.push(
                    Diagnostic::error(
                        "E501",
                        format!(
                            "lock '{lock}' not unlocked on return path in function '{}'",
                            f.name
                        ),
                    )
                    .with_path(format!("{fn_path}.body[{idx}]"))
                    .with_fix("add mutex_unlock/rwlock_unlock before return"),
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
    f: &Function,
    cfg: &Cfg,
    sync_locks: &HashSet<&str>,
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
    let mut worklist: Vec<(usize, BTreeSet<String>)> = vec![(0, BTreeSet::new())];

    while let Some((idx, mut held)) = worklist.pop() {
        if visited[idx].contains(&held) {
            continue;
        }
        visited[idx].insert(held.clone());

        let stmt = &f.body[idx];

        let s = &stmt.op;
        if let Some(resource) = s.is_lock_acquire() {
            if sync_locks.contains(resource) {
                held.insert(resource.to_string());
            }
        } else if let Some(resource) = s.is_lock_release() {
            if sync_locks.contains(resource) {
                held.remove(resource);
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
    f: &Function,
    cfg: &Cfg,
    lock_resources: &HashSet<&str>,
    fn_path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let n = f.body.len();
    if n == 0 {
        return;
    }

    let mut all_orders: Vec<Vec<String>> = Vec::new();
    let mut visited: HashSet<(usize, Vec<String>)> = HashSet::new();
    let mut stack: Vec<(usize, Vec<String>, BTreeSet<String>)> =
        vec![(0, Vec::new(), BTreeSet::new())];

    let max_iterations = n * 100;
    let mut iterations = 0;

    while let Some((idx, mut order, mut held)) = stack.pop() {
        iterations += 1;
        if iterations > max_iterations {
            break;
        }

        let key = (idx, order.clone());
        if visited.contains(&key) {
            continue;
        }
        visited.insert(key);

        let stmt = &f.body[idx];
        let s = &stmt.op;
        if let Some(resource) = s.is_lock_acquire() {
            if lock_resources.contains(resource) && !held.contains(resource) {
                order.push(resource.to_string());
                held.insert(resource.to_string());
            }
        } else if let Some(resource) = s.is_lock_release() {
            if lock_resources.contains(resource) {
                held.remove(resource);
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
                        .with_fix("use a consistent lock acquisition order across all paths"),
                    );
                }
            }
        }
    }
}

/// E309: Var read/write without holding the required protection lock.
///
/// Covers `read_shared` / `write_shared` of the Var itself, plus every
/// parsed r-value (guards, write exprs, call/spawn args, …) and
/// `switch.var` when the scrutinee is that Var.
fn check_var_access_without_lock(
    program: &Program,
    module: &Module,
    f: &Function,
    cfg: &Cfg,
    protection_map: &HashMap<String, String>,
    fn_path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let n = f.body.len();
    if n == 0 {
        return;
    }

    let env = NameEnv::build(program, module, f);
    let mut visited: Vec<HashSet<BTreeSet<String>>> = vec![HashSet::new(); n];
    let mut worklist: Vec<(usize, BTreeSet<String>)> = vec![(0, BTreeSet::new())];
    let mut reported: HashSet<(usize, String)> = HashSet::new();

    while let Some((idx, mut held)) = worklist.pop() {
        if visited[idx].contains(&held) {
            continue;
        }
        visited[idx].insert(held.clone());

        let stmt = &f.body[idx];

        for resource in protected_var_accesses(&stmt.op, &env) {
            let Some(required_lock) = required_lock(protection_map, &resource) else {
                continue;
            };
            if held.contains(required_lock) {
                continue;
            }
            let key = (idx, short_name(&resource).to_string());
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
                .with_fix("acquire the lock before accessing this variable"),
            );
        }

        let s = &stmt.op;
        if let Some(resource) = s.is_lock_acquire() {
            held.insert(resource.to_string());
        } else if let Some(resource) = s.is_lock_release() {
            held.remove(resource);
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
    fn_path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let n = f.body.len();
    if n == 0 {
        return;
    }

    let mut visited: Vec<HashSet<BTreeSet<String>>> = vec![HashSet::new(); n];
    let mut worklist: Vec<(usize, BTreeSet<String>)> = vec![(0, BTreeSet::new())];
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
                    let held_here = lock_held(&held, required, &owner.name, &module.name);
                    if held_here {
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
                        .with_fix("acquire the lock before this call, or drop requires_held on the callee"),
                    );
                }
            }
        }

        if let Some(resource) = stmt.op.is_lock_acquire() {
            held.insert(resource.to_string());
        } else if let Some(resource) = stmt.op.is_lock_release() {
            held.remove(resource);
        }

        for &succ in &cfg.successors[idx] {
            worklist.push((succ, held.clone()));
        }
    }
}

fn lock_held(
    held: &BTreeSet<String>,
    required: &str,
    callee_module: &str,
    caller_module: &str,
) -> bool {
    let req_q = fqn::qualify(callee_module, required);
    held.iter().any(|h| {
        if h == required || h == &req_q {
            return true;
        }
        let h_q = if fqn::is_fqn(h) {
            h.clone()
        } else {
            fqn::qualify(caller_module, h)
        };
        h_q == req_q
    })
}

fn short_name(name: &str) -> &str {
    fqn::split_fqn(name).map(|(_, e)| e).unwrap_or(name)
}

fn required_lock<'a>(protection_map: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    if let Some(lock) = protection_map.get(name) {
        return Some(lock.as_str());
    }
    protection_map.get(short_name(name)).map(String::as_str)
}

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
    names
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
