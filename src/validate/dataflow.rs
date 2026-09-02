use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::env::NameEnv;
use crate::fqn;

/// E9xx: name environment, unified dst, call vs concurrent-entry data flow.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let mut resource_names: HashSet<String> = HashSet::new();
    let mut callees: HashMap<String, &Function> = HashMap::new();

    for m in &program.modules {
        for r in &m.resources {
            resource_names.insert(r.name.clone());
            resource_names.insert(fqn::fqn(&m.name, &r.name));
        }
        for f in &m.functions {
            callees.insert(f.name.clone(), f);
            callees.insert(fqn::fqn(&m.name, &f.name), f);
        }
    }

    let concurrent_entries = concurrent_entry_callees(program);

    for (mi, m) in program.modules.iter().enumerate() {
        for (fi, f) in m.functions.iter().enumerate() {
            let env = NameEnv::build(program, m, f);
            check_param_decls(f, &resource_names, mi, fi, diags);
            check_local_decls(f, &resource_names, mi, fi, diags);
            check_return_decl(f, mi, fi, diags);
            check_unmodeled_refs(f, mi, fi, diags);
            check_destinations(f, &env, mi, fi, diags);
            check_call_sites(f, &callees, &env, mi, fi, diags);
            check_concurrent_sites(f, &callees, mi, fi, diags);
            if concurrent_entries.contains(&fqn::fqn(&m.name, &f.name)) {
                check_modeled_activation_on_entry(f, mi, fi, diags);
            }
        }
    }
}

fn concurrent_entry_callees(program: &Program) -> HashSet<String> {
    let mut spawned = HashSet::new();
    program.walk_stmts(|_, _, _, m, _, stmt| match &stmt.op {
        Op::Spawn { func, .. } | Op::AsyncCall { func, .. } => {
            if let Some((owner, f)) = program.lookup_function(&m.name, func) {
                spawned.insert(fqn::fqn(&owner.name, &f.name));
            }
        }
        Op::Scope { funcs } => {
            for func in funcs {
                if let Some((owner, f)) = program.lookup_function(&m.name, func) {
                    spawned.insert(fqn::fqn(&owner.name, &f.name));
                }
            }
        }
        _ => {}
    });
    spawned
}

fn check_param_decls(
    f: &Function,
    resource_names: &HashSet<String>,
    mi: usize,
    fi: usize,
    diags: &mut Vec<Diagnostic>,
) {
    let mut seen: HashSet<&str> = HashSet::new();
    for (pi, p) in f.params.iter().enumerate() {
        let path = format!("{}.params[{pi}]", Program::fn_path(mi, fi));
        if resource_names.contains(&p.name) {
            diags.push(
                Diagnostic::error(
                    "E910",
                    format!(
                        "parameter '{}' of function '{}' collides with a resource name",
                        p.name, f.name
                    ),
                )
                .with_path(path.clone())
                .with_fix("rename the parameter"),
            );
        }
        if !seen.insert(p.name.as_str()) {
            diags.push(
                Diagnostic::error(
                    "E911",
                    format!("duplicate parameter '{}' in function '{}'", p.name, f.name),
                )
                .with_path(path.clone())
                .with_fix("assign a unique parameter name"),
            );
        }
    }
}

fn check_local_decls(
    f: &Function,
    resource_names: &HashSet<String>,
    mi: usize,
    fi: usize,
    diags: &mut Vec<Diagnostic>,
) {
    let param_names: HashSet<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    for (li, local) in f.locals.iter().enumerate() {
        let path = format!("{}.locals[{li}]", Program::fn_path(mi, fi));
        if resource_names.contains(&local.name) || param_names.contains(local.name.as_str()) {
            diags.push(
                Diagnostic::error(
                    "E914",
                    format!(
                        "local '{}' of function '{}' collides with a parameter or resource name",
                        local.name, f.name
                    ),
                )
                .with_path(path.clone())
                .with_fix("rename the local"),
            );
        }
        if !seen.insert(local.name.as_str()) {
            diags.push(
                Diagnostic::error(
                    "E915",
                    format!("duplicate local '{}' in function '{}'", local.name, f.name),
                )
                .with_path(path)
                .with_fix("assign a unique local name"),
            );
        }
    }
}

fn check_return_decl(f: &Function, mi: usize, fi: usize, diags: &mut Vec<Diagnostic>) {
    let Some(ret) = &f.returns else {
        return;
    };
    if !ret.modeled {
        return;
    }
    let bare_returns = f
        .body
        .iter()
        .filter(|s| matches!(&s.op, Op::Return { value: None }))
        .count();
    if bare_returns > 0 {
        diags.push(
            Diagnostic::warning(
                "E913",
                format!(
                    "function '{}' declares a modeled return '{}' but {} return statement(s) \
                     carry no value; those paths bind Unknown",
                    f.name, ret.name, bare_returns
                ),
            )
            .with_path(format!("{}.returns", Program::fn_path(mi, fi)))
            .with_fix("give every return statement a value expression"),
        );
    }
}

/// E912: unmodeled activation names used as r-values are legal but Unknown in the net.
fn check_unmodeled_refs(f: &Function, mi: usize, fi: usize, diags: &mut Vec<Diagnostic>) {
    let unmodeled: Vec<&str> = f
        .params
        .iter()
        .filter(|p| !p.modeled)
        .map(|p| p.name.as_str())
        .chain(
            f.locals
                .iter()
                .filter(|l| !l.modeled)
                .map(|l| l.name.as_str()),
        )
        .collect();
    for name in unmodeled {
        if !name_referenced_as_rvalue(f, name) {
            continue;
        }
        diags.push(
            Diagnostic::warning(
                "E912",
                format!(
                    "'{name}' in function '{}' is modeled: false; the value is Unknown in the net",
                    f.name
                ),
            )
            .with_path(Program::fn_path(mi, fi))
            .with_fix(
                "set \"modeled\": true only if a single shared slot is acceptable, \
                 or use a Var for concurrent data; otherwise both branch arms are enabled",
            ),
        );
    }
}

fn check_destinations(
    f: &Function,
    env: &NameEnv,
    mi: usize,
    fi: usize,
    diags: &mut Vec<Diagnostic>,
) {
    for (si, stmt) in f.body.iter().enumerate() {
        let path = Program::stmt_path(mi, fi, si);
        match &stmt.op {
            Op::AssignLocal { target, .. } => check_assign_local_dst(env, target, &path, diags),
            Op::ReadShared { dst: Some(dst), .. } => {
                check_value_or_discard_dst(env, dst, &path, diags);
            }
            Op::AtomicLoad { dst, .. } => check_value_or_discard_dst(env, dst, &path, diags),
            Op::AtomicCas { dst, .. } => check_value_dst(env, dst, &path, diags),
            Op::ChannelRecv { dst, .. } => check_value_or_discard_dst(env, dst, &path, diags),
            Op::Select { branches, .. } => {
                for branch in branches {
                    if let SelectGuard::ChannelRecv { dst, .. } = &branch.guard {
                        check_value_or_discard_dst(env, dst, &path, diags);
                    }
                }
            }
            _ => {}
        }
    }
}

fn check_assign_local_dst(env: &NameEnv, name: &str, path: &str, diags: &mut Vec<Diagnostic>) {
    let ok = env.get(name).is_some_and(|s| s.is_assign_local_target());
    if !ok {
        diags.push(
            Diagnostic::error(
                "E936",
                format!("assign_local target '{name}' is not a function local or parameter"),
            )
            .with_path(path.to_string())
            .with_fix("assign to a declared local, or use write_shared for a Var"),
        );
    }
}

fn check_value_dst(env: &NameEnv, name: &str, path: &str, diags: &mut Vec<Diagnostic>) {
    let ok = env.get(name).is_some_and(|s| s.is_writable_value());
    if !ok {
        diags.push(
            Diagnostic::error(
                "E921",
                format!("'{name}' is not a writable slot (local, param, Var, or Atomic)"),
            )
            .with_path(path.to_string())
            .with_fix("bind dst to a declared local, parameter, Var, or Atomic"),
        );
    }
}

fn check_value_or_discard_dst(env: &NameEnv, name: &str, path: &str, diags: &mut Vec<Diagnostic>) {
    let ok = env
        .get(name)
        .is_some_and(|s| s.is_writable_value() || s.is_discard());
    if !ok {
        diags.push(
            Diagnostic::error(
                "E921",
                format!("'{name}' is not a writable slot (local, param, Var, Atomic, or \"_\")"),
            )
            .with_path(path.to_string())
            .with_fix("bind dst to a declared local, parameter, Var, Atomic, or \"_\""),
        );
    }
}

fn check_call_sites(
    f: &Function,
    callees: &HashMap<String, &Function>,
    env: &NameEnv,
    mi: usize,
    fi: usize,
    diags: &mut Vec<Diagnostic>,
) {
    for (si, stmt) in f.body.iter().enumerate() {
        let path = Program::stmt_path(mi, fi, si);
        if let Op::Func { func, args, dst } = &stmt.op {
            let Some(callee) = callees.get(func) else {
                continue;
            };
            let modeled_params: Vec<&ParamDecl> =
                callee.params.iter().filter(|p| p.modeled).collect();
            if args.len() != modeled_params.len() {
                diags.push(
                    Diagnostic::error(
                        "E920",
                        format!(
                            "call('{func}') expects {} argument(s) for its modeled parameters, \
                             got {}",
                            modeled_params.len(),
                            args.len()
                        ),
                    )
                    .with_path(path.clone())
                    .with_fix(
                        "supply one argument per modeled parameter, or mark unused parameters \
                         \"modeled\": false",
                    ),
                );
            }
            if let Some(out_name) = dst {
                check_value_dst(env, out_name, &path, diags);
                let has_modeled_return = callee.returns.as_ref().is_some_and(|r| r.modeled);
                if !has_modeled_return {
                    diags.push(
                        Diagnostic::error(
                            "E923",
                            format!(
                                "call('{func}') captures a return into '{out_name}', but '{func}' \
                                 has no modeled return"
                            ),
                        )
                        .with_path(path)
                        .with_fix("declare a modeled return on the callee, or drop dst"),
                    );
                }
            }
        }
    }
}

fn check_concurrent_sites(
    f: &Function,
    callees: &HashMap<String, &Function>,
    mi: usize,
    fi: usize,
    diags: &mut Vec<Diagnostic>,
) {
    for (si, stmt) in f.body.iter().enumerate() {
        let path = Program::stmt_path(mi, fi, si);
        match &stmt.op {
            Op::Spawn { func, args, .. } | Op::AsyncCall { func, args, .. } => {
                let op = match &stmt.op {
                    Op::Spawn { .. } => "spawn",
                    _ => "async_call",
                };
                check_concurrent_callee(op, func, Some(args), callees, &path, diags);
            }
            Op::Scope { funcs } => {
                for func in funcs {
                    check_concurrent_callee("scope", func, None, callees, &path, diags);
                }
            }
            _ => {}
        }
    }
}

fn check_concurrent_callee(
    op: &str,
    func: &str,
    args: Option<&[String]>,
    callees: &HashMap<String, &Function>,
    path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let Some(callee) = callees.get(func) else {
        return;
    };
    let modeled: Vec<&ParamDecl> = callee.params.iter().filter(|p| p.modeled).collect();
    if !modeled.is_empty() {
        diags.push(
            Diagnostic::error(
                "E922",
                format!(
                    "{op}('{func}') targets a function with modeled parameters; concurrent \
                     activations share one slot — pass inputs through a resource"
                ),
            )
            .with_path(path.to_string())
            .with_fix(
                "set the callee's parameters to \"modeled\": false and communicate via a Var, \
                 Atomic, or Channel",
            ),
        );
    }
    if let Some(args) = args {
        if args.is_empty() {
            return;
        }
        let unmodeled = callee.params.iter().filter(|p| !p.modeled).count();
        if args.len() != unmodeled {
            diags.push(
                Diagnostic::error(
                    "E924",
                    format!(
                        "{op}('{func}') has {} argument(s), but '{func}' has {unmodeled} \
                         unmodeled parameter(s)",
                        args.len()
                    ),
                )
                .with_path(path.to_string())
                .with_fix("pass one codegen argument per unmodeled parameter, or omit args"),
            );
        }
    }
}

fn check_modeled_activation_on_entry(
    f: &Function,
    mi: usize,
    fi: usize,
    diags: &mut Vec<Diagnostic>,
) {
    let modeled_local = f.locals.iter().any(|l| l.modeled);
    let modeled_ret = f.returns.as_ref().is_some_and(|r| r.modeled);
    if !modeled_local && !modeled_ret {
        return;
    }
    diags.push(
        Diagnostic::warning(
            "E937",
            format!(
                "function '{}' is a spawn/async_call/scope target but has modeled locals or \
                 a modeled return; those slots are shared across activations",
                f.name
            ),
        )
        .with_path(Program::fn_path(mi, fi))
        .with_fix("keep activation slots unmodeled, or publish concurrent values through a Var"),
    );
}

fn name_referenced_as_rvalue(f: &Function, name: &str) -> bool {
    f.body.iter().any(|s| {
        let mut texts: Vec<&str> = Vec::new();
        match &s.op {
            Op::AssignLocal { expr, .. } | Op::WriteShared { expr, .. } => texts.push(expr),
            Op::AtomicStore { value, .. } | Op::ChannelSend { value, .. } => texts.push(value),
            Op::AtomicCas {
                expected, desired, ..
            } => {
                texts.push(expected);
                texts.push(desired);
            }
            Op::Func { args, .. } | Op::Spawn { args, .. } | Op::AsyncCall { args, .. } => {
                texts.extend(args.iter().map(String::as_str));
            }
            Op::Return { value: Some(value) } => texts.push(value),
            Op::Branch { cond, .. } => texts.push(cond),
            Op::Switch { var, .. } => texts.push(var),
            _ => {}
        }
        texts.iter().any(|t| contains_word(t, name))
    })
}

fn contains_word(text: &str, name: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|tok| tok == name)
}
