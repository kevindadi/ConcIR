use std::collections::HashMap;

use crate::ast::*;
use crate::diagnostic::Diagnostic;

/// E2xx: Type checking.
pub fn check(program: &Program, source: &str, diags: &mut Vec<Diagnostic>) {
    let resource_types = build_resource_type_map(program);
    check_init_values(program, source, diags);
    check_branch_conditions(program, source, diags, &resource_types);
    check_switch_variables(program, source, diags, &resource_types);
    check_write_types(program, source, diags, &resource_types);
    check_send_types(program, source, diags, &resource_types);
}

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
    for r in &program.resources {
        let rt = match &r.kind {
            ResourceKind::Var(VarType::Var(bt)) => ResType::Var(bt.clone()),
            ResourceKind::Var(VarType::Atomic(bt)) => ResType::Atomic(bt.clone()),
            ResourceKind::Sync(SyncType::Mutex(_)) => ResType::Mutex,
            ResourceKind::Sync(SyncType::RwLock(_)) => ResType::RwLock,
            ResourceKind::Sync(SyncType::Condvar(_)) => ResType::Condvar,
            ResourceKind::Sync(SyncType::Semaphore(_, _)) => ResType::Semaphore,
            ResourceKind::Sync(SyncType::Channel(_, bt)) => ResType::Channel(bt.clone()),
        };
        map.insert(r.name.value.clone(), rt);
    }
    map
}

fn check_init_values(program: &Program, source: &str, diags: &mut Vec<Diagnostic>) {
    for r in &program.resources {
        if let ResourceKind::Var(vt) = &r.kind {
            let expected = match vt {
                VarType::Var(bt) | VarType::Atomic(bt) => bt,
            };
            let _ = expected;
        }
    }
    let _ = (source, diags);
}

fn check_branch_conditions(
    program: &Program,
    source: &str,
    diags: &mut Vec<Diagnostic>,
    resource_types: &HashMap<String, ResType>,
) {
    for f in &program.functions {
        for stmt in &f.statements {
            if let Transfer::Branch(ref cond, _, _) = stmt.transfer {
                // With the v0.2 grammar, cond_expr is always `expr cmp_op expr`,
                // which always produces Bool. We only flag E201 when operands are
                // structurally incomparable types (Struct, Array).
                let lhs_type = infer_expr_resource_type(&cond.lhs, resource_types);
                let rhs_type = infer_expr_resource_type(&cond.rhs, resource_types);

                for bt in [&lhs_type, &rhs_type].into_iter().flatten() {
                    if matches!(bt, BaseType::Struct(_) | BaseType::Array(_, _)) {
                        diags.push(
                            Diagnostic::error(
                                "E201",
                                format!(
                                    "branch condition uses incomparable type {bt:?}"
                                ),
                            )
                            .with_span(cond.span, source)
                            .with_fix("use a comparable type (Int, Float, Bool, String, Enum)"),
                        );
                        break;
                    }
                }
            }
        }
    }
}

/// Try to infer the base type of an expression by looking up identifiers
/// in the resource type map.
fn infer_expr_resource_type(
    expr: &Expr,
    resource_types: &HashMap<String, ResType>,
) -> Option<BaseType> {
    match expr {
        Expr::Ident(id) => {
            resource_types.get(&id.value).and_then(res_type_to_base)
        }
        Expr::Literal(lit) => lit.infer_base_type(),
        Expr::BinOp(lhs, _, _) => infer_expr_resource_type(lhs, resource_types),
        Expr::Paren(inner) | Expr::UnaryMinus(inner) => {
            infer_expr_resource_type(inner, resource_types)
        }
    }
}

fn check_switch_variables(
    program: &Program,
    source: &str,
    diags: &mut Vec<Diagnostic>,
    resource_types: &HashMap<String, ResType>,
) {
    for f in &program.functions {
        for stmt in &f.statements {
            if let Transfer::Switch(ref var, ref cases) = stmt.transfer {
                if let Some(rt) = resource_types.get(&var.value) {
                    let bt = res_type_to_base(rt);
                    match bt {
                        Some(BaseType::Enum(_)) | Some(BaseType::Int) => {}
                        Some(ref other) => {
                            diags.push(
                                Diagnostic::error(
                                    "E202",
                                    format!(
                                        "switch variable '{}' is of type {other:?}, expected Enum or Int",
                                        var.value
                                    ),
                                )
                                .with_span(var.span, source)
                                .with_fix("use an Enum or Int typed resource, or use branch instead"),
                            );
                        }
                        None => {}
                    }
                    if let Some(BaseType::Enum(ref variants)) = bt {
                        for case in cases {
                            if let Literal::Ident(ref label) = case.label {
                                if !variants.contains(label) {
                                    diags.push(
                                        Diagnostic::error(
                                            "E207",
                                            format!(
                                                "switch case label '{label}' is not a variant of enum '{}'",
                                                var.value
                                            ),
                                        )
                                        .with_span(case.target.span, source)
                                        .with_fix("use a valid enum variant as the case label"),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn check_write_types(
    program: &Program,
    source: &str,
    diags: &mut Vec<Diagnostic>,
    resource_types: &HashMap<String, ResType>,
) {
    for f in &program.functions {
        for stmt in &f.statements {
            if let Op::ResOp(ref res, ref action) = stmt.op {
                let rt = match resource_types.get(&res.value) {
                    Some(r) => r,
                    None => continue,
                };
                match action {
                    Action::Write(expr) => {
                        if let ResType::Var(ref expected) = rt {
                            check_expr_type(diags, source, stmt.span, "E203", expr, expected);
                        }
                    }
                    Action::Store(expr) => {
                        if let ResType::Atomic(ref expected) = rt {
                            check_expr_type(diags, source, stmt.span, "E204", expr, expected);
                        }
                    }
                    Action::Cas(e1, e2) => {
                        if let ResType::Atomic(ref expected) = rt {
                            check_expr_type(diags, source, stmt.span, "E205", e1, expected);
                            check_expr_type(diags, source, stmt.span, "E205", e2, expected);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn check_send_types(
    program: &Program,
    source: &str,
    diags: &mut Vec<Diagnostic>,
    resource_types: &HashMap<String, ResType>,
) {
    for f in &program.functions {
        for stmt in &f.statements {
            if let Op::ResOp(ref res, Action::Send(ref expr)) = stmt.op {
                if let Some(ResType::Channel(ref expected)) = resource_types.get(&res.value) {
                    check_expr_type(diags, source, stmt.span, "E206", expr, expected);
                }
            }
        }
    }
}

fn check_expr_type(
    diags: &mut Vec<Diagnostic>,
    source: &str,
    span: crate::span::Span,
    code: &'static str,
    expr: &Expr,
    expected: &BaseType,
) {
    let inferred = expr.infer_base_type();
    if let Some(ref actual) = inferred {
        if actual != expected {
            diags.push(
                Diagnostic::error(
                    code,
                    format!("type mismatch: expected {expected:?}, found {actual:?}"),
                )
                .with_span(span, source)
                .with_fix("change the value to match the expected type"),
            );
        }
    }
}

fn res_type_to_base(rt: &ResType) -> Option<BaseType> {
    match rt {
        ResType::Var(bt) | ResType::Atomic(bt) | ResType::Channel(bt) => Some(bt.clone()),
        _ => None,
    }
}
