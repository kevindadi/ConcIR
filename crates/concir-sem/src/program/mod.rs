mod lower_body;
mod lower_decl;

use std::collections::BTreeMap;

use concir_ir::ast::{BaseType, Program};
use concir_ir::fqn;
use concir_ir::typedef::TypeEnv;

use crate::eval::LExpr;
use crate::ids::{FunctionId, ModuleId, ResourceId, SlotRef, StatementId};
use crate::outcome::{location_of, BackendError, BackendResult, Unsupported};
use crate::value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResKind {
    Var,
    Atomic,
    Mutex,
    RwLock,
    Condvar,
    Semaphore,
    Channel,
}

#[derive(Debug, Clone)]
pub struct SemTypeDef {
    pub module: ModuleId,
    pub name: String,
    /// Fully resolved type (aliases expanded).
    pub ty: BaseType,
    /// Original declared AST type (preserving named type references).
    pub raw_ty: BaseType,
}

#[derive(Debug, Clone)]
pub struct SemResource {
    pub id: ResourceId,
    pub module: ModuleId,
    pub name: String,
    pub kind: ResKind,
    /// Payload / value type for Var, Atomic, Channel, and optional payload for Mutex/RwLock.
    pub ty: Option<BaseType>,
    /// Original declared AST `base` type (preserving named TypeDef references for codegen).
    pub raw_ty: Option<BaseType>,
    pub capacity: usize,
    pub permits: i64,
    pub init: Option<Value>,
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotClass {
    Param,
    Local,
    Return,
}

#[derive(Debug, Clone)]
pub struct SemSlot {
    pub name: String,
    pub ty: BaseType,
    /// Original declared AST type (preserving named TypeDef references for codegen).
    pub raw_ty: BaseType,
    pub modeled: bool,
    pub class: SlotClass,
    pub init: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct SemStmt {
    pub id: StatementId,
    pub sid: String,
    pub req: Vec<String>,
    pub milestone: Option<String>,
    pub op: SemOp,
}

#[derive(Debug, Clone)]
pub struct SemModeledUpdate {
    pub target: SlotRef,
    pub expr: LExpr,
}

#[derive(Debug, Clone)]
pub enum SemOp {
    Nop,
    AssignLocal {
        target: SlotRef,
        expr: LExpr,
    },
    ReadShared {
        resource: ResourceId,
        dst: Option<SlotRef>,
    },
    WriteShared {
        resource: ResourceId,
        expr: LExpr,
    },
    AtomicLoad {
        resource: ResourceId,
        dst: SlotRef,
    },
    AtomicStore {
        resource: ResourceId,
        value: LExpr,
    },
    AtomicCas {
        resource: ResourceId,
        expected: LExpr,
        desired: LExpr,
        dst: SlotRef,
    },
    MutexLock {
        resource: ResourceId,
    },
    MutexUnlock {
        resource: ResourceId,
    },
    RwLockRead {
        resource: ResourceId,
    },
    RwLockWrite {
        resource: ResourceId,
    },
    RwLockUnlock {
        resource: ResourceId,
    },
    ChannelSend {
        channel: ResourceId,
        value: LExpr,
    },
    ChannelRecv {
        channel: ResourceId,
        dst: SlotRef,
    },
    CondvarWait {
        condvar: ResourceId,
        lock: ResourceId,
    },
    CondvarNotify {
        condvar: ResourceId,
    },
    CondvarNotifyAll {
        condvar: ResourceId,
    },
    SemaphoreAcquire {
        resource: ResourceId,
        count: i64,
    },
    SemaphoreRelease {
        resource: ResourceId,
        count: i64,
    },
    Call {
        func: FunctionId,
        args: Vec<LExpr>,
        dst: Option<SlotRef>,
    },
    Spawn {
        func: FunctionId,
        handle: String,
    },
    Scope {
        funcs: Vec<FunctionId>,
    },
    Join {
        handle: String,
    },
    Goto {
        target: usize,
    },
    Branch {
        cond: LExpr,
        then: usize,
        else_target: usize,
    },
    Switch {
        var: LExpr,
        cases: BTreeMap<String, usize>,
        default: usize,
    },
    Return {
        value: Option<LExpr>,
    },
    SeqHole {
        id: String,
        desc: String,
        reads: Vec<String>,
        writes: Vec<String>,
        updates_modeled: Vec<SemModeledUpdate>,
        expected_type: String,
        req: Vec<String>,
    },
    /// A construct outside the supported subset; carries the reason.
    Unsupported {
        construct: String,
    },
}

#[derive(Debug, Clone)]
pub struct SemFunction {
    pub id: FunctionId,
    pub module: ModuleId,
    pub name: String,
    pub kind: String,
    pub is_nobody: bool,
    pub effects_empty: bool,
    pub may_block: Option<bool>,
    pub params: Vec<SemSlot>,
    pub returns: Option<SemSlot>,
    /// params ++ locals ++ optional return slot; indices are stable per function.
    pub slots: Vec<SemSlot>,
    pub body: Vec<SemStmt>,
    pub sid_to_idx: BTreeMap<String, usize>,
    pub requires_held: Vec<ResourceId>,
    pub num_modeled_params: usize,
    pub bound: Option<i64>,
}

impl SemFunction {
    pub fn stmt_index(&self, sid: &str) -> Option<usize> {
        self.sid_to_idx.get(sid).copied()
    }

    /// True for a body-less function that the backend can treat as an
    /// immediate, effect-free placeholder.
    pub fn is_transparent_nobody(&self) -> bool {
        self.is_nobody
            && self.effects_empty
            && self.returns.is_none()
            && self.may_block != Some(true)
    }
}

#[derive(Debug, Clone)]
pub struct SemProgram {
    module_names: Vec<String>,
    types: Vec<SemTypeDef>,
    functions: Vec<SemFunction>,
    resources: Vec<SemResource>,
    protection: BTreeMap<ResourceId, ResourceId>,
    entry: FunctionId,
    unsupported: Vec<Unsupported>,
}

impl SemProgram {
    pub fn module_name(&self, m: ModuleId) -> &str {
        &self.module_names[m.index()]
    }

    pub fn module_names(&self) -> &[String] {
        &self.module_names
    }

    pub fn types(&self) -> &[SemTypeDef] {
        &self.types
    }

    pub fn resolve_type_def(&self, from: ModuleId, name: &str) -> Option<&SemTypeDef> {
        let (module_id, entity) = match fqn::split_fqn(name) {
            Some((m, e)) => (self.module_id(m)?, e),
            None => (from, name),
        };
        self.types
            .iter()
            .find(|t| t.module == module_id && t.name == entity)
    }

    pub fn module_id(&self, name: &str) -> Option<ModuleId> {
        self.module_names
            .iter()
            .position(|n| n == name)
            .map(|i| ModuleId(i as u32))
    }

    pub fn function(&self, f: FunctionId) -> &SemFunction {
        &self.functions[f.index()]
    }

    pub fn function_by_name(&self, fqn_name: &str) -> Option<FunctionId> {
        self.functions
            .iter()
            .find(|f| fqn::fqn(self.module_name(f.module), &f.name) == fqn_name)
            .map(|f| f.id)
    }

    pub fn functions(&self) -> &[SemFunction] {
        &self.functions
    }

    pub fn milestone_sites(&self, name: &str) -> Vec<(FunctionId, usize)> {
        let mut sites = Vec::new();
        for f in &self.functions {
            for (idx, stmt) in f.body.iter().enumerate() {
                if stmt.milestone.as_deref() == Some(name) {
                    sites.push((f.id, idx));
                }
            }
        }
        sites
    }

    pub fn resource(&self, r: ResourceId) -> &SemResource {
        &self.resources[r.index()]
    }

    pub fn resources(&self) -> &[SemResource] {
        &self.resources
    }

    pub fn entry(&self) -> FunctionId {
        self.entry
    }

    /// The module that contains the entry function. Unqualified contract names
    /// are resolved in this namespace, so declaration reordering is stable.
    pub fn entry_module(&self) -> ModuleId {
        self.functions[self.entry.index()].module
    }

    pub fn protection_lock(&self, var: ResourceId) -> Option<ResourceId> {
        self.protection.get(&var).copied()
    }

    pub fn protection(&self) -> &BTreeMap<ResourceId, ResourceId> {
        &self.protection
    }

    pub fn unsupported(&self) -> &[Unsupported] {
        &self.unsupported
    }

    pub fn resolve_resource(&self, from: ModuleId, name: &str) -> Option<ResourceId> {
        let (module_id, entity) = match fqn::split_fqn(name) {
            Some((m, e)) => (self.module_id(m)?, e),
            None => (from, name),
        };
        self.resources
            .iter()
            .find(|r| r.module == module_id && r.name == entity)
            .map(|r| r.id)
    }

    pub fn resolve_function(&self, from: ModuleId, name: &str) -> Option<FunctionId> {
        let (module_id, entity) = match fqn::split_fqn(name) {
            Some((m, e)) => (self.module_id(m)?, e),
            None => (from, name),
        };
        self.functions
            .iter()
            .find(|f| f.module == module_id && f.name == entity)
            .map(|f| f.id)
    }

    pub fn location(&self, function: FunctionId, sid: Option<usize>) -> String {
        location_of(self, function, sid)
    }
}

// ─────────────────────────── Lowering ───────────────────────────

pub fn lower(program: &Program) -> BackendResult<SemProgram> {
    let owned;
    let program = if program.has_structured_bodies() {
        let mut lowered = program.clone();
        lowered
            .ensure_lowered()
            .map_err(|msg| BackendError::invalid("E011", msg))?;
        owned = lowered;
        &owned
    } else {
        program
    };

    let module_names: Vec<String> = program.modules.iter().map(|m| m.name.clone()).collect();
    let mut module_ids = BTreeMap::new();
    for (i, name) in module_names.iter().enumerate() {
        module_ids.insert(name.clone(), ModuleId(i as u32));
    }
    let type_env = TypeEnv::from_program(program);

    let mut types = Vec::new();
    for (mi, m) in program.modules.iter().enumerate() {
        let mid = ModuleId(mi as u32);
        for t in &m.types {
            let resolved = type_env
                .resolve(&m.name, &t.ty)
                .unwrap_or_else(|| t.ty.clone());
            types.push(SemTypeDef {
                module: mid,
                name: t.name.clone(),
                ty: resolved,
                raw_ty: t.ty.clone(),
            });
        }
    }

    let mut lowerer = Lowerer {
        module_names,
        module_ids,
        resources: Vec::new(),
        functions: Vec::new(),
        entry: FunctionId(0),
        unsupported: Vec::new(),
        type_env,
    };

    lower_decl::lower_resources(program, &mut lowerer)?;
    for (mi, m) in program.modules.iter().enumerate() {
        for f in &m.functions {
            let id = FunctionId(lowerer.functions.len() as u32);
            lowerer
                .functions
                .push(lower_decl::placeholder_function(id, ModuleId(mi as u32), f));
        }
    }

    let entry = lowerer.function_by_fqn(&program.entry).ok_or_else(|| {
        BackendError::invalid(
            "E100",
            format!("entry function '{}' was not found", program.entry),
        )
    })?;
    lowerer.entry = entry;

    for m in &program.modules {
        for f in &m.functions {
            let fid = lowerer
                .function_by_fqn(&fqn::fqn(&m.name, &f.name))
                .ok_or_else(|| {
                    BackendError::invalid(
                        "E102",
                        format!(
                            "function '{}::{}' is not registered (duplicate or empty name)",
                            m.name, f.name
                        ),
                    )
                })?;
            let lowered = lower_body::lower_function_body(program, m, f, fid, &mut lowerer)?;
            lowerer.functions[fid.index()] = lowered;
        }
    }

    let protection = lower_decl::lower_protection(program, &lowerer)?;

    // Target check (W1xx): `std::sync::Condvar` binds to a single mutex, so a
    // condition variable waited on with more than one distinct lock is not
    // supported in the Rust target.
    {
        use std::collections::{BTreeMap, BTreeSet};
        let mut locks: BTreeMap<ResourceId, BTreeSet<ResourceId>> = BTreeMap::new();
        for f in &lowerer.functions {
            for st in &f.body {
                if let SemOp::CondvarWait { condvar, lock } = &st.op {
                    locks.entry(*condvar).or_default().insert(*lock);
                }
            }
        }
        for (condvar, ls) in &locks {
            if ls.len() > 1 {
                let name = lowerer
                    .resources
                    .iter()
                    .find(|r| r.id == *condvar)
                    .map(|r| r.name.clone())
                    .unwrap_or_else(|| "?".to_string());
                lowerer.unsupported.push(Unsupported::new(
                    "condvar_multiple_locks",
                    format!(
                        "condition variable '{name}' is waited on with {} different \
                         locks; the Rust target binds a Condvar to one mutex",
                        ls.len()
                    ),
                ));
            }
        }
    }

    Ok(SemProgram {
        module_names: lowerer.module_names,
        types,
        functions: lowerer.functions,
        resources: lowerer.resources,
        protection,
        entry: lowerer.entry,
        unsupported: lowerer.unsupported,
    })
}


pub(super) struct Lowerer {
    pub(super) module_names: Vec<String>,
    pub(super) module_ids: BTreeMap<String, ModuleId>,
    pub(super) resources: Vec<SemResource>,
    pub(super) functions: Vec<SemFunction>,
    pub(super) entry: FunctionId,
    pub(super) unsupported: Vec<Unsupported>,
    pub(super) type_env: TypeEnv,
}

impl Lowerer {
    pub(super) fn resource_of(&self, from: ModuleId, name: &str) -> Option<ResourceId> {
        let (module_id, entity) = match fqn::split_fqn(name) {
            Some((m, e)) => (self.module_ids.get(m).copied()?, e),
            None => (from, name),
        };
        self.resources
            .iter()
            .find(|r| r.module == module_id && r.name == entity)
            .map(|r| r.id)
    }

    pub(super) fn function_by_fqn(&self, fqn_name: &str) -> Option<FunctionId> {
        let (m, e) = fqn::split_fqn(fqn_name)?;
        let mid = self.module_ids.get(m).copied()?;
        self.functions
            .iter()
            .find(|f| f.module == mid && f.name == e)
            .map(|f| f.id)
    }

    pub(super) fn function(&self, id: FunctionId) -> &SemFunction {
        &self.functions[id.index()]
    }

    pub(super) fn resource(&self, id: ResourceId) -> &SemResource {
        &self.resources[id.index()]
    }

    pub(super) fn resolve_function(&self, from: ModuleId, name: &str) -> Option<FunctionId> {
        let (module_id, entity) = match fqn::split_fqn(name) {
            Some((m, e)) => (self.module_ids.get(m).copied()?, e),
            None => (from, name),
        };
        self.functions
            .iter()
            .find(|f| f.module == module_id && f.name == entity)
            .map(|f| f.id)
    }
}

