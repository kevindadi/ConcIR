use std::collections::{HashMap, HashSet, VecDeque};

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::validate::types::{build_resource_type_map, ResType};

/// E6xx: Control flow checks.
pub fn check(program: &Program, source: &str, diags: &mut Vec<Diagnostic>) {
    let rt_map = build_resource_type_map(program);

    for f in &program.functions {
        if f.statements.is_empty() {
            continue;
        }

        let sid_to_idx: HashMap<&str, usize> = f
            .statements
            .iter()
            .enumerate()
            .map(|(i, s)| (s.sid.value.as_str(), i))
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
                    if let Some(&ti) = sid_to_idx.get(t.value.as_str()) {
                        successors[i].push(ti);
                    }
                    if let Some(&fi) = sid_to_idx.get(fl.value.as_str()) {
                        successors[i].push(fi);
                    }
                }
                Transfer::Switch(_, cases) => {
                    for c in cases {
                        if let Some(&ci) = sid_to_idx.get(c.target.value.as_str()) {
                            successors[i].push(ci);
                        }
                    }
                }
                Transfer::Return => {}
            }
        }

        check_reachability(f, &successors, n, source, diags);
        check_return_paths(f, &successors, n, source, diags);
        check_branch_targets_same(f, source, diags);
        check_switch_exhaustive(f, &rt_map, source, diags);
        check_infinite_loop(f, &successors, n, source, diags);
    }
}

/// E601: unreachable statements
fn check_reachability(
    f: &Function,
    successors: &[Vec<usize>],
    n: usize,
    source: &str,
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
                        f.statements[i].sid.value, f.name.value
                    ),
                )
                .with_span(f.statements[i].span, source)
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
    source: &str,
    diags: &mut Vec<Diagnostic>,
) {
    // A statement is a "sink" if it has no successors in the CFG
    for i in 0..n {
        let stmt = &f.statements[i];
        let is_return =
            matches!(stmt.op, Op::Return) || matches!(stmt.transfer, Transfer::Return);
        let has_no_successors = successors[i].is_empty();

        if has_no_successors && !is_return {
            diags.push(
                Diagnostic::error(
                    "E602",
                    format!(
                        "function '{}' has a path ending at '{}' without return",
                        f.name.value, stmt.sid.value
                    ),
                )
                .with_span(stmt.span, source)
                .with_fix("add 'return => return;' at the end of this path"),
            );
        }
    }
}

/// E603: branch with same true/false targets
fn check_branch_targets_same(f: &Function, source: &str, diags: &mut Vec<Diagnostic>) {
    for stmt in &f.statements {
        if let Transfer::Branch(_, ref t, ref fl) = stmt.transfer {
            if t.value == fl.value {
                diags.push(
                    Diagnostic::warning(
                        "E603",
                        format!(
                            "branch at '{}' has identical true/false targets '{}'",
                            stmt.sid.value, t.value
                        ),
                    )
                    .with_span(stmt.span, source)
                    .with_fix("use 'next' instead, or correct the branch targets"),
                );
            }
        }
    }
}

/// E604: switch not exhaustive for Enum types
fn check_switch_exhaustive(
    f: &Function,
    rt_map: &HashMap<String, ResType>,
    source: &str,
    diags: &mut Vec<Diagnostic>,
) {
    for stmt in &f.statements {
        if let Transfer::Switch(ref var, ref cases) = stmt.transfer {
            if let Some(ResType::Var(BaseType::Enum(ref variants)))
            | Some(ResType::Atomic(BaseType::Enum(ref variants))) = rt_map.get(&var.value)
            {
                let covered: HashSet<&str> = cases
                    .iter()
                    .filter_map(|c| {
                        if let Literal::Ident(ref s) = c.label {
                            Some(s.as_str())
                        } else {
                            None
                        }
                    })
                    .collect();

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
                                "switch on '{}' is not exhaustive; missing variants: [{}]",
                                var.value,
                                missing.join(", ")
                            ),
                        )
                        .with_span(stmt.span, source)
                        .with_fix("add case branches for the missing variants"),
                    );
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
    source: &str,
    diags: &mut Vec<Diagnostic>,
) {
    // Find SCCs with no exit edge and no blocking operation
    let sccs = tarjan_scc(successors, n);

    for scc in &sccs {
        if scc.len() < 2 {
            // Single node self-loop check
            let idx = scc[0];
            if !successors[idx].contains(&idx) {
                continue;
            }
        }

        let scc_set: HashSet<usize> = scc.iter().copied().collect();

        let has_exit = scc.iter().any(|&idx| {
            successors[idx].iter().any(|s| !scc_set.contains(s))
                || matches!(f.statements[idx].transfer, Transfer::Return)
                || matches!(f.statements[idx].op, Op::Return)
        });

        if has_exit {
            continue;
        }

        let has_blocking = scc.iter().any(|&idx| {
            let stmt = &f.statements[idx];
            matches!(
                stmt.op,
                Op::Await(_)
                    | Op::Join(_)
                    | Op::ResOp(_, Action::Recv)
                    | Op::ResOp(_, Action::Acquire)
                    | Op::ResOp(_, Action::Wait(_))
            )
        });

        if !has_blocking {
            let first = scc[0];
            diags.push(
                Diagnostic::warning(
                    "E605",
                    format!(
                        "potential infinite loop with no exit in function '{}' starting at '{}'",
                        f.name.value, f.statements[first].sid.value
                    ),
                )
                .with_span(f.statements[first].span, source)
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
