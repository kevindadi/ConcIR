use std::collections::HashMap;

use crate::ast::*;
use crate::diagnostic::Diagnostic;

/// E2xx: Type checking.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let resource_types = build_resource_type_map(program);
    check_branch_conditions(program, diags);
    check_switch_variables(program, diags, &resource_types);
    check_write_types(program, diags, &resource_types);
    check_send_types(program, diags, &resource_types);
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
    program.walk_blocks(|mi, fi, si, _, _, block| {
        let Some(cond) = block.branch_cond() else {
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
                .with_path(format!("{}.terminator.cond", Program::block_path(mi, fi, si)))
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
    program.walk_blocks(|mi, fi, si, _, _, block| {
        let Some((var, cases, _)) = block.switch() else {
            return;
        };
        let path = Program::block_path(mi, fi, si);
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
                        .with_path(format!("{path}.terminator.var"))
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
                            .with_path(format!("{path}.terminator.cases"))
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
    program.walk_blocks(|mi, fi, si, _, _, block| {
        let path = Program::block_path(mi, fi, si);
        for stmt in &block.statements {
            if let Stmt::WriteShared { resource, expr } = stmt {
                if let Some(ResType::Var(expected)) = resource_types.get(resource) {
                    check_literal_type(diags, "E203", expr, expected, &format!("{path}.statements"));
                }
            }
        }
        match &block.call {
            Some(Call::AtomicStore { resource, value, .. }) => {
                if let Some(ResType::Atomic(expected)) = resource_types.get(resource) {
                    check_literal_type(diags, "E204", value, expected, &format!("{path}.call"));
                }
            }
            Some(Call::AtomicCas {
                resource,
                expected,
                desired,
                ..
            }) => {
                if let Some(ResType::Atomic(ty)) = resource_types.get(resource) {
                    check_literal_type(diags, "E205", expected, ty, &format!("{path}.call"));
                    check_literal_type(diags, "E205", desired, ty, &format!("{path}.call"));
                }
            }
            _ => {}
        }
    });
}

/// E206: send type checking.
fn check_send_types(
    program: &Program,
    diags: &mut Vec<Diagnostic>,
    resource_types: &HashMap<String, ResType>,
) {
    program.walk_blocks(|mi, fi, si, _, _, block| {
        if let Some(Call::ChannelSend { channel, value, .. }) = &block.call {
            if let Some(ResType::Channel(expected)) = resource_types.get(channel) {
                check_literal_type(
                    diags,
                    "E206",
                    value,
                    expected,
                    &format!("{}.call", Program::block_path(mi, fi, si)),
                );
            }
        }
    });
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
                        format!(
                            "value {v} is outside the declared Int range {lo}..={hi}"
                        ),
                    )
                    .with_path(path.to_string())
                    .with_fix(format!("use a value between {lo} and {hi}")),
                );
            }
        }
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
