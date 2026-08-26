use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::fqn;

/// E9xx: Typed data-flow checks (params, returns, call sites).
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let mut resource_names: HashSet<String> = HashSet::new();
    let mut var_resources: HashSet<String> = HashSet::new();
    let mut callees: HashMap<String, &Function> = HashMap::new();

    for m in &program.modules {
        for r in &m.resources {
            resource_names.insert(r.name.clone());
            resource_names.insert(fqn::fqn(&m.name, &r.name));
            if r.kind == "var" {
                var_resources.insert(r.name.clone());
                var_resources.insert(fqn::fqn(&m.name, &r.name));
            }
        }
        for f in &m.functions {
            callees.insert(f.name.clone(), f);
            callees.insert(fqn::fqn(&m.name, &f.name), f);
        }
    }

    for (mi, m) in program.modules.iter().enumerate() {
        for (fi, f) in m.functions.iter().enumerate() {
            check_param_decls(f, &resource_names, mi, fi, diags);
            check_return_decl(f, mi, fi, diags);
            check_call_sites(f, &callees, &var_resources, mi, fi, diags);
        }
    }
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
        if !p.modeled && param_referenced_in_body(f, &p.name) {
            diags.push(
                Diagnostic::error(
                    "E912",
                    format!(
                        "parameter '{}' of function '{}' is referenced by an expression but \
                         modeled: false; the value is not in the net",
                        p.name, f.name
                    ),
                )
                .with_path(path.clone())
                .with_fix("set \"modeled\": true so the parameter enters the CVN variable store"),
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
        .filter(|b| matches!(&b.terminator, Terminator::Return { value: None }))
        .count();
    if bare_returns > 0 {
        diags.push(
            Diagnostic::warning(
                "E913",
                format!(
                    "function '{}' declares a modeled return '{}' but {} return terminator(s) \
                     carry no value; those paths bind Unknown",
                    f.name, ret.name, bare_returns
                ),
            )
            .with_path(format!("{}.returns", Program::fn_path(mi, fi)))
            .with_fix("give every return terminator a value expression"),
        );
    }
}

fn check_call_sites(
    f: &Function,
    callees: &HashMap<String, &Function>,
    var_resources: &HashSet<String>,
    mi: usize,
    fi: usize,
    diags: &mut Vec<Diagnostic>,
) {
    for (si, block) in f.body.iter().enumerate() {
        for stmt in &block.statements {
            let Stmt::Func { func, args, dst } = stmt else {
                continue;
            };
            let Some(callee) = callees.get(func) else {
                continue;
            };

            let modeled_params: Vec<&ParamDecl> =
                callee.params.iter().filter(|p| p.modeled).collect();
            let path = format!("{}.statements", Program::block_path(mi, fi, si));

            if args.len() != modeled_params.len() {
                diags.push(
                    Diagnostic::error(
                        "E920",
                        format!(
                        "call('{func}') expects {} argument(s) for its modeled parameters, got {}",
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
                if !var_resources.contains(out_name) {
                    diags.push(
                        Diagnostic::error(
                            "E921",
                            format!(
                            "call('{func}') captures its return into '{out_name}', which is not \
                             a writable Var/Atomic resource"
                        ),
                        )
                        .with_path(path.clone())
                        .with_fix("capture into a declared Var or Atomic resource"),
                    );
                }
            }
        }
    }
}

fn param_referenced_in_body(f: &Function, param: &str) -> bool {
    f.body.iter().any(|b| {
        let mut texts: Vec<&str> = Vec::new();
        for s in &b.statements {
            match s {
                Stmt::AssignLocal { expr, .. } | Stmt::WriteShared { expr, .. } => texts.push(expr),
                Stmt::AtomicStore { value, .. } | Stmt::ChannelSend { value, .. } => {
                    texts.push(value);
                }
                Stmt::AtomicCas {
                    expected, desired, ..
                } => {
                    texts.push(expected);
                    texts.push(desired);
                }
                Stmt::Func { args, .. }
                | Stmt::Spawn { args, .. }
                | Stmt::AsyncCall { args, .. } => {
                    texts.extend(args.iter().map(String::as_str));
                }
                _ => {}
            }
        }
        if let Terminator::Return { value: Some(value) } = &b.terminator {
            texts.push(value);
        }
        if let Some(cond) = b.branch_cond() {
            texts.push(cond);
        }
        if let Some((var, _, _)) = b.switch() {
            texts.push(var);
        }
        texts.iter().any(|t| contains_word(t, param))
    })
}

fn contains_word(text: &str, param: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|tok| tok == param)
}
