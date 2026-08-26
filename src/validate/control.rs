use std::collections::{HashMap, HashSet, VecDeque};

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::validate::types::{build_resource_type_map, ResType};

/// E6xx: Control flow checks.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let rt_map = build_resource_type_map(program);

    for (mi, m) in program.modules.iter().enumerate() {
        for (fi, f) in m.functions.iter().enumerate() {
            if f.body.is_empty() {
                continue;
            }

            let sid_to_idx: HashMap<&str, usize> = f
                .body
                .iter()
                .enumerate()
                .map(|(i, s)| (s.sid.as_str(), i))
                .collect();

            let n = f.body.len();
            let mut successors = vec![Vec::new(); n];

            for (i, block) in f.body.iter().enumerate() {
                for t in block.successor_sids() {
                    if let Some(&ti) = sid_to_idx.get(t) {
                        successors[i].push(ti);
                    }
                }
            }

            let fn_path = Program::fn_path(mi, fi);
            check_reachability(f, &successors, n, &fn_path, diags);
            check_return_paths(f, &successors, n, &fn_path, diags);
            check_branch_targets_same(f, &fn_path, diags);
            check_switch_exhaustive(f, &rt_map, &fn_path, diags);
            check_infinite_loop(f, &successors, n, &fn_path, diags);
        }
    }
}

/// E601: unreachable statements
fn check_reachability(
    f: &Function,
    successors: &[Vec<usize>],
    n: usize,
    fn_path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let mut reachable = vec![false; n];
    let mut queue = VecDeque::new();
    reachable[0] = true;
    queue.push_back(0);

    while let Some(idx) = queue.pop_front() {
        for &succ in &successors[idx] {
            if !reachable[succ] {
                reachable[succ] = true;
                queue.push_back(succ);
            }
        }
    }

    for (i, is_reachable) in reachable.iter().enumerate() {
        if !is_reachable {
            diags.push(
                Diagnostic::warning(
                    "E601",
                    format!(
                        "unreachable statement '{}' in function '{}'",
                        f.body[i].sid, f.name
                    ),
                )
                .with_path(format!("{fn_path}.body[{i}]"))
                .with_fix("remove the statement or fix control flow to reach it"),
            );
        }
    }
}

/// E602: missing return — every path must end with a return
fn check_return_paths(
    f: &Function,
    successors: &[Vec<usize>],
    n: usize,
    fn_path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    for (i, succs) in successors.iter().enumerate().take(n) {
        let stmt = &f.body[i];
        let is_return = stmt.is_return();
        let has_no_successors = succs.is_empty();

        if has_no_successors && !is_return {
            diags.push(
                Diagnostic::error(
                    "E602",
                    format!(
                        "function '{}' has a path ending at '{}' without return",
                        f.name, stmt.sid
                    ),
                )
                .with_path(format!("{fn_path}.body[{i}]"))
                .with_fix("add a return statement at the end of this path"),
            );
        }
    }
}

/// E603: branch with same true/false targets
fn check_branch_targets_same(f: &Function, fn_path: &str, diags: &mut Vec<Diagnostic>) {
    for (si, block) in f.body.iter().enumerate() {
        if let Some(Terminator::Branch {
            then, else_target, ..
        }) = &block.terminator
        {
            if then == else_target {
                diags.push(
                    Diagnostic::warning(
                        "E603",
                        format!(
                            "branch at '{}' has identical then/else targets '{then}'",
                            block.sid
                        ),
                    )
                    .with_path(format!("{fn_path}.body[{si}].terminator"))
                    .with_fix("use goto instead, or correct the branch targets"),
                );
            }
        }
    }
}

/// E604: switch not exhaustive for Enum types
fn check_switch_exhaustive(
    f: &Function,
    rt_map: &HashMap<String, ResType>,
    fn_path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    for (si, block) in f.body.iter().enumerate() {
        if let Some((var, cases, _)) = block.switch() {
            if let Some(rt) = rt_map.get(var) {
                let bt = crate::validate::types::res_type_to_base(rt);
                if let Some(BaseType::Complex(ComplexBaseType::Enum(ref variants))) = bt {
                    let covered: HashSet<&str> = cases.keys().map(String::as_str).collect();

                    let missing: Vec<&str> = variants
                        .iter()
                        .filter(|v| !covered.contains(v.as_str()))
                        .map(|v| v.as_str())
                        .collect();

                    if !missing.is_empty() {
                        diags.push(
                            Diagnostic::error(
                                "E604",
                                format!(
                                    "switch on '{var}' is not exhaustive; missing variants: [{}]",
                                    missing.join(", ")
                                ),
                            )
                            .with_path(format!("{fn_path}.body[{si}].terminator.cases"))
                            .with_fix("add case branches for the missing variants"),
                        );
                    }
                }
            }
        }
    }
}

/// E605: infinite loop with no exit and no blocking ops
fn check_infinite_loop(
    f: &Function,
    successors: &[Vec<usize>],
    n: usize,
    fn_path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let sccs = tarjan_scc(successors, n);

    for scc in &sccs {
        if scc.len() < 2 {
            let idx = scc[0];
            if !successors[idx].contains(&idx) {
                continue;
            }
        }

        let scc_set: HashSet<usize> = scc.iter().copied().collect();

        let has_exit = scc.iter().any(|&idx| {
            successors[idx].iter().any(|s| !scc_set.contains(s)) || f.body[idx].is_return()
        });

        if has_exit {
            continue;
        }

        let has_blocking = scc.iter().any(|&idx| {
            let block = &f.body[idx];
            matches!(
                &block.call,
                Some(
                    Call::Await { .. }
                        | Call::Join { .. }
                        | Call::JoinAll { .. }
                        | Call::ChannelRecv { .. }
                        | Call::SemaphoreAcquire { .. }
                        | Call::CondvarWait { .. }
                        | Call::Select { .. }
                )
            )
        });

        if !has_blocking {
            let first = scc[0];
            diags.push(
                Diagnostic::warning(
                    "E605",
                    format!(
                        "potential infinite loop with no exit in function '{}' starting at '{}'",
                        f.name, f.body[first].sid
                    ),
                )
                .with_path(format!("{fn_path}.body[{first}]"))
                .with_fix("add an exit condition or confirm this is an intentional event loop"),
            );
        }
    }
}

/// Tarjan's SCC algorithm
fn tarjan_scc(successors: &[Vec<usize>], n: usize) -> Vec<Vec<usize>> {
    struct State {
        index_counter: usize,
        stack: Vec<usize>,
        on_stack: Vec<bool>,
        index: Vec<Option<usize>>,
        lowlink: Vec<usize>,
        sccs: Vec<Vec<usize>>,
    }

    let mut state = State {
        index_counter: 0,
        stack: Vec::new(),
        on_stack: vec![false; n],
        index: vec![None; n],
        lowlink: vec![0; n],
        sccs: Vec::new(),
    };

    fn strongconnect(v: usize, successors: &[Vec<usize>], s: &mut State) {
        s.index[v] = Some(s.index_counter);
        s.lowlink[v] = s.index_counter;
        s.index_counter += 1;
        s.stack.push(v);
        s.on_stack[v] = true;

        for &w in &successors[v] {
            if s.index[w].is_none() {
                strongconnect(w, successors, s);
                s.lowlink[v] = s.lowlink[v].min(s.lowlink[w]);
            } else if s.on_stack[w] {
                s.lowlink[v] = s.lowlink[v].min(s.index[w].unwrap());
            }
        }

        if s.lowlink[v] == s.index[v].unwrap() {
            let mut scc = Vec::new();
            loop {
                let w = s.stack.pop().unwrap();
                s.on_stack[w] = false;
                scc.push(w);
                if w == v {
                    break;
                }
            }
            s.sccs.push(scc);
        }
    }

    for v in 0..n {
        if state.index[v].is_none() {
            strongconnect(v, successors, &mut state);
        }
    }

    state.sccs
}
