use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// ──────────────────── Top-level ────────────────────

/// Optional reachability goal for repair / verification (marking + variable values).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BusinessGoal {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
    #[serde(default)]
    pub marking: BTreeMap<String, u32>,
    #[serde(default)]
    pub variables: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    pub program: String,
    pub resources: Vec<Resource>,
    pub protection: Vec<Protection>,
    pub functions: Vec<Function>,
    pub entry: String,
    #[serde(default)]
    pub goals: Vec<BusinessGoal>,
}

// ──────────────────── Resources ────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct Protection {
    pub var: String,
    pub lock: String,
}

// ──────────────────── Function ────────────────────

/// A typed data-flow declaration: a function parameter, local, or return
/// value. `modeled` controls whether the value is materialized in the CVN
/// variable store (see `doc/syntax.md`). Unmodeled values are codegen-only
/// placeholders and never enter the net.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ParamDecl {
    pub name: String,
    #[serde(rename = "type")]
    pub param_type: BaseType,
    #[serde(default)]
    pub modeled: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Function {
    pub name: String,
    pub kind: String,
    /// Source module when the program was assembled from modular ConcIR
    /// fragments. Used for cross-module repair attribution; absent for
    /// single-fragment programs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// Typed parameters, bound at `call` sites (modeled ones) or carried for
    /// codegen only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<ParamDecl>,
    /// Optional typed return value, produced by `["return", <expr>]` and
    /// captured by a caller's `call` out-var (modeled ones).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<ParamDecl>,
    /// Statement body. An empty body marks a "nobody" function: it contains no
    /// control flow and no callsites, so it is modeled as a trivial skeleton
    /// (entry → single transition → return) when it is referenced by a
    /// `spawn`/`join`/`call`. Optional [`Function::effects`] attach
    /// computation hints (reads/writes) to that single transition.
    #[serde(default)]
    pub body: Vec<Statement>,
    /// Optional computation hint for a body-less ("nobody") function. Reads and
    /// writes are annotated on the single transition that bridges the
    /// function's entry and return places, so codegen still sees the data
    /// footprint without modeling internal control flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<FunctionEffects>,
}

/// Data-footprint hint for a body-less function.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionEffects {
    #[serde(default)]
    pub reads: Vec<String>,
    #[serde(default)]
    pub writes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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
    /// Synchronous call. `extras` holds the raw extra array elements after the
    /// callee name; validation interprets them as (optional out-var, then
    /// arguments) based on the callee's signature.
    Call {
        target: String,
        extras: Vec<String>,
    },
    /// Function return, optionally carrying a value expression that binds a
    /// modeled `returns` declaration: `"return"` or `["return", "<expr>"]`.
    Return(Option<String>),
    Nop,
}

impl Serialize for Op {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Op::Return(None) => serializer.serialize_str("return"),
            Op::Return(Some(value)) => ("return", value).serialize(serializer),
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
            Op::Call { target, extras } => {
                let mut v: Vec<&str> = vec!["call", target];
                v.extend(extras.iter().map(String::as_str));
                v.serialize(serializer)
            }
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
                    "return" => Ok(Op::Return(None)),
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
                        reject_extra_elements(&mut seq, "spawn")?;
                        Ok(Op::Spawn(name))
                    }
                    "spawn_async" => {
                        let name: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(1, &"function name"))?;
                        reject_extra_elements(&mut seq, "spawn_async")?;
                        Ok(Op::SpawnAsync(name))
                    }
                    "join" => {
                        let name: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(1, &"function name"))?;
                        reject_extra_elements(&mut seq, "join")?;
                        Ok(Op::Join(name))
                    }
                    "await" => {
                        let name: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(1, &"function name"))?;
                        reject_extra_elements(&mut seq, "await")?;
                        Ok(Op::Await(name))
                    }
                    "call" => {
                        let target: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(1, &"function name"))?;
                        let mut extras = Vec::new();
                        while let Some(a) = seq.next_element::<String>()? {
                            extras.push(a);
                        }
                        Ok(Op::Call { target, extras })
                    }
                    "return" => {
                        let value: Option<String> = seq.next_element()?;
                        reject_extra_elements(&mut seq, "return")?;
                        Ok(Op::Return(value))
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
                        reject_extra_elements(&mut seq, "next")?;
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
                        reject_extra_elements(&mut seq, "branch")?;
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
                        reject_extra_elements(&mut seq, "switch")?;
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

fn reject_extra_elements<'de, A: SeqAccess<'de>>(
    seq: &mut A,
    shape: &str,
) -> Result<(), A::Error> {
    if seq.next_element::<de::IgnoredAny>()?.is_some() {
        return Err(de::Error::custom(format!(
            "{shape} tuple has extra elements"
        )));
    }
    Ok(())
}

// ──────────────────── Helpers ────────────────────

impl Op {
    pub fn target_name(&self) -> Option<&str> {
        match self {
            Op::Spawn(n) | Op::SpawnAsync(n) | Op::Join(n) | Op::Await(n) => Some(n),
            Op::Call { target, .. } => Some(target),
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
