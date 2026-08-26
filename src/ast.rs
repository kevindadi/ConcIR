use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, Deserializer, Error};
use serde::{Deserialize, Serialize, Serializer};

use crate::fqn;

// ──────────────────── Top-level ────────────────────

fn default_version() -> String {
    "3.1.0".to_string()
}

/// A complete ConcIR program: a set of [`Module`]s with a single entry FQN.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    pub program: String,
    #[serde(default = "default_version")]
    pub version: String,
    pub modules: Vec<Module>,
    /// Entry function as an FQN: `module::function`.
    pub entry: String,
}

/// Exported / imported names. `provides` uses short names; `requires` uses FQNs.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NameSet {
    #[serde(default)]
    pub resources: Vec<String>,
    #[serde(default)]
    pub functions: Vec<String>,
}

/// One ConcIR module: resources, protection, functions, and a name-resolution contract.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Module {
    pub name: String,
    #[serde(default)]
    pub provides: NameSet,
    #[serde(default)]
    pub requires: NameSet,
    #[serde(default)]
    pub resources: Vec<Resource>,
    #[serde(default)]
    pub protection: Vec<Protection>,
    #[serde(default)]
    pub functions: Vec<Function>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity: Option<i64>,
}

// ──────────────────── BaseType ────────────────────

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArrayDef {
    pub elem: BaseType,
    pub len: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ComplexBaseType {
    Enum(Vec<String>),
    Struct(BTreeMap<String, BaseType>),
    Array(Box<ArrayDef>),
    /// Bounded Int value domain `[lo, hi]`, serialized as `{"Int": [lo, hi]}`.
    /// Keeps counter loops decidable (updates leaving the domain disable the
    /// transition in the CVN).
    BoundedInt {
        lo: i64,
        hi: i64,
    },
}

impl Serialize for ComplexBaseType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ComplexBaseType::Enum(variants) => ("Enum", variants).serialize(serializer),
            ComplexBaseType::Struct(fields) => ("Struct", fields).serialize(serializer),
            ComplexBaseType::Array(def) => ("Array", def).serialize(serializer),
            ComplexBaseType::BoundedInt { lo, hi } => ("Int", (lo, hi)).serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ComplexBaseType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| de::Error::custom("complex base type must be a single-key object"))?;
        if object.len() != 1 {
            return Err(de::Error::custom(
                "complex base type must have exactly one key",
            ));
        }
        let (key, val) = object.iter().next().unwrap();
        match key.as_str() {
            "Enum" => serde_json::from_value(val.clone())
                .map(ComplexBaseType::Enum)
                .map_err(de::Error::custom),
            "Struct" => serde_json::from_value(val.clone())
                .map(ComplexBaseType::Struct)
                .map_err(de::Error::custom),
            "Array" => serde_json::from_value(val.clone())
                .map(ComplexBaseType::Array)
                .map_err(de::Error::custom),
            "Int" => {
                let bounds: Vec<i64> =
                    serde_json::from_value(val.clone()).map_err(de::Error::custom)?;
                if bounds.len() != 2 || bounds[0] > bounds[1] {
                    return Err(de::Error::custom(
                        "bounded Int must be [lo, hi] with lo <= hi",
                    ));
                }
                Ok(ComplexBaseType::BoundedInt {
                    lo: bounds[0],
                    hi: bounds[1],
                })
            }
            other => Err(de::Error::custom(format!(
                "unknown complex base type key: \"{other}\""
            ))),
        }
    }
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
            BaseType::Complex(ComplexBaseType::BoundedInt { lo, hi }) => {
                write!(f, "Int{{{lo}..={hi}}}")
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

/// A typed data-flow declaration: a function parameter or return value.
/// `modeled` controls whether the value is materialized in the CVN variable
/// store (see `doc/syntax.md`). Unmodeled values are codegen-only placeholders
/// and never enter the net.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ParamDecl {
    pub name: String,
    #[serde(rename = "type")]
    pub param_type: BaseType,
    #[serde(default)]
    pub modeled: bool,
}

/// A function-local slot. Same projection flag as [`ParamDecl`], plus optional init.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalDecl {
    pub name: String,
    #[serde(rename = "type")]
    pub local_type: BaseType,
    #[serde(default)]
    pub modeled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Function {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<ParamDecl>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<ParamDecl>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locals: Vec<LocalDecl>,
    /// Basic blocks. Empty body is a nobody function (codegen placeholder).
    #[serde(default)]
    pub body: Vec<Block>,
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

// ──────────────────── Block (MIR-style) ────────────────────

/// One basic block: zero or more data [`Stmt`]s, then exactly one exit —
/// either a [`Call`] (sync / thread / function, with a successor) or a
/// [`Terminator`] (`goto` / `branch` / `switch` / `return`).
#[derive(Debug, Clone, Serialize)]
pub struct Block {
    pub sid: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statements: Vec<Stmt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<Call>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminator: Option<Terminator>,
}

impl<'de> Deserialize<'de> for Block {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Helper {
            sid: String,
            #[serde(default)]
            statements: Vec<Stmt>,
            #[serde(default)]
            call: Option<Call>,
            #[serde(default)]
            terminator: Option<Terminator>,
        }
        let h = Helper::deserialize(deserializer)?;
        match (h.call.is_some(), h.terminator.is_some()) {
            (true, false) | (false, true) => Ok(Block {
                sid: h.sid,
                statements: h.statements,
                call: h.call,
                terminator: h.terminator,
            }),
            (true, true) => Err(D::Error::custom(
                "block must not have both call and terminator",
            )),
            (false, false) => Err(D::Error::custom(
                "block requires exactly one of call or terminator",
            )),
        }
    }
}

/// Data / structured statements (non-control, non-call). Analogous to MIR statements.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Stmt {
    #[serde(rename = "nop")]
    Nop,
    #[serde(rename = "assign_local")]
    AssignLocal { target: String, expr: String },
    #[serde(rename = "read_shared")]
    ReadShared {
        resource: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dst: Option<String>,
    },
    #[serde(rename = "write_shared")]
    WriteShared { resource: String, expr: String },
    #[serde(rename = "abstract_step")]
    AbstractStep {
        #[serde(default)]
        reads: Vec<String>,
        #[serde(default)]
        writes: Vec<String>,
        #[serde(default)]
        desc: String,
    },
    /// Structured loop header: body starts at `body`, break target is `exit`.
    #[serde(rename = "loop")]
    Loop { body: String, exit: String },
}

/// Sync, thread, and function operations. Each (except `select`) names the
/// successor block in `target` — the analogue of MIR `Call { ..., target }`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Call {
    #[serde(rename = "mutex_lock")]
    MutexLock { resource: String, target: String },
    #[serde(rename = "mutex_unlock")]
    MutexUnlock { resource: String, target: String },
    #[serde(rename = "rwlock_read")]
    RwLockRead { resource: String, target: String },
    #[serde(rename = "rwlock_write")]
    RwLockWrite { resource: String, target: String },
    #[serde(rename = "rwlock_unlock")]
    RwLockUnlock { resource: String, target: String },
    #[serde(rename = "channel_send")]
    ChannelSend {
        channel: String,
        value: String,
        target: String,
    },
    #[serde(rename = "channel_recv")]
    ChannelRecv {
        channel: String,
        dst: String,
        target: String,
    },
    #[serde(rename = "condvar_wait")]
    CondvarWait {
        condvar: String,
        lock: String,
        target: String,
    },
    #[serde(rename = "condvar_notify")]
    CondvarNotify { condvar: String, target: String },
    #[serde(rename = "condvar_notify_all")]
    CondvarNotifyAll { condvar: String, target: String },
    #[serde(rename = "semaphore_acquire")]
    SemaphoreAcquire {
        resource: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        count: Option<i64>,
        target: String,
    },
    #[serde(rename = "semaphore_release")]
    SemaphoreRelease {
        resource: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        count: Option<i64>,
        target: String,
    },
    #[serde(rename = "atomic_load")]
    AtomicLoad {
        resource: String,
        dst: String,
        target: String,
    },
    #[serde(rename = "atomic_store")]
    AtomicStore {
        resource: String,
        value: String,
        target: String,
    },
    #[serde(rename = "atomic_cas")]
    AtomicCas {
        resource: String,
        expected: String,
        desired: String,
        dst: String,
        target: String,
    },
    #[serde(rename = "call")]
    Func {
        func: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dst: Option<String>,
        target: String,
    },
    #[serde(rename = "spawn")]
    Spawn {
        func: String,
        #[serde(default)]
        args: Vec<String>,
        handle: String,
        target: String,
    },
    #[serde(rename = "spawn_batch")]
    SpawnBatch {
        func: String,
        count: i64,
        handle: String,
        target: String,
    },
    #[serde(rename = "join")]
    Join { handle: String, target: String },
    #[serde(rename = "join_all")]
    JoinAll { handle: String, target: String },
    #[serde(rename = "async_call")]
    AsyncCall {
        func: String,
        #[serde(default)]
        args: Vec<String>,
        handle: String,
        target: String,
    },
    #[serde(rename = "await")]
    Await { handle: String, target: String },
    #[serde(rename = "select")]
    Select {
        branches: Vec<SelectBranch>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectBranch {
    pub guard: SelectGuard,
    pub target: String,
}

/// Blocking operations allowed as `select` guards.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum SelectGuard {
    #[serde(rename = "channel_recv")]
    ChannelRecv { channel: String, dst: String },
    #[serde(rename = "condvar_wait")]
    CondvarWait { condvar: String, lock: String },
    #[serde(rename = "semaphore_acquire")]
    SemaphoreAcquire { resource: String },
}

/// CFG terminator: the only place `return` appears.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Terminator {
    #[serde(rename = "goto")]
    Goto { target: String },
    #[serde(rename = "branch")]
    Branch {
        cond: String,
        then: String,
        #[serde(rename = "else")]
        else_target: String,
    },
    #[serde(rename = "switch")]
    Switch {
        var: String,
        cases: BTreeMap<String, String>,
        default: String,
    },
    #[serde(rename = "return")]
    Return {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
    },
}

// ──────────────────── Helpers ────────────────────

impl Program {
    pub fn lookup_module(&self, name: &str) -> Option<&Module> {
        self.modules.iter().find(|m| m.name == name)
    }

    /// Visit every basic block: `(module_idx, fn_idx, block_idx, module, function, block)`.
    pub fn walk_blocks<F>(&self, mut visit: F)
    where
        F: FnMut(usize, usize, usize, &Module, &Function, &Block),
    {
        for (mi, m) in self.modules.iter().enumerate() {
            for (fi, f) in m.functions.iter().enumerate() {
                for (si, b) in f.body.iter().enumerate() {
                    visit(mi, fi, si, m, f, b);
                }
            }
        }
    }

    pub fn fn_path(mi: usize, fi: usize) -> String {
        format!("modules[{mi}].functions[{fi}]")
    }

    pub fn block_path(mi: usize, fi: usize, si: usize) -> String {
        format!("modules[{mi}].functions[{fi}].body[{si}]")
    }

    /// Resolve a function name from `from_module` (short or FQN).
    pub fn lookup_function(&self, from_module: &str, name: &str) -> Option<(&Module, &Function)> {
        let (module, entity) = if let Some(pair) = fqn::split_fqn(name) {
            pair
        } else {
            (from_module, name)
        };
        let m = self.lookup_module(module)?;
        let f = m.functions.iter().find(|f| f.name == entity)?;
        Some((m, f))
    }

    pub fn lookup_resource(&self, from_module: &str, name: &str) -> Option<(&Module, &Resource)> {
        let (module, entity) = if let Some(pair) = fqn::split_fqn(name) {
            pair
        } else {
            (from_module, name)
        };
        let m = self.lookup_module(module)?;
        let r = m.resources.iter().find(|r| r.name == entity)?;
        Some((m, r))
    }
}

impl Block {
    pub fn successor_sids(&self) -> Vec<&str> {
        let mut v = Vec::new();
        for stmt in &self.statements {
            if let Stmt::Loop { body, exit } = stmt {
                v.push(body.as_str());
                v.push(exit.as_str());
            }
        }
        if let Some(call) = &self.call {
            v.extend(call.successor_sids());
            return v;
        }
        match &self.terminator {
            Some(Terminator::Goto { target }) => v.push(target),
            Some(Terminator::Branch {
                then, else_target, ..
            }) => {
                v.push(then);
                v.push(else_target);
            }
            Some(Terminator::Switch { cases, default, .. }) => {
                v.extend(cases.values().map(String::as_str));
                v.push(default);
            }
            Some(Terminator::Return { .. }) | None => {}
        }
        v
    }

    pub fn is_return(&self) -> bool {
        matches!(self.terminator, Some(Terminator::Return { .. }))
    }

    pub fn branch_cond(&self) -> Option<&str> {
        match &self.terminator {
            Some(Terminator::Branch { cond, .. }) => Some(cond),
            _ => None,
        }
    }

    pub fn switch(&self) -> Option<(&str, &BTreeMap<String, String>, &str)> {
        match &self.terminator {
            Some(Terminator::Switch {
                var,
                cases,
                default,
            }) => Some((var, cases, default)),
            _ => None,
        }
    }
}

impl Call {
    pub fn successor_sids(&self) -> Vec<&str> {
        match self {
            Call::Select { branches, default } => {
                let mut v: Vec<&str> = branches.iter().map(|b| b.target.as_str()).collect();
                if let Some(d) = default {
                    v.push(d);
                }
                v
            }
            Call::MutexLock { target, .. }
            | Call::MutexUnlock { target, .. }
            | Call::RwLockRead { target, .. }
            | Call::RwLockWrite { target, .. }
            | Call::RwLockUnlock { target, .. }
            | Call::ChannelSend { target, .. }
            | Call::ChannelRecv { target, .. }
            | Call::CondvarWait { target, .. }
            | Call::CondvarNotify { target, .. }
            | Call::CondvarNotifyAll { target, .. }
            | Call::SemaphoreAcquire { target, .. }
            | Call::SemaphoreRelease { target, .. }
            | Call::AtomicLoad { target, .. }
            | Call::AtomicStore { target, .. }
            | Call::AtomicCas { target, .. }
            | Call::Func { target, .. }
            | Call::Spawn { target, .. }
            | Call::SpawnBatch { target, .. }
            | Call::Join { target, .. }
            | Call::JoinAll { target, .. }
            | Call::AsyncCall { target, .. }
            | Call::Await { target, .. } => vec![target],
        }
    }

    pub fn callee_func(&self) -> Option<&str> {
        match self {
            Call::Func { func, .. }
            | Call::Spawn { func, .. }
            | Call::SpawnBatch { func, .. }
            | Call::AsyncCall { func, .. } => Some(func),
            _ => None,
        }
    }

    pub fn resource_name(&self) -> Option<&str> {
        match self {
            Call::MutexLock { resource, .. }
            | Call::MutexUnlock { resource, .. }
            | Call::RwLockRead { resource, .. }
            | Call::RwLockWrite { resource, .. }
            | Call::RwLockUnlock { resource, .. }
            | Call::SemaphoreAcquire { resource, .. }
            | Call::SemaphoreRelease { resource, .. }
            | Call::AtomicLoad { resource, .. }
            | Call::AtomicStore { resource, .. }
            | Call::AtomicCas { resource, .. } => Some(resource),
            Call::ChannelSend { channel, .. } | Call::ChannelRecv { channel, .. } => Some(channel),
            Call::CondvarWait { condvar, .. }
            | Call::CondvarNotify { condvar, .. }
            | Call::CondvarNotifyAll { condvar, .. } => Some(condvar),
            _ => None,
        }
    }

    pub fn is_lock_acquire(&self) -> Option<&str> {
        match self {
            Call::MutexLock { resource, .. }
            | Call::RwLockRead { resource, .. }
            | Call::RwLockWrite { resource, .. } => Some(resource),
            _ => None,
        }
    }

    pub fn is_lock_release(&self) -> Option<&str> {
        match self {
            Call::MutexUnlock { resource, .. } | Call::RwLockUnlock { resource, .. } => {
                Some(resource)
            }
            _ => None,
        }
    }

    pub fn is_await_like(&self) -> bool {
        matches!(self, Call::Await { .. })
    }

    pub fn is_join_like(&self) -> bool {
        matches!(
            self,
            Call::Join { .. } | Call::JoinAll { .. } | Call::Await { .. }
        )
    }
}

impl Stmt {
    pub fn shared_var_access(&self) -> Option<(&str, bool)> {
        match self {
            Stmt::ReadShared { resource, .. } => Some((resource, false)),
            Stmt::WriteShared { resource, .. } => Some((resource, true)),
            _ => None,
        }
    }
}
