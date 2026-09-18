//! Lowered expressions and evaluation.
//!
//! Expressions are parsed by the shared `crate::expr` parser, then *lowered*
//! so every name is resolved to a [`SlotRef`] or resource ID. The runtime
//! never compares short names across modules.

use crate::expr::{BinOp, CmpOp, Lit};
use crate::sem::ids::{ResourceId, SlotId, SlotRef};
use crate::sem::outcome::{BackendError, BackendResult};
use crate::sem::value::Value;

/// A lowered expression with fully resolved names.
#[derive(Debug, Clone, PartialEq)]
pub enum LExpr {
    Lit(Lit),
    Slot(SlotRef),
    Field {
        base: Box<LExpr>,
        field: String,
    },
    Neg(Box<LExpr>),
    Bin {
        op: BinOp,
        lhs: Box<LExpr>,
        rhs: Box<LExpr>,
    },
    Cmp {
        op: CmpOp,
        lhs: Box<LExpr>,
        rhs: Box<LExpr>,
    },
    Struct {
        fields: Vec<(String, LExpr)>,
    },
}

impl LExpr {
    pub fn mentions_float(&self) -> bool {
        match self {
            LExpr::Lit(Lit::Float(_)) => true,
            LExpr::Lit(_) => false,
            // A slot's type is not carried here; callers that need to reject
            // float control flow check the resolved types separately.
            LExpr::Slot(_) => false,
            LExpr::Field { base, .. } => base.mentions_float(),
            LExpr::Neg(e) => e.mentions_float(),
            LExpr::Bin { lhs, rhs, .. } | LExpr::Cmp { lhs, rhs, .. } => {
                lhs.mentions_float() || rhs.mentions_float()
            }
            LExpr::Struct { fields } => fields.iter().any(|(_, e)| e.mentions_float()),
        }
    }

    pub fn is_comparison(&self) -> bool {
        matches!(self, LExpr::Cmp { .. })
    }
}

/// Read access to the operational store during expression evaluation.
pub trait ValueStore {
    fn get_slot(&self, slot: SlotId) -> Option<&Value>;
    fn get_shared(&self, resource: ResourceId) -> Option<&Value>;
}

pub fn eval(expr: &LExpr, store: &dyn ValueStore, at: &str) -> BackendResult<Value> {
    match expr {
        LExpr::Lit(l) => Ok(lit_value(l)),
        LExpr::Slot(SlotRef::Discard) => {
            Err(BackendError::invalid("E931", "\"_\" is not an r-value"))
        }
        LExpr::Slot(SlotRef::Local(slot)) => store.get_slot(*slot).cloned().ok_or_else(|| {
            BackendError::invalid("E900", format!("uninitialized frame slot {slot} at {at}"))
        }),
        LExpr::Slot(SlotRef::Shared(r)) => store.get_shared(*r).cloned().ok_or_else(|| {
            BackendError::invalid("E900", format!("shared resource {r} has no value at {at}"))
        }),
        LExpr::Field { base, field } => {
            let v = eval(base, store, at)?;
            match v {
                Value::Struct(fields) => fields.get(field).cloned().ok_or_else(|| {
                    BackendError::invalid("E933", format!("struct has no field '{field}'"))
                }),
                other => Err(BackendError::invalid(
                    "E933",
                    format!("cannot project '.{field}' from {}", other.canonical()),
                )),
            }
        }
        LExpr::Neg(inner) => {
            let v = eval(inner, store, at)?;
            match v {
                Value::Int(i) => i
                    .checked_neg()
                    .map(Value::Int)
                    .ok_or_else(|| BackendError::invalid("E901", "integer negation overflow")),
                Value::Float(f) => Ok(Value::Float(-f)),
                other => Err(BackendError::invalid(
                    "E932",
                    format!(
                        "unary '-' requires Int or Float, found {}",
                        other.canonical()
                    ),
                )),
            }
        }
        LExpr::Bin { op, lhs, rhs } => {
            let l = eval(lhs, store, at)?;
            let r = eval(rhs, store, at)?;
            eval_bin(*op, l, r, at)
        }
        LExpr::Cmp { op, lhs, rhs } => {
            let l = eval(lhs, store, at)?;
            let r = eval(rhs, store, at)?;
            eval_cmp(*op, l, r, at)
        }
        LExpr::Struct { fields } => {
            let mut out = std::collections::BTreeMap::new();
            for (k, e) in fields {
                out.insert(k.clone(), eval(e, store, at)?);
            }
            Ok(Value::Struct(out))
        }
    }
}

fn lit_value(l: &Lit) -> Value {
    match l {
        Lit::Bool(b) => Value::Bool(*b),
        Lit::Int(i) => Value::Int(*i),
        Lit::Float(f) => Value::Float(*f),
        Lit::String(s) => Value::Str(s.clone()),
        Lit::Enum(e) => Value::Enum(e.clone()),
    }
}

fn eval_bin(op: BinOp, l: Value, r: Value, at: &str) -> BackendResult<Value> {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => {
            let out = match op {
                BinOp::Add => a.checked_add(b),
                BinOp::Sub => a.checked_sub(b),
                BinOp::Mul => a.checked_mul(b),
                BinOp::Div => {
                    if b == 0 {
                        return Err(BackendError::invalid(
                            "E902",
                            format!("division by zero at {at}"),
                        ));
                    }
                    a.checked_div(b)
                }
                BinOp::Mod => {
                    if b == 0 {
                        return Err(BackendError::invalid(
                            "E902",
                            format!("modulo by zero at {at}"),
                        ));
                    }
                    a.checked_rem(b)
                }
            };
            out.map(Value::Int).ok_or_else(|| {
                BackendError::invalid("E901", format!("integer arithmetic overflow at {at}"))
            })
        }
        (Value::Float(a), Value::Float(b)) => {
            let v = match op {
                BinOp::Add => a + b,
                BinOp::Sub => a - b,
                BinOp::Mul => a * b,
                BinOp::Div => a / b,
                BinOp::Mod => a % b,
            };
            Ok(Value::Float(v))
        }
        (l, r) => Err(BackendError::invalid(
            "E932",
            format!(
                "binary operator requires matching numeric types, found {} and {} at {at}",
                l.canonical(),
                r.canonical()
            ),
        )),
    }
}

fn eval_cmp(op: CmpOp, l: Value, r: Value, at: &str) -> BackendResult<Value> {
    let both_int = matches!((&l, &r), (Value::Int(_), Value::Int(_)));
    let both_float = matches!((&l, &r), (Value::Float(_), Value::Float(_)));
    if both_int || both_float {
        let ord = match (&l, &r) {
            (Value::Int(a), Value::Int(b)) => a.partial_cmp(b),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
            _ => unreachable!(),
        };
        let Some(ord) = ord else {
            return Ok(Value::Bool(false));
        };
        let res = match op {
            CmpOp::Eq => ord == std::cmp::Ordering::Equal,
            CmpOp::Ne => ord != std::cmp::Ordering::Equal,
            CmpOp::Lt => ord == std::cmp::Ordering::Less,
            CmpOp::Le => ord != std::cmp::Ordering::Greater,
            CmpOp::Gt => ord == std::cmp::Ordering::Greater,
            CmpOp::Ge => ord != std::cmp::Ordering::Less,
        };
        return Ok(Value::Bool(res));
    }
    match op {
        CmpOp::Eq => Ok(Value::Bool(l == r)),
        CmpOp::Ne => Ok(Value::Bool(l != r)),
        _ => Err(BackendError::invalid(
            "E932",
            format!(
                "ordered comparison requires numeric types, found {} and {} at {at}",
                l.canonical(),
                r.canonical()
            ),
        )),
    }
}
