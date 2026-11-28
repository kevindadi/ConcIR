use std::collections::BTreeMap;

use concir_ir::ast::{Function, Program, Resource};
use concir_ir::typedef::TypeEnv;

use crate::ids::{FunctionId, ModuleId, ResourceId};
use crate::outcome::{BackendError, BackendResult};
use crate::value::{default_value, from_json};

use super::{Lowerer, ResKind, SemFunction, SemResource};

pub(super) fn lower_resources(program: &Program, l: &mut Lowerer) -> BackendResult<()> {
    for (mi, m) in program.modules.iter().enumerate() {
        let module = ModuleId(mi as u32);
        for r in &m.resources {
            let id = ResourceId(l.resources.len() as u32);
            let res = lower_resource(&m.name, module, r, id, &l.type_env)?;
            l.resources.push(res);
        }
    }
    Ok(())
}

fn lower_resource(
    module_name: &str,
    module: ModuleId,
    r: &Resource,
    id: ResourceId,
    tenv: &TypeEnv,
) -> BackendResult<SemResource> {
    let kind = match (r.kind.as_str(), r.res_type.as_str()) {
        ("var", "Var") => ResKind::Var,
        ("var", "Atomic") => ResKind::Atomic,
        ("sync", "Mutex") => ResKind::Mutex,
        ("sync", "RwLock") => ResKind::RwLock,
        ("sync", "Condvar") => ResKind::Condvar,
        ("sync", "Semaphore") => ResKind::Semaphore,
        ("sync", "Channel") => ResKind::Channel,
        (k, t) => {
            return Err(BackendError::invalid(
                "E300",
                format!("unknown resource kind/type '{k}'/'{t}'"),
            ))
        }
    };
    let ty = match kind {
        ResKind::Var | ResKind::Atomic | ResKind::Channel => {
            let base = r.base.as_ref().ok_or_else(|| {
                BackendError::invalid("E001", format!("resource '{}' is missing 'base'", r.name))
            })?;
            Some(
                tenv.resolve(module_name, base)
                    .unwrap_or_else(|| base.clone()),
            )
        }
        ResKind::Mutex | ResKind::RwLock => r.base.as_ref().map(|base| {
            tenv.resolve(module_name, base)
                .unwrap_or_else(|| base.clone())
        }),
        _ => None,
    };
    let mut init = None;
    if matches!(kind, ResKind::Var | ResKind::Atomic) {
        let base = ty.as_ref().unwrap();
        init = match &r.init {
            Some(j) => Some(from_json(j, base).map_err(|e| {
                BackendError::invalid("E203", format!("resource '{}' init: {e}", r.name))
            })?),
            None => Some(default_value(base)),
        };
    }
    let capacity = match kind {
        ResKind::Channel => r.capacity.unwrap_or(-1).max(0) as usize,
        _ => 0,
    };
    let permits = match kind {
        ResKind::Semaphore => r.count.unwrap_or(1),
        _ => 0,
    };
    Ok(SemResource {
        id,
        module,
        name: r.name.clone(),
        kind,
        ty,
        raw_ty: r.base.clone(),
        capacity,
        permits,
        init,
        mode: r.mode.clone(),
    })
}

pub(super) fn placeholder_function(id: FunctionId, module: ModuleId, f: &Function) -> SemFunction {
    SemFunction {
        id,
        module,
        name: f.name.clone(),
        kind: f.kind.clone(),
        is_nobody: f.body.is_empty(),
        effects_empty: f
            .effects
            .as_ref()
            .map(|e| e.reads.is_empty() && e.writes.is_empty())
            .unwrap_or(true),
        may_block: f.may_block,
        params: Vec::new(),
        returns: None,
        slots: Vec::new(),
        body: Vec::new(),
        sid_to_idx: BTreeMap::new(),
        requires_held: Vec::new(),
        num_modeled_params: f.params.iter().filter(|p| p.modeled).count(),
        bound: f.bound,
    }
}


pub(super) fn lower_protection(
    program: &Program,
    l: &Lowerer,
) -> BackendResult<BTreeMap<ResourceId, ResourceId>> {
    let mut out = BTreeMap::new();
    for (mi, m) in program.modules.iter().enumerate() {
        let module = ModuleId(mi as u32);
        for p in &m.protection {
            let var = l.resource_of(module, &p.var).ok_or_else(|| {
                BackendError::invalid(
                    "E703",
                    format!("protection references unknown Var '{}'", p.var),
                )
            })?;
            let lock = l.resource_of(module, &p.lock).ok_or_else(|| {
                BackendError::invalid(
                    "E703",
                    format!("protection references unknown lock '{}'", p.lock),
                )
            })?;
            out.insert(var, lock);
        }
    }
    Ok(out)
}
