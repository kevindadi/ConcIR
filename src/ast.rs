use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// ──────────────────── Top-level ────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Program {
    pub program: String,
    pub resources: Vec<Resource>,
    pub protection: Vec<Protection>,
    pub functions: Vec<Function>,
    #[serde(default)]
    pub fn_summaries: Vec<FnSummary>,
    pub entry: String,
}

// ──────────────────── Resources ────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Resource {
    pub name: String,
    pub kind: String,
    #[serde(rename = "type")]
    pub res_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<BaseType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init: Option<serde_json::Value>,
}

// ──────────────────── BaseType ────────────────────

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ArrayDef {
    pub elem: BaseType,
    pub len: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub enum ComplexBaseType {
    Enum(Vec<String>),
    Struct(BTreeMap<String, BaseType>),
    Array(Box<ArrayDef>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum BaseType {
    Primitive(String),
    Complex(ComplexBaseType),
}

impl Serialize for BaseType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            BaseType::Primitive(s) => serializer.serialize_str(s),
            BaseType::Complex(c) => c.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for BaseType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::String(s) => Ok(BaseType::Primitive(s)),
            serde_json::Value::Object(_) => {
                let complex: ComplexBaseType =
                    serde_json::from_value(value).map_err(de::Error::custom)?;
                Ok(BaseType::Complex(complex))
            }
            _ => Err(de::Error::custom("base type must be a string or object")),
        }
    }
}

impl fmt::Display for BaseType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BaseType::Primitive(s) => write!(f, "{s}"),
            BaseType::Complex(ComplexBaseType::Enum(variants)) => {
                write!(f, "Enum{{{}}}", variants.join(", "))
            }
            BaseType::Complex(ComplexBaseType::Struct(fields)) => {
                let parts: Vec<String> = fields.iter().map(|(k, v)| format!("{k}: {v}")).collect();
                write!(f, "Struct{{{}}}", parts.join(", "))
            }
            BaseType::Complex(ComplexBaseType::Array(ref def)) => {
                write!(f, "Array<{}, {}>", def.elem, def.len)
            }
        }
    }
}

// ──────────────────── Protection ────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Protection {
    pub var: String,
    pub lock: String,
}

// ──────────────────── Function ────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Function {
    pub name: String,
    pub kind: String,
    pub body: Vec<Statement>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Statement {
    pub sid: String,
    pub op: Op,
    pub transfer: Transfer,
}

// ──────────────────── Operations ────────────────────

#[derive(Debug, Clone)]
pub enum Op {
    ResOp {
        resource: String,
        action: String,
        args: Vec<String>,
    },
    Spawn(String),
    SpawnAsync(String),
    Join(String),
    Await(String),
    Call(String),
    Return,
    Nop,
}

impl Serialize for Op {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Op::Return => serializer.serialize_str("return"),
            Op::Nop => serializer.serialize_str("nop"),
            Op::ResOp {
                resource,
                action,
                args,
            } => {
                let mut v: Vec<&str> = vec!["res_op", resource, action];
                for a in args {
                    v.push(a);
                }
                v.serialize(serializer)
            }
            Op::Spawn(f) => ("spawn", f).serialize(serializer),
            Op::SpawnAsync(f) => ("spawn_async", f).serialize(serializer),
            Op::Join(f) => ("join", f).serialize(serializer),
            Op::Await(f) => ("await", f).serialize(serializer),
            Op::Call(f) => ("call", f).serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Op {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OpVisitor;

        impl<'de> Visitor<'de> for OpVisitor {
            type Value = Op;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("\"return\" or an array like [\"res_op\", ...]")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Op, E> {
                match v {
                    "return" => Ok(Op::Return),
                    "nop" => Ok(Op::Nop),
                    _ => Err(E::custom(format!("unknown op string: \"{v}\""))),
                }
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Op, A::Error> {
                let op_type: String = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(0, &"at least 1 element"))?;

                match op_type.as_str() {
                    "res_op" => {
                        let resource: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(1, &"resource name"))?;
                        let action: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(2, &"action name"))?;
                        let mut args = Vec::new();
                        while let Some(a) = seq.next_element::<String>()? {
                            args.push(a);
                        }
                        Ok(Op::ResOp {
                            resource,
                            action,
                            args,
                        })
                    }
                    "spawn" => {
                        let name: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(1, &"function name"))?;
                        Ok(Op::Spawn(name))
                    }
                    "spawn_async" => {
                        let name: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(1, &"function name"))?;
                        Ok(Op::SpawnAsync(name))
                    }
                    "join" => {
                        let name: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(1, &"function name"))?;
                        Ok(Op::Join(name))
                    }
                    "await" => {
                        let name: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(1, &"function name"))?;
                        Ok(Op::Await(name))
                    }
                    "call" => {
                        let name: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(1, &"function name"))?;
                        Ok(Op::Call(name))
                    }
                    other => Err(de::Error::custom(format!("unknown op type: \"{other}\""))),
                }
            }
        }

        deserializer.deserialize_any(OpVisitor)
    }
}

// ──────────────────── Transfer ────────────────────

#[derive(Debug, Clone)]
pub enum Transfer {
    Next(String),
    Branch {
        cond: String,
        true_target: String,
        false_target: String,
    },
    Switch {
        var: String,
        cases: Vec<(String, String)>,
    },
    Return,
}

impl Serialize for Transfer {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Transfer::Return => serializer.serialize_str("return"),
            Transfer::Next(sid) => ("next", sid).serialize(serializer),
            Transfer::Branch {
                cond,
                true_target,
                false_target,
            } => ("branch", cond, true_target, false_target).serialize(serializer),
            Transfer::Switch { var, cases } => {
                use serde::ser::SerializeSeq;
                let map: BTreeMap<&str, &str> = cases
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                let mut seq = serializer.serialize_seq(Some(3))?;
                seq.serialize_element("switch")?;
                seq.serialize_element(var)?;
                seq.serialize_element(&map)?;
                seq.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Transfer {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TransferVisitor;

        impl<'de> Visitor<'de> for TransferVisitor {
            type Value = Transfer;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("\"return\" or an array like [\"next\", \"s1\"]")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Transfer, E> {
                if v == "return" {
                    Ok(Transfer::Return)
                } else {
                    Err(E::custom(format!("unknown transfer string: \"{v}\"")))
                }
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Transfer, A::Error> {
                let kind: String = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(0, &"at least 1 element"))?;

                match kind.as_str() {
                    "next" => {
                        let sid: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(1, &"target sid"))?;
                        Ok(Transfer::Next(sid))
                    }
                    "branch" => {
                        let cond: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(1, &"condition"))?;
                        let true_target: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(2, &"true target sid"))?;
                        let false_target: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(3, &"false target sid"))?;
                        Ok(Transfer::Branch {
                            cond,
                            true_target,
                            false_target,
                        })
                    }
                    "switch" => {
                        let var: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(1, &"variable name"))?;
                        let map: BTreeMap<String, String> = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(2, &"case mapping"))?;
                        let cases: Vec<(String, String)> = map.into_iter().collect();
                        Ok(Transfer::Switch { var, cases })
                    }
                    other => Err(de::Error::custom(format!(
                        "unknown transfer type: \"{other}\""
                    ))),
                }
            }
        }

        deserializer.deserialize_any(TransferVisitor)
    }
}

// ──────────────────── FnSummary ────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FnSummary {
    pub name: String,
    pub reads: Vec<String>,
    pub writes: Vec<String>,
    pub callees: Vec<String>,
    pub has_concurrency: bool,
}

// ──────────────────── Helpers ────────────────────

impl Op {
    pub fn target_name(&self) -> Option<&str> {
        match self {
            Op::Spawn(n) | Op::SpawnAsync(n) | Op::Join(n) | Op::Await(n) | Op::Call(n) => Some(n),
            _ => None,
        }
    }
}

impl Transfer {
    pub fn target_sids(&self) -> Vec<&str> {
        match self {
            Transfer::Next(s) => vec![s],
            Transfer::Branch {
                true_target,
                false_target,
                ..
            } => vec![true_target, false_target],
            Transfer::Switch { cases, .. } => cases.iter().map(|(_, s)| s.as_str()).collect(),
            Transfer::Return => vec![],
        }
    }
}
