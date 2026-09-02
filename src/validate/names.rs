use std::collections::HashSet;

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::fqn::{self, is_fqn, split_fqn};

/// E1xx: Name resolution checks.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    check_duplicate_modules(program, diags);
    let resource_fqns = check_duplicate_resources(program, diags);
    let function_fqns = check_duplicate_functions(program, diags);
    let type_fqns = collect_type_fqns(program);
    check_duplicate_sids(program, diags);
    check_contracts(program, diags, &resource_fqns, &function_fqns, &type_fqns);
    check_resource_references(program, diags);
    check_function_references(program, diags);
    check_sid_references(program, diags);
    check_entry(program, diags, &function_fqns);
}

fn check_duplicate_modules(program: &Program, diags: &mut Vec<Diagnostic>) {
    let mut seen = HashSet::new();
    for (i, m) in program.modules.iter().enumerate() {
        if !seen.insert(m.name.clone()) {
            diags.push(
                Diagnostic::error("E108", format!("duplicate module name '{}'", m.name))
                    .with_path(format!("modules[{i}].name"))
                    .with_fix("rename one of the modules"),
            );
        }
    }
}

fn check_duplicate_resources(program: &Program, diags: &mut Vec<Diagnostic>) -> HashSet<String> {
    let mut fqns = HashSet::new();
    for (mi, m) in program.modules.iter().enumerate() {
        let mut seen = HashSet::new();
        for (i, r) in m.resources.iter().enumerate() {
            if !seen.insert(r.name.clone()) {
                diags.push(
                    Diagnostic::error("E104", format!("duplicate resource name '{}'", r.name))
                        .with_path(format!("modules[{mi}].resources[{i}].name"))
                        .with_fix("remove the duplicate or rename one of them"),
                );
            }
            fqns.insert(fqn::fqn(&m.name, &r.name));
        }
    }
    fqns
}

fn check_duplicate_functions(program: &Program, diags: &mut Vec<Diagnostic>) -> HashSet<String> {
    let mut fqns = HashSet::new();
    for (mi, m) in program.modules.iter().enumerate() {
        let mut seen = HashSet::new();
        for (i, f) in m.functions.iter().enumerate() {
            if !seen.insert(f.name.clone()) {
                diags.push(
                    Diagnostic::error("E105", format!("duplicate function name '{}'", f.name))
                        .with_path(format!("modules[{mi}].functions[{i}].name"))
                        .with_fix("rename one of the functions"),
                );
            }
            fqns.insert(fqn::fqn(&m.name, &f.name));
        }
    }
    fqns
}

fn check_duplicate_sids(program: &Program, diags: &mut Vec<Diagnostic>) {
    for (mi, m) in program.modules.iter().enumerate() {
        for (fi, f) in m.functions.iter().enumerate() {
            let mut seen = HashSet::new();
            let mut hole_ids = HashSet::new();
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
                        .with_path(format!("{}.body[{si}].sid", Program::fn_path(mi, fi)))
                        .with_fix("assign a unique statement id"),
                    );
                }
                if let Op::SeqHole { id, .. } = &stmt.op {
                    if !hole_ids.insert(id.clone()) {
                        diags.push(
                            Diagnostic::error(
                                "E109",
                                format!("duplicate seq_hole id '{id}' in function '{}'", f.name),
                            )
                            .with_path(format!("{}.body[{si}].id", Program::fn_path(mi, fi)))
                            .with_fix("give each seq_hole a unique id within the function"),
                        );
                    }
                }
            }
        }
    }
}

fn collect_type_fqns(program: &Program) -> HashSet<String> {
    program
        .modules
        .iter()
        .flat_map(|m| m.types.iter().map(|t| fqn::fqn(&m.name, &t.name)))
        .collect()
}

fn check_contracts(
    program: &Program,
    diags: &mut Vec<Diagnostic>,
    resource_fqns: &HashSet<String>,
    function_fqns: &HashSet<String>,
    type_fqns: &HashSet<String>,
) {
    let provided_res: HashSet<String> = program
        .modules
        .iter()
        .flat_map(|m| m.provides.resources.iter().map(|n| fqn::fqn(&m.name, n)))
        .collect();
    let provided_fn: HashSet<String> = program
        .modules
        .iter()
        .flat_map(|m| m.provides.functions.iter().map(|n| fqn::fqn(&m.name, n)))
        .collect();
    let provided_ty: HashSet<String> = program
        .modules
        .iter()
        .flat_map(|m| m.provides.types.iter().map(|n| fqn::fqn(&m.name, n)))
        .collect();

    for (mi, m) in program.modules.iter().enumerate() {
        let local_res: HashSet<&str> = m.resources.iter().map(|r| r.name.as_str()).collect();
        let local_fn: HashSet<&str> = m.functions.iter().map(|f| f.name.as_str()).collect();
        let local_ty: HashSet<&str> = m.types.iter().map(|t| t.name.as_str()).collect();
        for (i, name) in m.provides.resources.iter().enumerate() {
            if !local_res.contains(name.as_str()) {
                diags.push(
                    Diagnostic::error(
                        "E108",
                        format!(
                            "module '{}' provides resource '{name}' which it does not declare",
                            m.name
                        ),
                    )
                    .with_path(format!("modules[{mi}].provides.resources[{i}]"))
                    .with_fix("declare the resource in this module or remove it from provides"),
                );
            }
        }
        for (i, name) in m.provides.types.iter().enumerate() {
            if !local_ty.contains(name.as_str()) {
                diags.push(
                    Diagnostic::error(
                        "E108",
                        format!(
                            "module '{}' provides type '{name}' which it does not declare",
                            m.name
                        ),
                    )
                    .with_path(format!("modules[{mi}].provides.types[{i}]"))
                    .with_fix("declare the type in this module or remove it from provides"),
                );
            }
        }
        for (i, name) in m.provides.functions.iter().enumerate() {
            if !local_fn.contains(name.as_str()) {
                diags.push(
                    Diagnostic::error(
                        "E108",
                        format!(
                            "module '{}' provides function '{name}' which it does not declare",
                            m.name
                        ),
                    )
                    .with_path(format!("modules[{mi}].provides.functions[{i}]"))
                    .with_fix("declare the function in this module or remove it from provides"),
                );
            }
        }
        for (i, name) in m.requires.resources.iter().enumerate() {
            if !is_fqn(name) {
                diags.push(
                    Diagnostic::error(
                        "E108",
                        format!("requires resource '{name}' must be an FQN (module::entity)"),
                    )
                    .with_path(format!("modules[{mi}].requires.resources[{i}]"))
                    .with_fix("write the resource as module::name"),
                );
                continue;
            }
            if !resource_fqns.contains(name) || !provided_res.contains(name) {
                diags.push(
                    Diagnostic::error("E108", format!("unresolved import of resource '{name}'"))
                        .with_path(format!("modules[{mi}].requires.resources[{i}]"))
                        .with_fix("export it from the owning module's provides.resources"),
                );
            }
        }
        for (i, req) in m.requires.functions.iter().enumerate() {
            let name = req.name();
            if !is_fqn(name) {
                diags.push(
                    Diagnostic::error(
                        "E108",
                        format!("requires function '{name}' must be an FQN (module::entity)"),
                    )
                    .with_path(format!("modules[{mi}].requires.functions[{i}]"))
                    .with_fix("write the function as module::name"),
                );
                continue;
            }
            if !function_fqns.contains(name) || !provided_fn.contains(name) {
                diags.push(
                    Diagnostic::error("E108", format!("unresolved import of function '{name}'"))
                        .with_path(format!("modules[{mi}].requires.functions[{i}]"))
                        .with_fix("export it from the owning module's provides.functions"),
                );
            }
        }
        for (i, name) in m.requires.types.iter().enumerate() {
            if !is_fqn(name) {
                diags.push(
                    Diagnostic::error(
                        "E108",
                        format!("requires type '{name}' must be an FQN (module::entity)"),
                    )
                    .with_path(format!("modules[{mi}].requires.types[{i}]"))
                    .with_fix("write the type as module::Name"),
                );
                continue;
            }
            if !type_fqns.contains(name) || !provided_ty.contains(name) {
                diags.push(
                    Diagnostic::error("E108", format!("unresolved import of type '{name}'"))
                        .with_path(format!("modules[{mi}].requires.types[{i}]"))
                        .with_fix("export it from the owning module's provides.types"),
                );
            }
        }
    }
}

fn resource_defined(program: &Program, from_module: &str, name: &str) -> bool {
    program.lookup_resource(from_module, name).is_some()
}

fn check_resource_references(program: &Program, diags: &mut Vec<Diagnostic>) {
    program.walk_stmts(|mi, fi, si, m, _, stmt| {
        let path = Program::stmt_path(mi, fi, si);
        if let Some(resource) = stmt.op.resource_name() {
            if !resource_defined(program, &m.name, resource) {
                diags.push(
                    Diagnostic::error("E101", format!("undefined resource '{resource}'"))
                        .with_path(path.clone())
                        .with_fix("declare the resource or import it via requires"),
                );
            }
        }
        if let Some((reads, writes)) = stmt.op.footprint() {
            for resource in reads.iter().chain(writes.iter()) {
                if !resource_defined(program, &m.name, resource) {
                    diags.push(
                        Diagnostic::error("E101", format!("undefined resource '{resource}'"))
                            .with_path(path.clone())
                            .with_fix("declare the resource or import it via requires"),
                    );
                }
            }
        }
        if let Op::CondvarWait { lock, .. } = &stmt.op {
            if !resource_defined(program, &m.name, lock) {
                diags.push(
                    Diagnostic::error(
                        "E101",
                        format!("undefined resource '{lock}' referenced in condvar_wait"),
                    )
                    .with_path(path.clone())
                    .with_fix("declare this lock resource"),
                );
            }
        }
        if let Op::Select { branches, .. } = &stmt.op {
            for branch in branches {
                if let Some(resource) = branch.guard.resource_name() {
                    if !resource_defined(program, &m.name, resource) {
                        diags.push(
                            Diagnostic::error("E101", format!("undefined resource '{resource}'"))
                                .with_path(path.clone())
                                .with_fix("declare the resource or import it via requires"),
                        );
                    }
                }
                if let SelectGuard::CondvarWait { lock, .. } = &branch.guard {
                    if !resource_defined(program, &m.name, lock) {
                        diags.push(
                            Diagnostic::error(
                                "E101",
                                format!("undefined resource '{lock}' referenced in condvar_wait"),
                            )
                            .with_path(path.clone())
                            .with_fix("declare this lock resource"),
                        );
                    }
                }
            }
        }
    });
}

fn check_function_references(program: &Program, diags: &mut Vec<Diagnostic>) {
    program.walk_stmts(|mi, fi, si, m, _, stmt| {
        for func in stmt.op.callee_funcs() {
            if program.lookup_function(&m.name, func).is_none() {
                diags.push(
                    Diagnostic::error("E102", format!("undefined function '{func}' referenced"))
                        .with_path(Program::stmt_path(mi, fi, si))
                        .with_fix("define the function or import it via requires"),
                );
            }
            if is_fqn(func) {
                let required: HashSet<&str> = m.requires.function_names().into_iter().collect();
                if split_fqn(func).map(|(mod_name, _)| mod_name) != Some(m.name.as_str())
                    && !required.contains(func)
                {
                    diags.push(
                        Diagnostic::error(
                            "E108",
                            format!(
                                "cross-module reference '{func}' is not listed in requires.functions"
                            ),
                        )
                        .with_path(Program::stmt_path(mi, fi, si))
                        .with_fix("add this FQN to the module's requires.functions"),
                    );
                }
            }
        }
    });
}

fn check_sid_references(program: &Program, diags: &mut Vec<Diagnostic>) {
    for (mi, m) in program.modules.iter().enumerate() {
        for (fi, f) in m.functions.iter().enumerate() {
            let sids: HashSet<&str> = f.body.iter().map(|s| s.sid.as_str()).collect();
            for (si, _) in f.body.iter().enumerate() {
                for t in f.successors(si) {
                    if !sids.contains(t) {
                        diags.push(
                            Diagnostic::error(
                                "E103",
                                format!("undefined statement id '{t}' in function '{}'", f.name),
                            )
                            .with_path(Program::stmt_path(mi, fi, si))
                            .with_fix("use an existing statement id from this function"),
                        );
                    }
                }
            }
        }
    }
}

fn check_entry(program: &Program, diags: &mut Vec<Diagnostic>, function_fqns: &HashSet<String>) {
    if !is_fqn(&program.entry) {
        diags.push(
            Diagnostic::error(
                "E107",
                format!(
                    "entry '{}' is not an FQN; write module::function",
                    program.entry
                ),
            )
            .with_path("entry".to_string())
            .with_fix("use an FQN such as core::main"),
        );
        return;
    }
    if !function_fqns.contains(&program.entry) {
        diags.push(
            Diagnostic::error(
                "E107",
                format!("entry function '{}' is not defined", program.entry),
            )
            .with_path("entry".to_string())
            .with_fix("change entry to a defined function FQN"),
        );
    }
}
