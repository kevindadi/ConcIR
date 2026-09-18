//! Runtime values for the backend.
//!
//! Values are the payloads of the operational store and of Petri-net tokens.
//! They implement `Eq` and `Hash` (floats by their IEEE bit pattern) so a
//! complete state can be deduplicated by value, never by a loose hash.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use crate::ast::{BaseType, ComplexBaseType};
use serde_json::Value as Json;

#[derive(Debug, Clone)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Enum(String),
    Struct(BTreeMap<String, Value>),
    Array(Vec<Value>),
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Enum(a), Value::Enum(b)) => a == b,
            (Value::Struct(a), Value::Struct(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Value {}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Value::Bool(v) => {
                0u8.hash(state);
                v.hash(state);
            }
            Value::Int(v) => {
                1u8.hash(state);
                v.hash(state);
            }
            Value::Float(v) => {
                2u8.hash(state);
                v.to_bits().hash(state);
            }
            Value::Str(v) => {
                3u8.hash(state);
                v.hash(state);
            }
            Value::Enum(v) => {
                4u8.hash(state);
                v.hash(state);
            }
            Value::Struct(fields) => {
                5u8.hash(state);
                fields.hash(state);
            }
            Value::Array(items) => {
                6u8.hash(state);
                items.hash(state);
            }
        }
    }
}

impl Value {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Enum(s) => Some(s),
            _ => None,
        }
    }

    /// Canonical, stable text for diagnostics and differential comparison.
    pub fn canonical(&self) -> String {
        let mut out = String::new();
        write_canonical(self, &mut out);
        out
    }
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Float(f) => {
            out.push_str("f");
            out.push_str(&f.to_bits().to_string());
        }
        Value::Str(s) => {
            // JSON-escape so the display text is unambiguous.
            out.push_str(&serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\"")));
        }
        Value::Enum(e) => {
            out.push('#');
            out.push_str(e);
        }
        Value::Struct(fields) => {
            out.push('{');
            let mut first = true;
            for (k, val) in fields {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&serde_json::to_string(k).unwrap_or_else(|_| format!("\"{k}\"")));
                out.push(':');
                write_canonical(val, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, val) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(val, out);
            }
            out.push(']');
        }
    }
}

impl Value {
    /// An unambiguous, type-tagged, length-prefixed encoding used as the
    /// *semantic* state key. Unlike [`Value::canonical`] it never relies on
    /// delimiters inside payloads: every string and field name is prefixed by
    /// its byte length, and every value is tagged by its variant. Equal keys
    /// therefore imply equal values and equal predicate truth.
    pub fn key(&self) -> String {
        let mut out = String::new();
        self.encode_key(&mut out);
        out
    }

    fn encode_key(&self, out: &mut String) {
        match self {
            Value::Bool(b) => {
                out.push_str("B");
                out.push(if *b { '1' } else { '0' });
            }
            Value::Int(i) => {
                out.push('I');
                out.push_str(&i.to_string());
                out.push(';');
            }
            Value::Float(f) => {
                out.push('F');
                out.push_str(&f.to_bits().to_string());
                out.push(';');
            }
            Value::Str(s) => {
                out.push('T');
                out.push_str(&s.len().to_string());
                out.push(':');
                out.push_str(s);
            }
            Value::Enum(e) => {
                out.push('E');
                out.push_str(&e.len().to_string());
                out.push(':');
                out.push_str(e);
            }
            Value::Struct(fields) => {
                out.push('S');
                out.push_str(&fields.len().to_string());
                out.push('[');
                for (k, v) in fields {
                    out.push_str(&k.len().to_string());
                    out.push(':');
                    out.push_str(k);
                    out.push('=');
                    v.encode_key(out);
                    out.push(';');
                }
                out.push(']');
            }
            Value::Array(items) => {
                out.push('A');
                out.push_str(&items.len().to_string());
                out.push('[');
                for v in items {
                    v.encode_key(out);
                    out.push(';');
                }
                out.push(']');
            }
        }
    }
}

/// True if `v` is inside the declared domain of `ty`. Recurses through
/// `Struct` fields, `Array` length and elements, and `Enum` membership, and
/// checks primitives exactly, so a composite value cannot smuggle a bounded
/// member outside its domain. Used to disable a transition whose update would
/// leave the finite domain.
pub fn within_type(v: &Value, ty: &BaseType) -> bool {
    match ty {
        BaseType::Primitive(p) => match p.as_str() {
            "Bool" => matches!(v, Value::Bool(_)),
            "Int" => matches!(v, Value::Int(_)),
            "Float" => matches!(v, Value::Float(_)),
            "String" => matches!(v, Value::Str(_)),
            // A resolved named type is never a bare primitive; an unknown
            // primitive falls through to "no constraint".
            _ => true,
        },
        BaseType::Complex(c) => match c {
            ComplexBaseType::BoundedInt { lo, hi } => {
                matches!(v, Value::Int(i) if i >= lo && i <= hi)
            }
            ComplexBaseType::Enum(variants) => {
                matches!(v, Value::Enum(e) if variants.iter().any(|x| x == e))
            }
            ComplexBaseType::Struct(fields) => match v {
                Value::Struct(m) => {
                    m.len() == fields.len()
                        && fields
                            .iter()
                            .all(|(k, t)| m.get(k).map(|x| within_type(x, t)).unwrap_or(false))
                }
                _ => false,
            },
            ComplexBaseType::Array(def) => match v {
                Value::Array(items) => {
                    items.len() == def.len.max(0) as usize
                        && items.iter().all(|x| within_type(x, &def.elem))
                }
                _ => false,
            },
        },
    }
}

/// The default value of a declared type.
pub fn default_value(ty: &BaseType) -> Value {
    match ty {
        BaseType::Primitive(p) => match p.as_str() {
            "Bool" => Value::Bool(false),
            "Int" => Value::Int(0),
            "Float" => Value::Float(0.0),
            "String" => Value::Str(String::new()),
            _ => Value::Int(0),
        },
        BaseType::Complex(c) => match c {
            ComplexBaseType::Enum(variants) => {
                Value::Enum(variants.first().cloned().unwrap_or_default())
            }
            ComplexBaseType::Struct(fields) => Value::Struct(
                fields
                    .iter()
                    .map(|(k, t)| (k.clone(), default_value(t)))
                    .collect(),
            ),
            ComplexBaseType::Array(def) => Value::Array(
                (0..def.len.max(0))
                    .map(|_| default_value(&def.elem))
                    .collect(),
            ),
            ComplexBaseType::BoundedInt { lo, hi } => Value::Int(0i64.clamp(*lo, *hi)),
        },
    }
}

/// Convert a JSON literal into a typed value.
pub fn from_json(json: &Json, ty: &BaseType) -> Result<Value, String> {
    match ty {
        BaseType::Primitive(p) => match p.as_str() {
            "Bool" => json
                .as_bool()
                .map(Value::Bool)
                .ok_or_else(|| format!("expected Bool for init, found {json}")),
            "Int" => json
                .as_i64()
                .map(Value::Int)
                .ok_or_else(|| format!("expected Int for init, found {json}")),
            "Float" => json
                .as_f64()
                .map(Value::Float)
                .ok_or_else(|| format!("expected Float for init, found {json}")),
            "String" => json
                .as_str()
                .map(|s| Value::Str(s.to_string()))
                .ok_or_else(|| format!("expected String for init, found {json}")),
            other => Err(format!("unsupported init type '{other}'")),
        },
        BaseType::Complex(c) => match c {
            ComplexBaseType::BoundedInt { lo, hi } => {
                let i = json
                    .as_i64()
                    .ok_or_else(|| format!("expected Int for init, found {json}"))?;
                if i < *lo || i > *hi {
                    return Err(format!("init {i} outside bounded Int [{lo}, {hi}]"));
                }
                Ok(Value::Int(i))
            }
            ComplexBaseType::Enum(variants) => {
                let s = json
                    .as_str()
                    .ok_or_else(|| format!("expected enum variant string, found {json}"))?;
                if !variants.iter().any(|v| v == s) {
                    return Err(format!("'{s}' is not a variant of the enum type"));
                }
                Ok(Value::Enum(s.to_string()))
            }
            ComplexBaseType::Struct(fields) => {
                let obj = json
                    .as_object()
                    .ok_or_else(|| format!("expected object for struct init, found {json}"))?;
                let mut out = BTreeMap::new();
                for (k, t) in fields {
                    let field = obj
                        .get(k)
                        .ok_or_else(|| format!("struct init missing field '{k}'"))?;
                    out.insert(k.clone(), from_json(field, t)?);
                }
                Ok(Value::Struct(out))
            }
            ComplexBaseType::Array(def) => {
                let arr = json
                    .as_array()
                    .ok_or_else(|| format!("expected array for init, found {json}"))?;
                if arr.len() as i64 != def.len {
                    return Err(format!(
                        "array init has {} elements, expected {}",
                        arr.len(),
                        def.len
                    ));
                }
                let mut out = Vec::new();
                for item in arr {
                    out.push(from_json(item, &def.elem)?);
                }
                Ok(Value::Array(out))
            }
        },
    }
}
