use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diagnostic::Diagnostic;

/// E1xx: Name resolution checks.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let resource_names = check_duplicate_resources(program, diags);
    let function_names = check_duplicate_functions(program, diags);
    check_duplicate_sids(program, diags);
    check_resource_references(program, diags, &resource_names);
    check_function_references(program, diags, &function_names);
    check_sid_references(program, diags);
    check_entry(program, diags, &function_names);
}

fn check_duplicate_resources(program: &Program, diags: &mut Vec<Diagnostic>) -> HashSet<String> {
    let mut seen = HashMap::new();
    for (i, r) in program.resources.iter().enumerate() {
        if let Some(&first_idx) = seen.get(&r.name) {
            diags.push(
                Diagnostic::error("E104", format!("duplicate resource name '{}'", r.name))
                    .with_path(format!("resources[{i}].name"))
                    .with_fix("remove the duplicate or rename one of them"),
            );
            let _ = first_idx;
        } else {
            seen.insert(r.name.clone(), i);
        }
    }
    seen.into_keys().collect()
}

fn check_duplicate_functions(program: &Program, diags: &mut Vec<Diagnostic>) -> HashSet<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (i, f) in program.functions.iter().enumerate() {
        if seen.contains_key(&f.name) {
            diags.push(
                Diagnostic::error("E105", format!("duplicate function name '{}'", f.name))
                    .with_path(format!("functions[{i}].name"))
                    .with_fix("rename one of the functions"),
            );
        } else {
            seen.insert(f.name.clone(), i);
        }
    }
    seen.into_keys().collect()
}

fn check_duplicate_sids(program: &Program, diags: &mut Vec<Diagnostic>) {
    for (fi, f) in program.functions.iter().enumerate() {
        let mut seen = HashSet::new();
        for (si, stmt) in f.body.iter().enumerate() {
            if !seen.insert(stmt.sid.clone()) {
                diags.push(
                    Diagnostic::error(
                        "E106",
                        format!(
                            "duplicate statement id '{}' in function '{}'",
                            stmt.sid, f.name
                        ),
                    )
                    .with_path(format!("functions[{fi}].body[{si}].sid"))
                    .with_fix("assign a unique statement id"),
                );
            }
        }
    }
}

fn check_resource_references(
    program: &Program,
    diags: &mut Vec<Diagnostic>,
    resources: &HashSet<String>,
) {
    for (fi, f) in program.functions.iter().enumerate() {
        for (si, stmt) in f.body.iter().enumerate() {
            if let Op::ResOp {
                ref resource,
                ref action,
                ref args,
            } = stmt.op
            {
                if !resources.contains(resource) {
                    diags.push(
                        Diagnostic::error(
                            "E101",
                            format!("undefined resource '{resource}' referenced in res_op"),
                        )
                        .with_path(format!("functions[{fi}].body[{si}].op[1]"))
                        .with_fix("add this resource to the resources block"),
                    );
                }
                // Check wait lock reference
                if action == "wait" {
                    if let Some(lock_name) = args.first() {
                        if !resources.contains(lock_name) {
                            diags.push(
                                Diagnostic::error(
                                    "E101",
                                    format!(
                                        "undefined resource '{lock_name}' referenced in wait()"
                                    ),
                                )
                                .with_path(format!("functions[{fi}].body[{si}].op[3]"))
                                .with_fix("add this resource to the resources block"),
                            );
                        }
                    }
                }
            }
        }
    }
}

fn check_function_references(
    program: &Program,
    diags: &mut Vec<Diagnostic>,
    functions: &HashSet<String>,
) {
    for (fi, f) in program.functions.iter().enumerate() {
        for (si, stmt) in f.body.iter().enumerate() {
            if let Some(target) = stmt.op.target_name() {
                if !functions.contains(target) {
                    diags.push(
                        Diagnostic::error(
                            "E102",
                            format!("undefined function '{target}' referenced"),
                        )
                        .with_path(format!("functions[{fi}].body[{si}].op[1]"))
                        .with_fix("add a fn definition for this function"),
                    );
                }
            }
        }
    }
}

fn check_sid_references(program: &Program, diags: &mut Vec<Diagnostic>) {
    for (fi, f) in program.functions.iter().enumerate() {
        let sids: HashSet<&str> = f.body.iter().map(|s| s.sid.as_str()).collect();
        for (si, stmt) in f.body.iter().enumerate() {
            let targets = stmt.transfer.target_sids();
            for t in targets {
                if !sids.contains(t) {
                    diags.push(
                        Diagnostic::error(
                            "E103",
                            format!("undefined statement id '{t}' in function '{}'", f.name),
                        )
                        .with_path(format!("functions[{fi}].body[{si}].transfer"))
                        .with_fix("use an existing statement id from this function"),
                    );
                }
            }
        }
    }
}

fn check_entry(program: &Program, diags: &mut Vec<Diagnostic>, functions: &HashSet<String>) {
    if !functions.contains(&program.entry) {
        diags.push(
            Diagnostic::error(
                "E107",
                format!("entry function '{}' is not defined", program.entry),
            )
            .with_path("entry".to_string())
            .with_fix("change entry to the name of a defined function"),
        );
    }
}
