use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diagnostic::Diagnostic;

/// E1xx: Name resolution checks.
pub fn check(program: &Program, source: &str, diags: &mut Vec<Diagnostic>) {
    let resource_names = check_duplicate_resources(program, source, diags);
    let function_names = check_duplicate_functions(program, source, diags);
    check_duplicate_sids(program, source, diags);
    check_resource_references(program, source, diags, &resource_names);
    check_function_references(program, source, diags, &function_names);
    check_sid_references(program, source, diags);
    check_entry(program, source, diags, &function_names);
}

fn check_duplicate_resources(
    program: &Program,
    source: &str,
    diags: &mut Vec<Diagnostic>,
) -> HashSet<String> {
    let mut seen = HashMap::new();
    for r in &program.resources {
        let name = &r.name.value;
        if let Some(&first_span) = seen.get(name) {
            diags.push(
                Diagnostic::error("E104", format!("duplicate resource name '{name}'"))
                    .with_span(r.name.span, source)
                    .with_fix("remove the duplicate or rename one of them"),
            );
            let _ = first_span; // first occurrence recorded
        } else {
            seen.insert(name.clone(), r.name.span);
        }
    }
    seen.into_keys().collect()
}

fn check_duplicate_functions(
    program: &Program,
    source: &str,
    diags: &mut Vec<Diagnostic>,
) -> HashSet<String> {
    let mut seen = HashMap::new();
    for f in &program.functions {
        let name = &f.name.value;
        if seen.contains_key(name) {
            diags.push(
                Diagnostic::error("E105", format!("duplicate function name '{name}'"))
                    .with_span(f.name.span, source)
                    .with_fix("rename one of the functions"),
            );
        } else {
            seen.insert(name.clone(), f.name.span);
        }
    }
    for s in &program.fn_summaries {
        seen.entry(s.name.value.clone()).or_insert(s.name.span);
    }
    seen.into_keys().collect()
}

fn check_duplicate_sids(program: &Program, source: &str, diags: &mut Vec<Diagnostic>) {
    for f in &program.functions {
        let mut seen = HashSet::new();
        for stmt in &f.statements {
            if !seen.insert(stmt.sid.value.clone()) {
                diags.push(
                    Diagnostic::error(
                        "E106",
                        format!(
                            "duplicate statement id '{}' in function '{}'",
                            stmt.sid.value, f.name.value
                        ),
                    )
                    .with_span(stmt.sid.span, source)
                    .with_fix("assign a unique statement id"),
                );
            }
        }
    }
}

fn check_resource_references(
    program: &Program,
    source: &str,
    diags: &mut Vec<Diagnostic>,
    resources: &HashSet<String>,
) {
    for f in &program.functions {
        for stmt in &f.statements {
            if let Op::ResOp(ref res, _) = stmt.op {
                if !resources.contains(&res.value) {
                    diags.push(
                        Diagnostic::error(
                            "E101",
                            format!("undefined resource '{}' referenced in res_op", res.value),
                        )
                        .with_span(res.span, source)
                        .with_fix("add this resource to the resources block"),
                    );
                }
            }
            // Check condvar wait lock reference
            if let Op::ResOp(_, Action::Wait(ref lock)) = stmt.op {
                if !resources.contains(&lock.value) {
                    diags.push(
                        Diagnostic::error(
                            "E101",
                            format!(
                                "undefined resource '{}' referenced in wait()",
                                lock.value
                            ),
                        )
                        .with_span(lock.span, source)
                        .with_fix("add this resource to the resources block"),
                    );
                }
            }
        }
    }
}

fn check_function_references(
    program: &Program,
    source: &str,
    diags: &mut Vec<Diagnostic>,
    functions: &HashSet<String>,
) {
    for f in &program.functions {
        for stmt in &f.statements {
            let target = match &stmt.op {
                Op::Spawn(t) | Op::SpawnAsync(t) | Op::Join(t) | Op::Await(t) | Op::Call(t) => {
                    Some(t)
                }
                _ => None,
            };
            if let Some(t) = target {
                if !functions.contains(&t.value) {
                    diags.push(
                        Diagnostic::error(
                            "E102",
                            format!("undefined function '{}' referenced", t.value),
                        )
                        .with_span(t.span, source)
                        .with_fix("add a fn definition or fn_summary for this function"),
                    );
                }
            }
        }
    }
}

fn check_sid_references(program: &Program, source: &str, diags: &mut Vec<Diagnostic>) {
    for f in &program.functions {
        let sids: HashSet<&str> = f.statements.iter().map(|s| s.sid.value.as_str()).collect();
        for stmt in &f.statements {
            let targets: Vec<&Spanned<String>> = match &stmt.transfer {
                Transfer::Branch(_, t, f) => vec![t, f],
                Transfer::Switch(_, cases) => cases.iter().map(|c| &c.target).collect(),
                _ => vec![],
            };
            for t in targets {
                if !sids.contains(t.value.as_str()) {
                    diags.push(
                        Diagnostic::error(
                            "E103",
                            format!(
                                "undefined statement id '{}' in function '{}'",
                                t.value, f.name.value
                            ),
                        )
                        .with_span(t.span, source)
                        .with_fix("use an existing statement id from this function"),
                    );
                }
            }
        }
    }
}

fn check_entry(
    program: &Program,
    source: &str,
    diags: &mut Vec<Diagnostic>,
    functions: &HashSet<String>,
) {
    if !functions.contains(&program.entry.value) {
        diags.push(
            Diagnostic::error(
                "E107",
                format!("entry function '{}' is not defined", program.entry.value),
            )
            .with_span(program.entry.span, source)
            .with_fix("change entry to the name of a defined function"),
        );
    }
}
