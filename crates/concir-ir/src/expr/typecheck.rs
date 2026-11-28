use crate::ast::{BaseType, ComplexBaseType};
use crate::env::{NameEnv, SlotKind};

use super::{CmpOp, Expr, Lit, TypeError};

/// Type of `expr` in `env`. `None` means Unknown (unmodeled activation slot).
pub fn type_of(expr: &Expr, env: &NameEnv) -> Result<Option<BaseType>, TypeError> {
    match expr {
        Expr::Lit(Lit::Bool(_)) => Ok(Some(BaseType::Primitive("Bool".into()))),
        Expr::Lit(Lit::Int(_)) => Ok(Some(BaseType::Primitive("Int".into()))),
        Expr::Lit(Lit::Float(_)) => Ok(Some(BaseType::Primitive("Float".into()))),
        Expr::Lit(Lit::String(_)) => Ok(Some(BaseType::Primitive("String".into()))),
        Expr::Lit(Lit::Enum(_)) => Ok(None), // unify at comparison / assignment
        Expr::Name(n) => type_of_name(n, env),
        Expr::Field { base, field } => {
            let Some(ty) = type_of(base, env)? else {
                return Ok(None);
            };
            match ty {
                BaseType::Complex(ComplexBaseType::Struct(fields)) => match fields.get(field) {
                    Some(ft) => Ok(Some(ft.clone())),
                    None => Err(TypeError {
                        code: "E933",
                        message: format!("struct has no field '{field}'"),
                    }),
                },
                other => Err(TypeError {
                    code: "E933",
                    message: format!("cannot project '.{field}' from type {other}"),
                }),
            }
        }
        Expr::UnaryNeg(inner) => {
            let ty = type_of(inner, env)?;
            match ty {
                None => Ok(None),
                Some(ref t) if is_int(t) => Ok(ty),
                Some(t) => Err(TypeError {
                    code: "E932",
                    message: format!("unary '-' requires Int, found {t}"),
                }),
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            let lt = type_of(lhs, env)?;
            let rt = type_of(rhs, env)?;
            match (lt, rt) {
                (None, _) | (_, None) => Ok(None),
                (Some(l), Some(r)) if is_int(&l) && is_int(&r) => {
                    Ok(Some(BaseType::Primitive("Int".into())))
                }
                (Some(l), Some(r)) if is_float(&l) && is_float(&r) => {
                    Ok(Some(BaseType::Primitive("Float".into())))
                }
                (Some(l), Some(r)) => Err(TypeError {
                    code: "E932",
                    message: format!(
                        "binary operator requires matching numeric types, found {l} and {r}"
                    ),
                }),
            }
        }
        Expr::Cmp { op, lhs, rhs } => {
            let lt = type_of(lhs, env)?;
            let rt = type_of(rhs, env)?;
            check_comparable(op, lt.as_ref(), rt.as_ref(), lhs, rhs)?;
            Ok(Some(BaseType::Primitive("Bool".into())))
        }
        Expr::Struct { fields } => {
            let mut map = std::collections::BTreeMap::new();
            for (name, val) in fields {
                let Some(ty) = type_of(val, env)? else {
                    continue;
                };
                map.insert(name.clone(), ty);
            }
            Ok(Some(BaseType::Complex(ComplexBaseType::Struct(map))))
        }
    }
}

fn type_of_name(n: &str, env: &NameEnv) -> Result<Option<BaseType>, TypeError> {
    let Some(slot) = env.get(n) else {
        return Err(TypeError {
            code: "E931",
            message: format!("undefined name '{n}' in expression"),
        });
    };
    match slot.kind {
        SlotKind::Discard => Err(TypeError {
            code: "E931",
            message: "\"_\" is not an r-value".into(),
        }),
        SlotKind::SyncResource => Err(TypeError {
            code: "E934",
            message: format!("'{n}' is a sync resource and cannot appear as a value"),
        }),
        SlotKind::Local | SlotKind::Param | SlotKind::Return => {
            if !slot.modeled {
                return Ok(None);
            }
            Ok(slot.ty.clone())
        }
        SlotKind::Var | SlotKind::Atomic => Ok(slot.ty.clone()),
    }
}

fn check_comparable(
    op: &CmpOp,
    lt: Option<&BaseType>,
    rt: Option<&BaseType>,
    lhs: &Expr,
    rhs: &Expr,
) -> Result<(), TypeError> {
    match (lt, rt) {
        (None, None) => Ok(()),
        (Some(l), None) => match rhs {
            Expr::Lit(Lit::Enum(_)) if enum_lit_ok(rhs, l) => Ok(()),
            Expr::Lit(Lit::Enum(v)) => Err(TypeError {
                code: "E932",
                message: format!("'{v}' is not a variant of {l}"),
            }),
            _ => Ok(()),
        },
        (None, Some(r)) => match lhs {
            Expr::Lit(Lit::Enum(_)) if enum_lit_ok(lhs, r) => Ok(()),
            Expr::Lit(Lit::Enum(v)) => Err(TypeError {
                code: "E932",
                message: format!("'{v}' is not a variant of {r}"),
            }),
            _ => Ok(()),
        },
        (Some(l), Some(r)) => {
            if enum_lit_ok(lhs, r) || enum_lit_ok(rhs, l) {
                return Ok(());
            }
            let ordered = matches!(op, CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge);
            if ordered {
                if is_int(l) && is_int(r) || is_float(l) && is_float(r) {
                    return Ok(());
                }
                return Err(TypeError {
                    code: "E932",
                    message: format!(
                        "ordered comparison requires numeric types, found {l} and {r}"
                    ),
                });
            }
            if types_eq(l, r) || is_int(l) && is_int(r) {
                return Ok(());
            }
            Err(TypeError {
                code: "E932",
                message: format!("comparison operands have types {l} and {r}"),
            })
        }
    }
}

fn enum_lit_ok(expr: &Expr, other: &BaseType) -> bool {
    match (expr, other) {
        (Expr::Lit(Lit::Enum(v)), BaseType::Complex(ComplexBaseType::Enum(vars))) => {
            vars.iter().any(|x| x == v)
        }
        _ => false,
    }
}

pub fn assignable(
    got: Option<&BaseType>,
    expected: &BaseType,
    expr: &Expr,
) -> Result<(), TypeError> {
    if got.is_none() {
        if let Expr::Lit(Lit::Enum(v)) = expr {
            if let BaseType::Complex(ComplexBaseType::Enum(vars)) = expected {
                if vars.iter().any(|x| x == v) {
                    return Ok(());
                }
                return Err(TypeError {
                    code: "E932",
                    message: format!("'{v}' is not a variant of {expected}"),
                });
            }
        }
        return Ok(());
    }
    let got = got.unwrap();
    if types_eq(got, expected) || is_int(got) && is_int(expected) {
        if let BaseType::Complex(ComplexBaseType::BoundedInt { lo, hi }) = expected {
            if let Expr::Lit(Lit::Int(v)) = expr {
                if v < lo || v > hi {
                    return Err(TypeError {
                        code: "E203",
                        message: format!("value {v} is outside the declared Int range {lo}..={hi}"),
                    });
                }
            }
        }
        return Ok(());
    }
    if let (
        BaseType::Complex(ComplexBaseType::Struct(g)),
        BaseType::Complex(ComplexBaseType::Struct(e)),
    ) = (got, expected)
    {
        for (k, et) in e {
            match g.get(k) {
                None => {
                    return Err(TypeError {
                        code: "E933",
                        message: format!("struct literal missing field '{k}'"),
                    });
                }
                Some(gt) if !types_eq(gt, et) && !(is_int(gt) && is_int(et)) => {
                    return Err(TypeError {
                        code: "E932",
                        message: format!("field '{k}' has type {gt}, expected {et}"),
                    });
                }
                _ => {}
            }
        }
        for k in g.keys() {
            if !e.contains_key(k) {
                return Err(TypeError {
                    code: "E933",
                    message: format!("struct literal has unknown field '{k}'"),
                });
            }
        }
        return Ok(());
    }
    Err(TypeError {
        code: "E932",
        message: format!("type mismatch: expected {expected}, found {got}"),
    })
}

fn is_int(ty: &BaseType) -> bool {
    match ty {
        BaseType::Primitive(p) if p == "Int" => true,
        BaseType::Complex(ComplexBaseType::BoundedInt { .. }) => true,
        _ => false,
    }
}

fn is_float(ty: &BaseType) -> bool {
    matches!(ty, BaseType::Primitive(p) if p == "Float")
}

fn types_eq(a: &BaseType, b: &BaseType) -> bool {
    a == b
}
