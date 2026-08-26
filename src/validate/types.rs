use std::collections::HashMap;

use crate::ast::*;
use crate::diagnostic::Diagnostic;

/// E2xx: Type checking.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let resource_types = build_resource_type_map(program);
    check_branch_conditions(program, diags);
    check_switch_variables(program, diags, &resource_types);
    check_write_types(program, diags, &resource_types);
    check_channel_payload_types(program, diags, &resource_types);
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

/// E201: branch condition must be a comparison expression producing Bool.
/// In the JSON format, conditions are strings. We check for the presence of
/// a comparison operator (==, !=, >, <, >=, <=).
fn check_branch_conditions(program: &Program, diags: &mut Vec<Diagnostic>) {
    program.walk_stmts(|mi, fi, si, _, _, stmt| {
        let Some(cond) = stmt.branch_cond() else {
            return;
        };
        let has_cmp = cond.contains("==")
            || cond.contains("!=")
            || cond.contains(">=")
            || cond.contains("<=")
            || cond.contains('>')
            || cond.contains('<');
        if !has_cmp {
            diags.push(
                Diagnostic::error(
                    "E201",
                    format!("branch condition \"{cond}\" is not a comparison expression"),
                )
                .with_path(Program::stmt_path(mi, fi, si))
                .with_fix("use a comparison operator: ==, !=, >, <, >=, <="),
            );
        }
    });
}

/// E202, E207: switch variable type and case label validation.
fn check_switch_variables(
    program: &Program,
    diags: &mut Vec<Diagnostic>,
    resource_types: &HashMap<String, ResType>,
) {
    program.walk_stmts(|mi, fi, si, _, _, stmt| {
        let Some((var, cases, _)) = stmt.switch() else {
            return;
        };
        let path = Program::stmt_path(mi, fi, si);
        if let Some(rt) = resource_types.get(var) {
            let bt = res_type_to_base(rt);
            match bt {
                Some(BaseType::Primitive(ref p)) if p == "Int" => {}
                Some(BaseType::Complex(ComplexBaseType::BoundedInt { .. })) => {}
                Some(BaseType::Complex(ComplexBaseType::Enum(_))) => {}
                Some(ref other) => {
                    diags.push(
                        Diagnostic::error(
                            "E202",
                            format!(
                                "switch variable '{var}' is of type {other}, expected Enum or Int"
                            ),
                        )
                        .with_path(format!("{path}.var"))
                        .with_fix("use an Enum or Int typed resource, or use branch instead"),
                    );
                }
                None => {}
            }
            if let Some(BaseType::Complex(ComplexBaseType::Enum(ref variants))) = bt {
                for label in cases.keys() {
                    if !variants.contains(label) {
                        diags.push(
                            Diagnostic::error(
                                "E207",
                                format!(
                                    "switch case label '{label}' is not a variant of enum '{var}'"
                                ),
                            )
                            .with_path(format!("{path}.cases"))
                            .with_fix("use a valid enum variant as the case label"),
                        );
                    }
                }
            }
        }
    });
}

/// E203, E204, E205: write/store/cas type checking (best-effort on literal values).
fn check_write_types(
    program: &Program,
    diags: &mut Vec<Diagnostic>,
    resource_types: &HashMap<String, ResType>,
) {
    program.walk_stmts(|mi, fi, si, _, f, stmt| {
        let path = Program::stmt_path(mi, fi, si);
        match &stmt.op {
            Op::WriteShared { resource, expr } => {
                if let Some(ResType::Var(expected)) = resource_types.get(resource) {
                    check_literal_type(diags, "E203", expr, expected, &path);
                }
            }
            Op::AtomicStore { resource, value } => {
                if let Some(ResType::Atomic(expected)) = resource_types.get(resource) {
                    check_literal_type(diags, "E204", value, expected, &path);
                }
            }
            Op::AtomicCas {
                resource,
                expected,
                desired,
                dst,
            } => {
                if let Some(ResType::Atomic(ty)) = resource_types.get(resource) {
                    check_literal_type(diags, "E205", expected, ty, &path);
                    check_literal_type(diags, "E205", desired, ty, &path);
                    if let Some(dst_ty) = lookup_dst_type(f, resource_types, dst) {
                        if dst_ty != ty {
                            diags.push(
                                Diagnostic::error(
                                    "E205",
                                    format!(
                                        "atomic_cas dst '{dst}' has type {dst_ty}, but must \
                                             hold the pre-CAS old value (Atomic base {ty}), not a \
                                             Bool success flag"
                                    ),
                                )
                                .with_path(path.to_string())
                                .with_fix(
                                    "bind dst to a local or Var/Atomic of the same base type; \
                                         test success with branch(dst == expected)",
                                ),
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    });
}

/// E206: `channel_send` value and `channel_recv` `dst` must match Channel `base`.
fn check_channel_payload_types(
    program: &Program,
    diags: &mut Vec<Diagnostic>,
    resource_types: &HashMap<String, ResType>,
) {
    program.walk_stmts(|mi, fi, si, _, f, stmt| {
        let path = Program::stmt_path(mi, fi, si);
        match &stmt.op {
            Op::ChannelSend { channel, value } => {
                if let Some(ResType::Channel(expected)) = resource_types.get(channel) {
                    check_literal_type(diags, "E206", value, expected, &path);
                }
            }
            Op::ChannelRecv { channel, dst } => {
                check_recv_dst(diags, f, resource_types, channel, dst, &path);
            }
            Op::Select { branches, .. } => {
                for branch in branches {
                    if let SelectGuard::ChannelRecv { channel, dst } = &branch.guard {
                        check_recv_dst(diags, f, resource_types, channel, dst, &path);
                    }
                }
            }
            _ => {}
        }
    });
}

/// `dst` is the popped payload (Channel `base`). `"_"` discards. Unknown names
/// are left to later passes (same as `atomic_cas` `dst`).
fn check_recv_dst(
    diags: &mut Vec<Diagnostic>,
    f: &Function,
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
    if let Some(dst_ty) = lookup_dst_type(f, resource_types, dst) {
        if dst_ty != expected {
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

/// Best-effort type check: only flags mismatches when the value is a recognizable literal.
fn check_literal_type(
    diags: &mut Vec<Diagnostic>,
    code: &'static str,
    val: &str,
    expected: &BaseType,
    path: &str,
) {
    let inferred = infer_literal_type(val);
    if let Some(ref actual) = inferred {
        let expected_name = match expected {
            BaseType::Primitive(p) => Some(p.as_str()),
            _ => None,
        };
        let actual_name = match actual {
            BaseType::Primitive(p) => Some(p.as_str()),
            _ => None,
        };
        if let (Some(e), Some(a)) = (expected_name, actual_name) {
            if e != a {
                diags.push(
                    Diagnostic::error(code, format!("type mismatch: expected {e}, found {a}"))
                        .with_path(path.to_string())
                        .with_fix("change the value to match the expected type"),
                );
            }
        }
    }

    // Bounded Int: the value must lie within the declared domain.
    if let BaseType::Complex(ComplexBaseType::BoundedInt { lo, hi }) = expected {
        if let Ok(v) = val.trim().parse::<i64>() {
            if v < *lo || v > *hi {
                diags.push(
                    Diagnostic::error(
                        code,
                        format!("value {v} is outside the declared Int range {lo}..={hi}"),
                    )
                    .with_path(path.to_string())
                    .with_fix(format!("use a value between {lo} and {hi}")),
                );
            }
        }
    }
}

fn lookup_dst_type<'a>(
    f: &'a Function,
    resource_types: &'a HashMap<String, ResType>,
    dst: &str,
) -> Option<&'a BaseType> {
    if let Some(local) = f.locals.iter().find(|l| l.name == dst) {
        return Some(&local.local_type);
    }
    if let Some(p) = f.params.iter().find(|p| p.name == dst) {
        return Some(&p.param_type);
    }
    match resource_types.get(dst) {
        Some(ResType::Var(bt) | ResType::Atomic(bt)) => Some(bt),
        _ => None,
    }
}

fn infer_literal_type(val: &str) -> Option<BaseType> {
    if val == "true" || val == "false" {
        return Some(BaseType::Primitive("Bool".to_string()));
    }
    if val.parse::<i64>().is_ok() {
        return Some(BaseType::Primitive("Int".to_string()));
    }
    if val.parse::<f64>().is_ok() && val.contains('.') {
        return Some(BaseType::Primitive("Float".to_string()));
    }
    if val.starts_with('"') && val.ends_with('"') {
        return Some(BaseType::Primitive("String".to_string()));
    }
    None
}

pub(crate) fn res_type_to_base(rt: &ResType) -> Option<BaseType> {
    match rt {
        ResType::Var(bt) | ResType::Atomic(bt) | ResType::Channel(bt) => Some(bt.clone()),
        _ => None,
    }
}
