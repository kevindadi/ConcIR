use std::collections::HashSet;

use crate::ast::*;
use crate::diagnostic::Diagnostic;

/// E9xx: Typed data-flow checks (params, returns, call sites).
///
/// Data-flow follows the projection principle: only `modeled: true` params and
/// returns enter the CVN variable store. Unmodeled values are codegen-only
/// placeholders and must never be referenced by the model's expressions.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let resource_names: HashSet<&str> = program.resources.iter().map(|r| r.name.as_str()).collect();
    let var_resources: HashSet<&str> = program
        .resources
        .iter()
        .filter(|r| r.kind == "var")
        .map(|r| r.name.as_str())
        .collect();
    let callees: std::collections::HashMap<&str, &Function> = program
        .functions
        .iter()
        .map(|f| (f.name.as_str(), f))
        .collect();

    for (fi, f) in program.functions.iter().enumerate() {
        check_param_decls(f, &resource_names, fi, diags);
        check_return_decl(f, fi, diags);
        check_call_sites(f, &callees, &var_resources, fi, diags);
    }
}

fn check_param_decls(
    f: &Function,
    resource_names: &HashSet<&str>,
    fi: usize,
    diags: &mut Vec<Diagnostic>,
) {
    let mut seen: HashSet<&str> = HashSet::new();
    for (pi, p) in f.params.iter().enumerate() {
        let path = format!("functions[{fi}].params[{pi}]");
        if resource_names.contains(p.name.as_str()) {
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
        // E912: an unmodeled param must not be referenced by the model's
        // expressions (it is not in the CVN variable store).
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

fn check_return_decl(f: &Function, fi: usize, diags: &mut Vec<Diagnostic>) {
    let Some(ret) = &f.returns else {
        return;
    };
    if !ret.modeled {
        return;
    }
    // E913: a modeled return should be produced by every return statement;
    // a bare `return` binds Unknown.
    let bare_returns = f
        .body
        .iter()
        .filter(|s| matches!(&s.op, Op::Return(None)))
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
            .with_path(format!("functions[{fi}].returns"))
            .with_fix("give every return statement a value expression"),
        );
    }
}

fn check_call_sites(
    f: &Function,
    callees: &std::collections::HashMap<&str, &Function>,
    var_resources: &HashSet<&str>,
    fi: usize,
    diags: &mut Vec<Diagnostic>,
) {
    for (si, stmt) in f.body.iter().enumerate() {
        let Op::Call { target, extras } = &stmt.op else {
            continue;
        };
        let Some(callee) = callees.get(target.as_str()) else {
            continue; // undefined target is already E102
        };

        let modeled_params: Vec<&ParamDecl> = callee.params.iter().filter(|p| p.modeled).collect();
        let has_modeled_return = callee
            .returns
            .as_ref()
            .map(|r| r.modeled)
            .unwrap_or(false);

        // Interpret extras: optional out-var (only when the callee models a
        // return), then the argument list.
        let (out, args): (Option<&String>, &[String]) = if has_modeled_return {
            if extras.is_empty() {
                (None, &[])
            } else {
                let out = if extras[0].is_empty() {
                    None
                } else {
                    Some(&extras[0])
                };
                (out, &extras[1..])
            }
        } else {
            (None, extras.as_slice())
        };

        let path = format!("functions[{fi}].body[{si}].op");

        // E920: argument count must match the callee's modeled params.
        if args.len() != modeled_params.len() {
            diags.push(
                Diagnostic::error(
                    "E920",
                    format!(
                        "call('{target}') expects {} argument(s) for its modeled parameters, got {}",
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

        // E921: the capture out-var must be a writable Var/Atomic resource.
        if let Some(out_name) = out {
            if !var_resources.contains(out_name.as_str()) {
                diags.push(
                    Diagnostic::error(
                        "E921",
                        format!(
                            "call('{target}') captures its return into '{out_name}', which is not \
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

/// Does the body reference `param` in any model expression (branch condition,
/// switch variable, write/store/send/cas value, or return value)?
fn param_referenced_in_body(f: &Function, param: &str) -> bool {
    f.body.iter().any(|s| {
        let value_texts: Vec<&str> = match &s.op {
            Op::ResOp {
                action, args, ..
            } if matches!(action.as_str(), "write" | "store" | "send" | "cas") => {
                args.iter().map(String::as_str).collect()
            }
            Op::Return(Some(value)) => vec![value.as_str()],
            _ => Vec::new(),
        };
        value_texts.iter().any(|t| contains_word(t, param))
            || matches!(&s.transfer, Transfer::Branch { cond, .. } if contains_word(cond, param))
            || matches!(&s.transfer, Transfer::Switch { var, .. } if contains_word(var, param))
    })
}

/// True when `param` appears as a whole word in `text` (split on non-identifier
/// characters).
fn contains_word(text: &str, param: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|tok| tok == param)
}
