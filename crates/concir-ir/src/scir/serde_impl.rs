use serde::de::{self, Deserializer};
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};

use crate::ast::Op;

use super::StructuredStmt;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLockGuard {
    #[serde(default)]
    id: String,
    #[serde(default)]
    req: Vec<String>,
    #[allow(dead_code)]
    kind: String,
    resource: String,
    #[serde(default)]
    body: Vec<StructuredStmt>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIfElse {
    #[serde(default)]
    id: String,
    #[serde(default)]
    req: Vec<String>,
    #[allow(dead_code)]
    kind: String,
    cond: String,
    #[serde(default)]
    then_body: Vec<StructuredStmt>,
    #[serde(default)]
    else_body: Vec<StructuredStmt>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWhileLoop {
    #[serde(default)]
    id: String,
    #[serde(default)]
    req: Vec<String>,
    #[allow(dead_code)]
    kind: String,
    cond: String,
    #[serde(default)]
    body: Vec<StructuredStmt>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWhileWait {
    #[serde(default)]
    id: String,
    #[serde(default)]
    req: Vec<String>,
    #[allow(dead_code)]
    kind: String,
    cond: String,
    condvar: String,
    lock: String,
    #[serde(default)]
    body: Vec<StructuredStmt>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStmtWrapper {
    #[serde(default)]
    id: String,
    #[serde(default)]
    req: Vec<String>,
    #[serde(default)]
    milestone: Option<String>,
    #[allow(dead_code)]
    kind: String,
    #[serde(default)]
    op: Option<Op>,
    #[serde(default)]
    stmt: Option<Op>,
}

impl<'de> Deserialize<'de> for StructuredStmt {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let obj = value
            .as_object()
            .ok_or_else(|| de::Error::custom("structured statement must be a JSON object"))?;
        let kind = obj
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| de::Error::custom("structured statement missing string 'kind'"))?;

        match kind {
            "lock_guard" | "rwlock_read_guard" | "rwlock_write_guard" => {
                let raw: RawLockGuard =
                    serde_json::from_value(value).map_err(de::Error::custom)?;
                Ok(match raw.kind.as_str() {
                    "lock_guard" => StructuredStmt::LockGuard {
                        id: raw.id,
                        req: raw.req,
                        resource: raw.resource,
                        body: raw.body,
                    },
                    "rwlock_read_guard" => StructuredStmt::RwLockReadGuard {
                        id: raw.id,
                        req: raw.req,
                        resource: raw.resource,
                        body: raw.body,
                    },
                    _ => StructuredStmt::RwLockWriteGuard {
                        id: raw.id,
                        req: raw.req,
                        resource: raw.resource,
                        body: raw.body,
                    },
                })
            }
            "if_else" => {
                let raw: RawIfElse = serde_json::from_value(value).map_err(de::Error::custom)?;
                Ok(StructuredStmt::IfElse {
                    id: raw.id,
                    req: raw.req,
                    cond: raw.cond,
                    then_body: raw.then_body,
                    else_body: raw.else_body,
                })
            }
            "while_loop" => {
                let raw: RawWhileLoop =
                    serde_json::from_value(value).map_err(de::Error::custom)?;
                Ok(StructuredStmt::WhileLoop {
                    id: raw.id,
                    req: raw.req,
                    cond: raw.cond,
                    body: raw.body,
                })
            }
            "while_wait" => {
                let raw: RawWhileWait =
                    serde_json::from_value(value).map_err(de::Error::custom)?;
                Ok(StructuredStmt::WhileWait {
                    id: raw.id,
                    req: raw.req,
                    cond: raw.cond,
                    condvar: raw.condvar,
                    lock: raw.lock,
                    body: raw.body,
                })
            }
            "stmt" => {
                let raw: RawStmtWrapper =
                    serde_json::from_value(value).map_err(de::Error::custom)?;
                let op = match (raw.op, raw.stmt) {
                    (Some(op), None) | (None, Some(op)) => op,
                    (Some(_), Some(_)) => {
                        return Err(de::Error::custom(
                            "structured 'stmt' cannot specify both 'op' and 'stmt'",
                        ));
                    }
                    (None, None) => {
                        return Err(de::Error::custom(
                            "structured 'stmt' requires an 'op' field",
                        ));
                    }
                };
                Ok(StructuredStmt::Stmt {
                    id: raw.id,
                    req: raw.req,
                    milestone: raw.milestone,
                    op,
                })
            }
            // Also accept any flat `Op` directly inside `structured_body`,
            // extracting optional `id`, `req`, and `milestone` metadata.
            _ => {
                let mut map = obj.clone();
                let id = match map.remove("id") {
                    Some(v) => {
                        // Note: `seq_hole` uses `"id"` as the hole identifier!
                        // When a flat `seq_hole` appears directly inside `structured_body`,
                        // keep `"id"` for `Op::SeqHole` and reuse it as the structured id.
                        let id_str: String =
                            serde_json::from_value(v.clone()).map_err(de::Error::custom)?;
                        if kind == "seq_hole" {
                            map.insert("id".to_string(), v);
                        }
                        id_str
                    }
                    None => String::new(),
                };
                let req: Vec<String> = match map.remove("req") {
                    Some(v) => serde_json::from_value(v).map_err(de::Error::custom)?,
                    None => Vec::new(),
                };
                let milestone: Option<String> = match map.remove("milestone") {
                    Some(v) => serde_json::from_value(v).map_err(de::Error::custom)?,
                    None => None,
                };
                let op: Op = serde_json::from_value(serde_json::Value::Object(map))
                    .map_err(de::Error::custom)?;
                Ok(StructuredStmt::Stmt {
                    id,
                    req,
                    milestone,
                    op,
                })
            }
        }
    }
}

impl Serialize for StructuredStmt {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        match self {
            StructuredStmt::Stmt {
                id,
                req,
                milestone,
                op,
            } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("kind", "stmt")?;
                map.serialize_entry("op", op)?;
                if !req.is_empty() {
                    map.serialize_entry("req", req)?;
                }
                if let Some(m) = milestone {
                    map.serialize_entry("milestone", m)?;
                }
            }
            StructuredStmt::LockGuard {
                id,
                req,
                resource,
                body,
            } => {
                serialize_guard(&mut map, id, "lock_guard", resource, body, req)?;
            }
            StructuredStmt::RwLockReadGuard {
                id,
                req,
                resource,
                body,
            } => {
                serialize_guard(&mut map, id, "rwlock_read_guard", resource, body, req)?;
            }
            StructuredStmt::RwLockWriteGuard {
                id,
                req,
                resource,
                body,
            } => {
                serialize_guard(&mut map, id, "rwlock_write_guard", resource, body, req)?;
            }
            StructuredStmt::IfElse {
                id,
                req,
                cond,
                then_body,
                else_body,
            } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("kind", "if_else")?;
                map.serialize_entry("cond", cond)?;
                map.serialize_entry("then_body", then_body)?;
                if !else_body.is_empty() {
                    map.serialize_entry("else_body", else_body)?;
                }
                if !req.is_empty() {
                    map.serialize_entry("req", req)?;
                }
            }
            StructuredStmt::WhileLoop {
                id,
                req,
                cond,
                body,
            } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("kind", "while_loop")?;
                map.serialize_entry("cond", cond)?;
                map.serialize_entry("body", body)?;
                if !req.is_empty() {
                    map.serialize_entry("req", req)?;
                }
            }
            StructuredStmt::WhileWait {
                id,
                req,
                cond,
                condvar,
                lock,
                body,
            } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("kind", "while_wait")?;
                map.serialize_entry("cond", cond)?;
                map.serialize_entry("condvar", condvar)?;
                map.serialize_entry("lock", lock)?;
                if !body.is_empty() {
                    map.serialize_entry("body", body)?;
                }
                if !req.is_empty() {
                    map.serialize_entry("req", req)?;
                }
            }
        }
        map.end()
    }
}

fn serialize_guard<M: SerializeMap>(
    map: &mut M,
    id: &str,
    kind: &str,
    resource: &str,
    body: &[StructuredStmt],
    req: &[String],
) -> Result<(), M::Error> {
    map.serialize_entry("id", id)?;
    map.serialize_entry("kind", kind)?;
    map.serialize_entry("resource", resource)?;
    map.serialize_entry("body", body)?;
    if !req.is_empty() {
        map.serialize_entry("req", req)?;
    }
    Ok(())
}
