use std::collections::{BTreeSet, HashMap, HashSet};

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::validate::types::{build_resource_type_map, ResType};

/// E5xx (+ E309): Lock safety analysis via CFG path traversal.
pub fn check(program: &Program, source: &str, diags: &mut Vec<Diagnostic>) {
    let rt_map = build_resource_type_map(program);

    let lock_resources: HashSet<&str> = rt_map
        .iter()
        .filter(|(_, v)| matches!(v, ResType::Mutex | ResType::RwLock))
        .map(|(k, _)| k.as_str())
        .collect();

    let sync_lock_resources: HashSet<&str> = program
        .resources
        .iter()
        .filter_map(|r| match &r.kind {
            ResourceKind::Sync(SyncType::Mutex(Mode::Sync)) => Some(r.name.value.as_str()),
            ResourceKind::Sync(SyncType::RwLock(Mode::Sync)) => Some(r.name.value.as_str()),
            _ => None,
        })
        .collect();

    // Build protection map: var_name -> lock_name
    let protection_map: HashMap<String, String> = program
        .protections
        .iter()
        .map(|p| (p.var_name.value.clone(), p.lock_name.value.clone()))
        .collect();

    for f in &program.functions {
        if f.statements.is_empty() {
            continue;
        }

        let cfg = build_cfg(f);

        check_lock_drop_pairing(f, &cfg, &lock_resources, source, diags);
        check_sync_lock_across_await(f, &cfg, &sync_lock_resources, source, diags);
        check_lock_ordering(f, &cfg, &lock_resources, source, diags);
        check_var_access_without_lock(f, &cfg, &lock_resources, &protection_map, source, diags);
    }
}

struct Cfg {
    /// index -> list of successor indices
    successors: Vec<Vec<usize>>,
}

fn build_cfg(f: &Function) -> Cfg {
    let sid_to_idx: HashMap<String, usize> = f
        .statements
        .iter()
        .enumerate()
        .map(|(i, s)| (s.sid.value.clone(), i))
        .collect();

    let n = f.statements.len();
    let mut successors = vec![Vec::new(); n];

    for (i, stmt) in f.statements.iter().enumerate() {
        match &stmt.transfer {
            Transfer::Next => {
                if i + 1 < n {
                    successors[i].push(i + 1);
                }
            }
            Transfer::Branch(_, t, fl) => {
                if let Some(&ti) = sid_to_idx.get(&t.value) {
                    successors[i].push(ti);
                }
                if let Some(&fi) = sid_to_idx.get(&fl.value) {
                    successors[i].push(fi);
                }
            }
            Transfer::Switch(_, cases) => {
                for c in cases {
                    if let Some(&ci) = sid_to_idx.get(&c.target.value) {
                        successors[i].push(ci);
                    }
                }
            }
            Transfer::Return => {
                // Terminal node — no successors
            }
        }
    }

    let _ = sid_to_idx; // used during construction
    Cfg { successors }
}

/// DFS traversal tracking held locks at each node.
/// Detects E501 (lock without drop), E502 (drop without lock), E503 (double lock).
fn check_lock_drop_pairing(
    f: &Function,
    cfg: &Cfg,
    lock_resources: &HashSet<&str>,
    source: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let n = f.statements.len();
    if n == 0 {
        return;
    }

    // BFS/DFS with state: held_locks set at each node
    // Use worklist algorithm: visited[node] = set of held-lock sets we've seen
    let mut visited: Vec<HashSet<BTreeSet<String>>> = vec![HashSet::new(); n];
    let mut worklist: Vec<(usize, BTreeSet<String>)> = vec![(0, BTreeSet::new())];

    while let Some((idx, mut held)) = worklist.pop() {
        if visited[idx].contains(&held) {
            continue;
        }
        visited[idx].insert(held.clone());

        let stmt = &f.statements[idx];
        if let Op::ResOp(ref res, ref action) = stmt.op {
            let res_name = &res.value;
            if lock_resources.contains(res_name.as_str()) {
                match action {
                    Action::Lock | Action::Read => {
                        if held.contains(res_name) {
                            diags.push(
                                Diagnostic::error(
                                    "E503",
                                    format!(
                                        "double lock on '{}' in function '{}' without prior drop",
                                        res_name, f.name.value
                                    ),
                                )
                                .with_span(stmt.span, source)
                                .with_fix("add drop before re-locking"),
                            );
                        }
                        held.insert(res_name.clone());
                    }
                    Action::Drop => {
                        if !held.contains(res_name) {
                            diags.push(
                                Diagnostic::error(
                                    "E502",
                                    format!(
                                        "drop without lock for '{}' in function '{}'",
                                        res_name, f.name.value
                                    ),
                                )
                                .with_span(stmt.span, source)
                                .with_fix("add lock before drop, or remove the drop"),
                            );
                        }
                        held.remove(res_name);
                    }
                    _ => {}
                }
            }
        }

        // Check E501 at return: held locks not dropped
        if matches!(stmt.transfer, Transfer::Return) || matches!(stmt.op, Op::Return) {
            for lock in &held {
                diags.push(
                    Diagnostic::error(
                        "E501",
                        format!(
                            "lock '{}' not dropped on return path in function '{}'",
                            lock, f.name.value
                        ),
                    )
                    .with_span(stmt.span, source)
                    .with_fix("add drop() before return"),
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
    source: &str,
    diags: &mut Vec<Diagnostic>,
) {
    if f.kind != FnKind::Async || sync_locks.is_empty() {
        return;
    }

    let n = f.statements.len();
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

        let stmt = &f.statements[idx];

        // Track sync lock state
        if let Op::ResOp(ref res, ref action) = stmt.op {
            if sync_locks.contains(res.value.as_str()) {
                match action {
                    Action::Lock | Action::Read => {
                        held.insert(res.value.clone());
                    }
                    Action::Drop => {
                        held.remove(&res.value);
                    }
                    _ => {}
                }
            }
        }

        // Check if this is an await point with held sync locks
        if matches!(stmt.op, Op::Await(_)) && !held.is_empty() {
            for lock in &held {
                diags.push(
                    Diagnostic::error(
                        "E504",
                        format!(
                            "Sync-mode lock '{}' held across await point in async function '{}'",
                            lock, f.name.value
                        ),
                    )
                    .with_span(stmt.span, source)
                    .with_fix("drop the lock before await and re-acquire after, or use Async-mode lock"),
                );
            }
        }

        for &succ in &cfg.successors[idx] {
            worklist.push((succ, held.clone()));
        }
    }
}

/// E505: Lock ordering violation — detect inconsistent lock acquisition order.
fn check_lock_ordering(
    f: &Function,
    cfg: &Cfg,
    lock_resources: &HashSet<&str>,
    source: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let n = f.statements.len();
    if n == 0 {
        return;
    }

    // Collect lock acquisition sequences on all paths
    let mut all_orders: Vec<Vec<String>> = Vec::new();
    let mut visited: HashSet<(usize, Vec<String>)> = HashSet::new();
    let mut stack: Vec<(usize, Vec<String>, BTreeSet<String>)> =
        vec![(0, Vec::new(), BTreeSet::new())];

    let max_iterations = n * 100; // prevent combinatorial explosion
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

        let stmt = &f.statements[idx];
        if let Op::ResOp(ref res, ref action) = stmt.op {
            let name = &res.value;
            if lock_resources.contains(name.as_str()) {
                match action {
                    Action::Lock | Action::Read => {
                        if !held.contains(name) {
                            order.push(name.clone());
                            held.insert(name.clone());
                        }
                    }
                    Action::Drop => {
                        held.remove(name);
                    }
                    _ => {}
                }
            }
        }

        if matches!(stmt.transfer, Transfer::Return) || matches!(stmt.op, Op::Return) {
            if order.len() >= 2 {
                all_orders.push(order.clone());
            }
            continue;
        }

        if cfg.successors[idx].is_empty() {
            if order.len() >= 2 {
                all_orders.push(order.clone());
            }
        }

        for &succ in &cfg.successors[idx] {
            stack.push((succ, order.clone(), held.clone()));
        }
    }

    // Compare all lock orderings for conflicts
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
                                f.name.value,
                                all_orders[i].join(", "),
                                all_orders[j].join(", "),
                            ),
                        )
                        .with_span(f.span, source)
                        .with_fix("use a consistent lock acquisition order across all paths"),
                    );
                }
            }
        }
    }
}

/// E309: Var read/write without holding the required protection lock.
fn check_var_access_without_lock(
    f: &Function,
    cfg: &Cfg,
    lock_resources: &HashSet<&str>,
    protection_map: &HashMap<String, String>,
    source: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let n = f.statements.len();
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

        let stmt = &f.statements[idx];

        // Track lock state
        if let Op::ResOp(ref res, ref action) = stmt.op {
            if lock_resources.contains(res.value.as_str()) {
                match action {
                    Action::Lock | Action::Read => {
                        held.insert(res.value.clone());
                    }
                    Action::Drop => {
                        held.remove(&res.value);
                    }
                    _ => {}
                }
            }

            // Check Var read/write against protection map
            if matches!(action, Action::Read | Action::Write(_)) {
                if let Some(required_lock) = protection_map.get(&res.value) {
                    if !held.contains(required_lock) {
                        diags.push(
                            Diagnostic::error(
                                "E309",
                                format!(
                                    "access to protected Var '{}' without holding lock '{}' in function '{}'",
                                    res.value, required_lock, f.name.value
                                ),
                            )
                            .with_span(stmt.span, source)
                            .with_fix("acquire the lock before accessing this variable"),
                        );
                    }
                }
            }
        }

        for &succ in &cfg.successors[idx] {
            worklist.push((succ, held.clone()));
        }
    }
}

fn has_order_conflict(a: &[String], b: &[String]) -> bool {
    // Check if any pair of locks appears in opposite order
    for i in 0..a.len() {
        for j in (i + 1)..a.len() {
            let l1 = &a[i];
            let l2 = &a[j];
            // In sequence `a`, l1 comes before l2
            // Check if in `b`, l2 comes before l1
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
