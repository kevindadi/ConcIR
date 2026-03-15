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
            // Init value is not stored in AST currently; skip deep check.
            // This would require extending the parser to keep the init literal.
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
                let lhs_name = &cond.lhs.value;
                if let Some(rt) = resource_types.get(lhs_name) {
                    let lhs_type = res_type_to_base(rt);
                    if let Some(bt) = lhs_type {
                        if !is_comparable_to_bool(&bt, &cond.op, &cond.rhs) {
                            diags.push(
                                Diagnostic::error(
                                    "E201",
                                    format!(
                                        "branch condition does not produce Bool: '{lhs_name}' is of type {bt:?}"
                                    ),
                                )
                                .with_span(cond.span, source)
                                .with_fix("use a comparison operator (==, !=, >, <, >=, <=) that yields Bool"),
                            );
                        }
                    }
                }
            }
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
    let inferred = match expr {
        Expr::Literal(lit) => lit.infer_base_type(),
        Expr::BinOp(_, _, lit) => lit.infer_base_type(),
        Expr::Ident(_) => None,
    };
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

fn is_comparable_to_bool(bt: &BaseType, _op: &CmpOp, _rhs: &CondOperand) -> bool {
    // Any comparison on numeric/bool/string/enum types produces Bool.
    matches!(
        bt,
        BaseType::Int | BaseType::Float | BaseType::Bool | BaseType::String | BaseType::Enum(_)
    )
}
