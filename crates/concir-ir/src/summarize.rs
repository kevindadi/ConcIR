//! Compositional module summary (`concir-backend summarize`, schema `concir-module-summary/1`).
//!
//! Computes per-module:
//! - exported/imported interfaces (`provides`, `requires`)
//! - intra- and inter-procedural lock-order edges (`first -> second`)
//! - global/cross-module lock-order cycle detection
//! - tau-collapsed quotient synchronization automaton per module (hiding
//!   `Nop`/`AssignLocal`/`ReadShared`/`WriteShared`/`SeqHole`/internal control
//!   via tau-closure and partition-refinement stuttering collapse).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::ast::{Function, Module, NameSet, Op, Program, RequireSet};
use crate::fqn;

pub const MODULE_SUMMARY_SCHEMA: &str = "concir-module-summary/1";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LockOrderEdge {
    pub first: String,
    pub second: String,
    pub function: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotientState {
    pub id: usize,
    pub function: String,
    pub entry: bool,
    pub accepting: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct QuotientTransition {
    pub from: usize,
    pub to: usize,
    pub op: String,
    pub resource: String,
    pub function: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleSummary {
    pub name: String,
    pub provides: NameSet,
    pub requires: RequireSet,
    pub lock_order_edges: Vec<LockOrderEdge>,
    pub quotient_states: Vec<QuotientState>,
    pub quotient_transitions: Vec<QuotientTransition>,
    pub cross_module_lock_cycle: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleSummaryReport {
    pub schema: String,
    pub program: String,
    pub cross_module_lock_cycle: bool,
    pub lock_cycles: Vec<Vec<String>>,
    pub modules: Vec<ModuleSummary>,
}

/// Compute compositional module summaries for `program`, optionally filtering
/// the returned `modules` list to `module_filter` (while still computing
/// cross-module lock-cycle detection across the whole program).
pub fn summarize_program(
    program: &Program,
    module_filter: Option<&str>,
) -> Result<ModuleSummaryReport, String> {
    let owned;
    let program = if program.has_structured_bodies() {
        let mut lowered = program.clone();
        lowered.ensure_lowered()?;
        owned = lowered;
        &owned
    } else {
        program
    };

    // Precompute transitive locks acquired by every function (FQN -> set of lock FQNs)
    // so inter-procedural calls while holding a lock emit cross-function/cross-module
    // lock-order edges.
    let fn_acquires = compute_transitive_acquires(program);

    // Collect lock-order edges per module.
    let mut edges_by_module: BTreeMap<String, Vec<LockOrderEdge>> = BTreeMap::new();
    let mut global_graph: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut edge_modules: HashMap<(String, String), BTreeSet<String>> = HashMap::new();

    for m in &program.modules {
        let mut mod_edges: BTreeSet<LockOrderEdge> = BTreeSet::new();
        for f in &m.functions {
            for edge in extract_function_lock_edges(program, m, f, &fn_acquires) {
                global_graph
                    .entry(edge.first.clone())
                    .or_default()
                    .insert(edge.second.clone());
                edge_modules
                    .entry((edge.first.clone(), edge.second.clone()))
                    .or_default()
                    .insert(m.name.clone());
                mod_edges.insert(edge);
            }
        }
        edges_by_module.insert(m.name.clone(), mod_edges.into_iter().collect());
    }

    // Detect lock cycles in the global lock-order graph via Tarjan's SCC.
    let lock_cycles = find_lock_cycles(&global_graph);
    let has_cycle = !lock_cycles.is_empty();

    // Determine which modules participate in any cycle.
    let mut cyclic_locks: HashSet<&str> = HashSet::new();
    for cyc in &lock_cycles {
        for l in cyc {
            cyclic_locks.insert(l.as_str());
        }
    }

    let mut modules_out = Vec::new();
    for m in &program.modules {
        if let Some(filter) = module_filter {
            if m.name != filter {
                continue;
            }
        }
        let edges = edges_by_module.remove(&m.name).unwrap_or_default();
        let mod_in_cycle = edges.iter().any(|e| {
            cyclic_locks.contains(e.first.as_str()) && cyclic_locks.contains(e.second.as_str())
        });
        let (quotient_states, quotient_transitions) = build_module_quotient(program, m);
        modules_out.push(ModuleSummary {
            name: m.name.clone(),
            provides: m.provides.clone(),
            requires: m.requires.clone(),
            lock_order_edges: edges,
            quotient_states,
            quotient_transitions,
            cross_module_lock_cycle: mod_in_cycle,
        });
    }

    if let Some(filter) = module_filter {
        if modules_out.is_empty() {
            return Err(format!("module '{filter}' not found in program"));
        }
    }

    Ok(ModuleSummaryReport {
        schema: MODULE_SUMMARY_SCHEMA.to_string(),
        program: program.program.clone(),
        cross_module_lock_cycle: has_cycle,
        lock_cycles,
        modules: modules_out,
    })
}

fn canon_resource(program: &Program, module: &str, name: &str) -> String {
    program
        .lookup_resource(module, name)
        .map(|(owner, r)| fqn::fqn(&owner.name, &r.name))
        .unwrap_or_else(|| fqn::qualify(module, name))
}

fn compute_transitive_acquires(program: &Program) -> HashMap<String, BTreeSet<String>> {
    let mut acquires: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut callees: HashMap<String, BTreeSet<String>> = HashMap::new();

    for m in &program.modules {
        for f in &m.functions {
            let f_fqn = fqn::fqn(&m.name, &f.name);
            let acq = acquires.entry(f_fqn.clone()).or_default();
            for l in &f.locks.acquires {
                acq.insert(canon_resource(program, &m.name, l));
            }
            for stmt in &f.body {
                if let Some(res) = stmt.op.is_lock_acquire() {
                    acq.insert(canon_resource(program, &m.name, res));
                }
                if let Op::Func { func, .. } = &stmt.op {
                    if let Some((owner, callee)) = program.lookup_function(&m.name, func) {
                        callees
                            .entry(f_fqn.clone())
                            .or_default()
                            .insert(fqn::fqn(&owner.name, &callee.name));
                    }
                }
            }
        }
    }

    // Least fixed point over call graph.
    let mut changed = true;
    while changed {
        changed = false;
        let keys: Vec<String> = acquires.keys().cloned().collect();
        for f_fqn in keys {
            let Some(cs) = callees.get(&f_fqn) else {
                continue;
            };
            let mut to_add = Vec::new();
            for c in cs {
                if let Some(c_acq) = acquires.get(c) {
                    to_add.extend(c_acq.iter().cloned());
                }
            }
            let entry = acquires.entry(f_fqn).or_default();
            for l in to_add {
                if entry.insert(l) {
                    changed = true;
                }
            }
        }
    }

    acquires
}

fn extract_function_lock_edges(
    program: &Program,
    module: &Module,
    f: &Function,
    fn_acquires: &HashMap<String, BTreeSet<String>>,
) -> Vec<LockOrderEdge> {
    let n = f.body.len();
    if n == 0 {
        return Vec::new();
    }

    let sid_to_idx: HashMap<&str, usize> = f
        .body
        .iter()
        .enumerate()
        .map(|(i, s)| (s.sid.as_str(), i))
        .collect();
    let mut succs = vec![Vec::new(); n];
    for i in 0..n {
        for t in f.successors(i) {
            if let Some(&ti) = sid_to_idx.get(t) {
                succs[i].push(ti);
            }
        }
    }

    let entry_held: BTreeSet<String> = f
        .locks
        .requires_held
        .iter()
        .map(|name| canon_resource(program, &module.name, name))
        .collect();

    let mut edges: BTreeSet<LockOrderEdge> = BTreeSet::new();
    let mut visited: Vec<HashSet<BTreeSet<String>>> = vec![HashSet::new(); n];
    let mut worklist: Vec<(usize, BTreeSet<String>)> = vec![(0, entry_held)];

    while let Some((idx, mut held)) = worklist.pop() {
        if !visited[idx].insert(held.clone()) {
            continue;
        }
        let op = &f.body[idx].op;
        if let Some(res) = op.is_lock_acquire() {
            let l2 = canon_resource(program, &module.name, res);
            for l1 in &held {
                if l1 != &l2 {
                    edges.insert(LockOrderEdge {
                        first: l1.clone(),
                        second: l2.clone(),
                        function: f.name.clone(),
                    });
                }
            }
            held.insert(l2);
        } else if let Some(res) = op.is_lock_release() {
            let l = canon_resource(program, &module.name, res);
            held.remove(&l);
        } else if let Op::Func { func, .. } = op {
            if let Some((owner, callee)) = program.lookup_function(&module.name, func) {
                let c_fqn = fqn::fqn(&owner.name, &callee.name);
                if let Some(c_locks) = fn_acquires.get(&c_fqn) {
                    for l1 in &held {
                        for l2 in c_locks {
                            if l1 != l2 {
                                edges.insert(LockOrderEdge {
                                    first: l1.clone(),
                                    second: l2.clone(),
                                    function: f.name.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }

        for &next in &succs[idx] {
            worklist.push((next, held.clone()));
        }
    }

    edges.into_iter().collect()
}

fn find_lock_cycles(graph: &BTreeMap<String, BTreeSet<String>>) -> Vec<Vec<String>> {
    let mut nodes: BTreeSet<String> = BTreeSet::new();
    for (u, vs) in graph {
        nodes.insert(u.clone());
        for v in vs {
            nodes.insert(v.clone());
        }
    }
    let node_list: Vec<String> = nodes.into_iter().collect();
    let idx_of: HashMap<&str, usize> = node_list
        .iter()
        .enumerate()
        .map(|(i, s)| (s.as_str(), i))
        .collect();
    let n = node_list.len();
    let mut adj = vec![Vec::new(); n];
    for (u, vs) in graph {
        let ui = idx_of[u.as_str()];
        for v in vs {
            adj[ui].push(idx_of[v.as_str()]);
        }
    }

    // Tarjan's SCC.
    struct State {
        index: usize,
        indices: Vec<Option<usize>>,
        lowlink: Vec<usize>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        sccs: Vec<Vec<usize>>,
    }
    fn strongconnect(v: usize, adj: &[Vec<usize>], st: &mut State) {
        st.indices[v] = Some(st.index);
        st.lowlink[v] = st.index;
        st.index += 1;
        st.stack.push(v);
        st.on_stack[v] = true;
        for &w in &adj[v] {
            if st.indices[w].is_none() {
                strongconnect(w, adj, st);
                st.lowlink[v] = st.lowlink[v].min(st.lowlink[w]);
            } else if st.on_stack[w] {
                st.lowlink[v] = st.lowlink[v].min(st.indices[w].unwrap());
            }
        }
        if st.lowlink[v] == st.indices[v].unwrap() {
            let mut scc = Vec::new();
            loop {
                let w = st.stack.pop().unwrap();
                st.on_stack[w] = false;
                scc.push(w);
                if w == v {
                    break;
                }
            }
            st.sccs.push(scc);
        }
    }

    let mut st = State {
        index: 0,
        indices: vec![None; n],
        lowlink: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        sccs: Vec::new(),
    };
    for v in 0..n {
        if st.indices[v].is_none() {
            strongconnect(v, &adj, &mut st);
        }
    }

    let mut cycles = Vec::new();
    for scc in st.sccs {
        let is_cycle = scc.len() >= 2 || (scc.len() == 1 && adj[scc[0]].contains(&scc[0]));
        if is_cycle {
            let mut names: Vec<String> = scc.into_iter().map(|i| node_list[i].clone()).collect();
            names.sort();
            cycles.push(names);
        }
    }
    cycles.sort();
    cycles
}

/// Classify a statement op into an observable synchronization label `(op, resource)`,
/// or `None` if it is silent (`tau`: `Nop`, `AssignLocal`, `ReadShared`, `WriteShared`,
/// `SeqHole`, `AbstractStep`, internal control).
fn sync_label(program: &Program, module: &Module, op: &Op) -> Option<(String, String)> {
    match op {
        Op::MutexLock { resource } => {
            Some(("mutex_lock".into(), canon_resource(program, &module.name, resource)))
        }
        Op::MutexUnlock { resource } => {
            Some(("mutex_unlock".into(), canon_resource(program, &module.name, resource)))
        }
        Op::RwLockRead { resource } => {
            Some(("rwlock_read".into(), canon_resource(program, &module.name, resource)))
        }
        Op::RwLockWrite { resource } => {
            Some(("rwlock_write".into(), canon_resource(program, &module.name, resource)))
        }
        Op::RwLockUnlock { resource } => {
            Some(("rwlock_unlock".into(), canon_resource(program, &module.name, resource)))
        }
        Op::CondvarWait { condvar, .. } => {
            Some(("condvar_wait".into(), canon_resource(program, &module.name, condvar)))
        }
        Op::CondvarNotify { condvar } => {
            Some(("condvar_notify".into(), canon_resource(program, &module.name, condvar)))
        }
        Op::CondvarNotifyAll { condvar } => Some((
            "condvar_notify_all".into(),
            canon_resource(program, &module.name, condvar),
        )),
        Op::SemaphoreAcquire { resource, .. } => {
            Some(("sem_acquire".into(), canon_resource(program, &module.name, resource)))
        }
        Op::SemaphoreRelease { resource, .. } => {
            Some(("sem_release".into(), canon_resource(program, &module.name, resource)))
        }
        Op::ChannelSend { channel, .. } => {
            Some(("channel_send".into(), canon_resource(program, &module.name, channel)))
        }
        Op::ChannelRecv { channel, .. } => {
            Some(("channel_recv".into(), canon_resource(program, &module.name, channel)))
        }
        Op::AtomicLoad { resource, .. } => {
            Some(("atomic_load".into(), canon_resource(program, &module.name, resource)))
        }
        Op::AtomicStore { resource, .. } => {
            Some(("atomic_store".into(), canon_resource(program, &module.name, resource)))
        }
        Op::AtomicCas { resource, .. } => {
            Some(("atomic_cas".into(), canon_resource(program, &module.name, resource)))
        }
        Op::Spawn { func, .. } => Some(("spawn".into(), fqn::qualify(&module.name, func))),
        Op::Scope { funcs } => {
            let names: Vec<String> = funcs
                .iter()
                .map(|f| fqn::qualify(&module.name, f))
                .collect();
            Some(("scope".into(), names.join(",")))
        }
        Op::Join { handle } => Some(("join".into(), handle.clone())),
        Op::Func { func, .. } => {
            // Cross-module calls are visible boundary actions; intra-module calls are internal.
            let target = fqn::qualify(&module.name, func);
            if fqn::split_fqn(&target).map(|(m, _)| m) != Some(module.name.as_str()) {
                Some(("call".into(), target))
            } else {
                None
            }
        }
        Op::Nop
        | Op::AssignLocal { .. }
        | Op::ReadShared { .. }
        | Op::WriteShared { .. }
        | Op::SeqHole { .. }
        | Op::AbstractStep { .. }
        | Op::Goto { .. }
        | Op::Branch { .. }
        | Op::Switch { .. }
        | Op::Return { .. }
        | Op::AsyncCall { .. }
        | Op::Await { .. }
        | Op::Select { .. } => None,
    }
}

fn build_module_quotient(
    program: &Program,
    module: &Module,
) -> (Vec<QuotientState>, Vec<QuotientTransition>) {
    let mut all_states = Vec::new();
    let mut all_transitions = Vec::new();
    let mut next_state_id = 0usize;

    for f in &module.functions {
        let n = f.body.len();
        if n == 0 {
            continue;
        }
        let exit_node = n;
        let total_nodes = n + 1;

        let sid_to_idx: HashMap<&str, usize> = f
            .body
            .iter()
            .enumerate()
            .map(|(i, s)| (s.sid.as_str(), i))
            .collect();

        let mut tau_adj: Vec<Vec<usize>> = vec![Vec::new(); total_nodes];
        let mut obs_adj: Vec<Vec<(String, String, usize)>> = vec![Vec::new(); total_nodes];

        for (i, stmt) in f.body.iter().enumerate() {
            let succs: Vec<usize> = if stmt.is_return() {
                vec![exit_node]
            } else {
                let raw = f.successors(i);
                if raw.is_empty() {
                    vec![exit_node]
                } else {
                    raw.into_iter()
                        .filter_map(|t| sid_to_idx.get(t).copied())
                        .collect()
                }
            };

            if let Some((op_name, res_name)) = sync_label(program, module, &stmt.op) {
                for next in succs {
                    obs_adj[i].push((op_name.clone(), res_name.clone(), next));
                }
            } else {
                for next in succs {
                    tau_adj[i].push(next);
                }
            }
        }

        // Compute tau*-closure for each node.
        let mut tau_closure: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); total_nodes];
        for start in 0..total_nodes {
            let mut visited = BTreeSet::new();
            let mut q = VecDeque::new();
            visited.insert(start);
            q.push_back(start);
            while let Some(u) = q.pop_front() {
                for &v in &tau_adj[u] {
                    if visited.insert(v) {
                        q.push_back(v);
                    }
                }
            }
            tau_closure[start] = visited;
        }

        // Build weak observable transition graph reachable from entry (node 0).
        let mut weak_edges: Vec<BTreeSet<(String, String, usize)>> =
            vec![BTreeSet::new(); total_nodes];
        let mut accepting = vec![false; total_nodes];
        for u in 0..total_nodes {
            accepting[u] = tau_closure[u].contains(&exit_node);
            for &v in &tau_closure[u] {
                for (op, res, w) in &obs_adj[v] {
                    weak_edges[u].insert((op.clone(), res.clone(), *w));
                }
            }
        }

        // Restrict to nodes reachable from entry (0) via weak transitions.
        let mut reachable = BTreeSet::new();
        let mut q = VecDeque::new();
        reachable.insert(0usize);
        q.push_back(0usize);
        while let Some(u) = q.pop_front() {
            for (_, _, w) in &weak_edges[u] {
                if reachable.insert(*w) {
                    q.push_back(*w);
                }
            }
        }

        let reach_list: Vec<usize> = reachable.into_iter().collect();
        // Partition refinement (stuttering bisimulation quotient).
        let mut block_of: HashMap<usize, usize> = HashMap::new();
        for &u in &reach_list {
            block_of.insert(u, if accepting[u] { 1 } else { 0 });
        }

        loop {
            let mut sig_to_block: BTreeMap<(bool, BTreeSet<(String, String, usize)>), usize> =
                BTreeMap::new();
            let mut new_block_of: HashMap<usize, usize> = HashMap::new();
            for &u in &reach_list {
                let trans_sig: BTreeSet<(String, String, usize)> = weak_edges[u]
                    .iter()
                    .map(|(op, res, w)| (op.clone(), res.clone(), block_of[w]))
                    .collect();
                let key = (accepting[u], trans_sig);
                let next_id = sig_to_block.len();
                let bid = *sig_to_block.entry(key).or_insert(next_id);
                new_block_of.insert(u, bid);
            }
            if new_block_of == block_of {
                break;
            }
            block_of = new_block_of;
        }

        // Number quotient blocks deterministically starting from the entry block.
        let mut block_remap: BTreeMap<usize, usize> = BTreeMap::new();
        let entry_old_block = block_of[&0];
        block_remap.insert(entry_old_block, next_state_id);
        next_state_id += 1;
        for &u in &reach_list {
            let b = block_of[&u];
            if !block_remap.contains_key(&b) {
                block_remap.insert(b, next_state_id);
                next_state_id += 1;
            }
        }

        let mut emitted_states: BTreeSet<usize> = BTreeSet::new();
        for &u in &reach_list {
            let qid = block_remap[&block_of[&u]];
            if emitted_states.insert(qid) {
                all_states.push(QuotientState {
                    id: qid,
                    function: f.name.clone(),
                    entry: qid == block_remap[&entry_old_block],
                    accepting: accepting[u],
                });
            }
        }

        let mut fn_trans: BTreeSet<QuotientTransition> = BTreeSet::new();
        for &u in &reach_list {
            let from_q = block_remap[&block_of[&u]];
            for (op, res, w) in &weak_edges[u] {
                let to_q = block_remap[&block_of[w]];
                fn_trans.insert(QuotientTransition {
                    from: from_q,
                    to: to_q,
                    op: op.clone(),
                    resource: res.clone(),
                    function: f.name.clone(),
                });
            }
        }
        all_transitions.extend(fn_trans);
    }

    (all_states, all_transitions)
}
