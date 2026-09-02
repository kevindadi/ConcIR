use std::collections::HashMap;

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::env::NameEnv;
use crate::expr;

/// E2xx: Type checking, now parser-backed (E931–E934).
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let resource_types = build_resource_type_map(program);
    check_exprs(program, diags, &resource_types);
    check_switch_variables(program, diags);
}

#[derive(Clone)]
pub(crate) enum ResType {
    Var(BaseType),
    Atomic(BaseType),
    Mutex,
    RwLock,
    Condvar,
    Semaphore,
    Channel(BaseType),
}

pub(crate) fn build_resource_type_map(program: &Program) -> HashMap<String, ResType> {
    let mut map = HashMap::new();
    for m in &program.modules {
        for r in &m.resources {
            let rt = match (r.kind.as_str(), r.res_type.as_str()) {
                ("var", "Var") => {
                    if let Some(ref bt) = r.base {
                        ResType::Var(bt.clone())
                    } else {
                        continue;
                    }
                }
                ("var", "Atomic") => {
                    if let Some(ref bt) = r.base {
                        ResType::Atomic(bt.clone())
                    } else {
                        continue;
                    }
                }
                ("sync", "Mutex") => ResType::Mutex,
                ("sync", "RwLock") => ResType::RwLock,
                ("sync", "Condvar") => ResType::Condvar,
                ("sync", "Semaphore") => ResType::Semaphore,
                ("sync", "Channel") => {
                    if let Some(ref bt) = r.base {
                        ResType::Channel(bt.clone())
                    } else {
                        continue;
                    }
                }
                _ => continue,
            };
            map.insert(r.name.clone(), rt.clone());
            map.insert(crate::fqn::fqn(&m.name, &r.name), rt);
        }
    }
    map
}

fn check_exprs(
    program: &Program,
    diags: &mut Vec<Diagnostic>,
    resource_types: &HashMap<String, ResType>,
) {
    program.walk_stmts(|mi, fi, si, m, f, stmt| {
        let path = Program::stmt_path(mi, fi, si);
        let env = NameEnv::build(program, m, f);
        match &stmt.op {
            Op::Branch { cond, .. } => {
                check_expr(
                    cond,
                    &env,
                    Some(&BaseType::Primitive("Bool".into())),
                    true,
                    &path,
                    "E201",
                    diags,
                );
            }
            Op::AssignLocal { target, expr } => {
                if let Some(ty) = env.ty(target) {
                    check_expr(expr, &env, Some(ty), false, &path, "E932", diags);
                } else {
                    check_expr(expr, &env, None, false, &path, "E932", diags);
                }
            }
            Op::WriteShared { resource, expr } => {
                if let Some(ResType::Var(expected)) = resource_types.get(resource) {
                    check_expr(expr, &env, Some(expected), false, &path, "E203", diags);
                } else {
                    check_expr(expr, &env, None, false, &path, "E203", diags);
                }
            }
            Op::AtomicStore { resource, value } => {
                if let Some(ResType::Atomic(expected)) = resource_types.get(resource) {
                    check_expr(value, &env, Some(expected), false, &path, "E204", diags);
                } else {
                    check_expr(value, &env, None, false, &path, "E204", diags);
                }
            }
            Op::AtomicCas {
                resource,
                expected,
                desired,
                dst,
            } => {
                if let Some(ResType::Atomic(ty)) = resource_types.get(resource) {
                    check_expr(expected, &env, Some(ty), false, &path, "E205", diags);
                    check_expr(desired, &env, Some(ty), false, &path, "E205", diags);
                    if let Some(dst_ty) = env.ty(dst) {
                        if dst_ty != ty && !(is_intish(dst_ty) && is_intish(ty)) {
                            diags.push(
                                Diagnostic::error(
                                    "E205",
                                    format!(
                                        "atomic_cas dst '{dst}' has type {dst_ty}, but must \
                                         hold the pre-CAS old value (Atomic base {ty}), not a \
                                         Bool success flag"
                                    ),
                                )
                                .with_path(path.clone())
                                .with_fix(
                                    "bind dst to a local or Var/Atomic of the same base type; \
                                     test success with branch(dst == expected)",
                                ),
                            );
                        }
                    }
                }
            }
            Op::ChannelSend { channel, value } => {
                if let Some(ResType::Channel(expected)) = resource_types.get(channel) {
                    check_expr(value, &env, Some(expected), false, &path, "E206", diags);
                } else {
                    check_expr(value, &env, None, false, &path, "E206", diags);
                }
            }
            Op::ChannelRecv { channel, dst } => {
                check_recv_dst(diags, &env, resource_types, channel, dst, &path);
            }
            Op::Select { branches, .. } => {
                for branch in branches {
                    if let SelectGuard::ChannelRecv { channel, dst } = &branch.guard {
                        check_recv_dst(diags, &env, resource_types, channel, dst, &path);
                    }
                }
            }
            Op::Return { value: Some(value) } => {
                if let Some(ret) = &f.returns {
                    check_expr(
                        value,
                        &env,
                        Some(&ret.param_type),
                        false,
                        &path,
                        "E932",
                        diags,
                    );
                } else {
                    check_expr(value, &env, None, false, &path, "E932", diags);
                }
            }
            Op::Func { func, args, .. } => {
                if let Some((_, callee)) = program.lookup_function(&m.name, func) {
                    let modeled: Vec<&ParamDecl> =
                        callee.params.iter().filter(|p| p.modeled).collect();
                    for (arg, param) in args.iter().zip(modeled.iter()) {
                        check_expr(
                            arg,
                            &env,
                            Some(&param.param_type),
                            false,
                            &path,
                            "E932",
                            diags,
                        );
                    }
                    for arg in args.iter().skip(modeled.len()) {
                        check_expr(arg, &env, None, false, &path, "E932", diags);
                    }
                } else {
                    for arg in args {
                        check_expr(arg, &env, None, false, &path, "E932", diags);
                    }
                }
            }
            Op::Spawn { args, .. } | Op::AsyncCall { args, .. } => {
                for arg in args {
                    check_expr(arg, &env, None, false, &path, "E932", diags);
                }
            }
            _ => {}
        }
    });
}

fn is_intish(ty: &BaseType) -> bool {
    match ty {
        BaseType::Primitive(p) if p == "Int" => true,
        BaseType::Complex(ComplexBaseType::BoundedInt { .. }) => true,
        _ => false,
    }
}

fn check_expr(
    text: &str,
    env: &NameEnv,
    expected: Option<&BaseType>,
    require_cmp: bool,
    path: &str,
    assign_code: &'static str,
    diags: &mut Vec<Diagnostic>,
) {
    let expr = match expr::parse(text, env) {
        Ok(e) => e,
        Err(e) => {
            diags.push(
                Diagnostic::error("E931", e.message)
                    .with_path(path.to_string())
                    .with_fix("fix the expression syntax, or use a declared slot or enum variant"),
            );
            return;
        }
    };
    if require_cmp && !expr.is_comparison() {
        diags.push(
            Diagnostic::error(
                "E201",
                format!("branch condition \"{text}\" is not a comparison expression"),
            )
            .with_path(path.to_string())
            .with_fix("use a comparison operator: ==, !=, >, <, >=, <="),
        );
        return;
    }
    let got = match expr::type_of(&expr, env) {
        Ok(t) => t,
        Err(te) => {
            diags.push(
                Diagnostic::error(te.code, te.message)
                    .with_path(path.to_string())
                    .with_fix("use a value name of the matching type"),
            );
            return;
        }
    };
    let Some(expected) = expected else {
        return;
    };
    if let Err(te) = expr::assignable(got.as_ref(), expected, &expr) {
        let code = if te.code == "E203" {
            assign_code
        } else {
            te.code
        };
        diags.push(
            Diagnostic::error(code, te.message)
                .with_path(path.to_string())
                .with_fix("change the value to match the expected type"),
        );
    }
}

fn check_recv_dst(
    diags: &mut Vec<Diagnostic>,
    env: &NameEnv,
    resource_types: &HashMap<String, ResType>,
    channel: &str,
    dst: &str,
    path: &str,
) {
    if dst == "_" {
        return;
    }
    let Some(ResType::Channel(expected)) = resource_types.get(channel) else {
        return;
    };
    if let Some(dst_ty) = env.ty(dst) {
        if dst_ty != expected && !(is_intish(dst_ty) && is_intish(expected)) {
            diags.push(
                Diagnostic::error(
                    "E206",
                    format!(
                        "channel_recv dst '{dst}' has type {dst_ty}, but Channel '{channel}' \
                         payload type is {expected}"
                    ),
                )
                .with_path(path.to_string())
                .with_fix(
                    "bind dst to a local or Var/Atomic of the Channel's base type, or use \
                     \"_\" to discard the payload",
                ),
            );
        }
    }
}

/// E202, E207: switch variable type and case label validation.
fn check_switch_variables(program: &Program, diags: &mut Vec<Diagnostic>) {
    program.walk_stmts(|mi, fi, si, m, f, stmt| {
        let Some((var, cases, _)) = stmt.switch() else {
            return;
        };
        let path = Program::stmt_path(mi, fi, si);
        let env = crate::env::NameEnv::build(program, m, f);
        let Some(slot) = env.get(var) else {
            diags.push(
                Diagnostic::error(
                    "E935",
                    format!("switch scrutinee '{var}' is not a declared slot"),
                )
                .with_path(format!("{path}.var"))
                .with_fix("switch on a local, param, Var, or Atomic of Enum or Int type"),
            );
            return;
        };
        if !slot.is_value_slot() {
            diags.push(
                Diagnostic::error(
                    "E935",
                    format!("switch scrutinee '{var}' is not a value slot"),
                )
                .with_path(format!("{path}.var"))
                .with_fix("switch on a local, param, Var, or Atomic of Enum or Int type"),
            );
            return;
        }
        let Some(bt) = slot.ty.as_ref() else {
            return;
        };
        match bt {
            BaseType::Primitive(p) if p == "Int" => {}
            BaseType::Complex(ComplexBaseType::BoundedInt { .. }) => {}
            BaseType::Complex(ComplexBaseType::Enum(_)) => {}
            other => {
                diags.push(
                    Diagnostic::error(
                        "E202",
                        format!("switch variable '{var}' is of type {other}, expected Enum or Int"),
                    )
                    .with_path(format!("{path}.var"))
                    .with_fix("use an Enum or Int typed slot, or use branch instead"),
                );
            }
        }
        if let BaseType::Complex(ComplexBaseType::Enum(ref variants)) = bt {
            for label in cases.keys() {
                if !variants.contains(label) {
                    diags.push(
                        Diagnostic::error(
                            "E207",
                            format!("switch case label '{label}' is not a variant of enum '{var}'"),
                        )
                        .with_path(format!("{path}.cases"))
                        .with_fix("use a valid enum variant as the case label"),
                    );
                }
            }
        }
    });
}

#[allow(dead_code)]
pub(crate) fn res_type_to_base(rt: &ResType) -> Option<BaseType> {
    match rt {
        ResType::Var(bt) | ResType::Atomic(bt) | ResType::Channel(bt) => Some(bt.clone()),
        _ => None,
    }
}
