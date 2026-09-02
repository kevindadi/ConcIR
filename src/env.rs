//! Per-function name environment for ConcIR data flow.
//!
//! Resolution order inside function `F` of module `M` (see
//! `doc/syntax/dataflow.md`): `"_"` → local → param → return slot →
//! required FQN resource → in-module resource.

use std::collections::{HashMap, HashSet};

use crate::ast::{BaseType, ComplexBaseType, Function, Module, Program, Resource};
use crate::fqn;

/// Kind of a resolved name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    Local,
    Param,
    Return,
    Var,
    Atomic,
    /// `"_"` — discard dst only.
    Discard,
    /// Mutex / RwLock / Condvar / Semaphore / Channel: not a value slot.
    SyncResource,
}

/// A name visible in a function, with its type and projection flag.
#[derive(Debug, Clone)]
pub struct Slot {
    pub kind: SlotKind,
    pub ty: Option<BaseType>,
    /// Resources are always projected. Activation slots follow `modeled`.
    pub modeled: bool,
}

impl Slot {
    pub fn is_writable_value(&self) -> bool {
        matches!(
            self.kind,
            SlotKind::Local | SlotKind::Param | SlotKind::Var | SlotKind::Atomic
        )
    }

    pub fn is_discard(&self) -> bool {
        self.kind == SlotKind::Discard
    }

    pub fn is_assign_local_target(&self) -> bool {
        matches!(self.kind, SlotKind::Local | SlotKind::Param)
    }

    /// Scrutinee / r-value: activation slot or Var/Atomic.
    pub fn is_value_slot(&self) -> bool {
        matches!(
            self.kind,
            SlotKind::Local | SlotKind::Param | SlotKind::Return | SlotKind::Var | SlotKind::Atomic
        )
    }

    pub fn is_activation(&self) -> bool {
        matches!(
            self.kind,
            SlotKind::Local | SlotKind::Param | SlotKind::Return
        )
    }
}

/// Names resolvable from one function body.
#[derive(Debug, Clone)]
pub struct NameEnv {
    slots: HashMap<String, Slot>,
}

impl NameEnv {
    pub fn build(program: &Program, module: &Module, function: &Function) -> Self {
        let mut slots = HashMap::new();

        slots.insert(
            "_".to_string(),
            Slot {
                kind: SlotKind::Discard,
                ty: None,
                modeled: false,
            },
        );

        for p in &function.params {
            slots.insert(
                p.name.clone(),
                Slot {
                    kind: SlotKind::Param,
                    ty: Some(p.param_type.clone()),
                    modeled: p.modeled,
                },
            );
        }
        for local in &function.locals {
            slots.insert(
                local.name.clone(),
                Slot {
                    kind: SlotKind::Local,
                    ty: Some(local.local_type.clone()),
                    modeled: local.modeled,
                },
            );
        }
        if let Some(ret) = &function.returns {
            slots.entry(ret.name.clone()).or_insert(Slot {
                kind: SlotKind::Return,
                ty: Some(ret.param_type.clone()),
                modeled: ret.modeled,
            });
        }

        for m in &program.modules {
            for r in &m.resources {
                let slot = resource_slot(r);
                if m.name == module.name {
                    slots.entry(r.name.clone()).or_insert_with(|| slot.clone());
                    slots
                        .entry(fqn::fqn(&m.name, &r.name))
                        .or_insert_with(|| slot.clone());
                }
            }
        }
        for req in &module.requires.resources {
            if let Some((_, r)) = program.lookup_resource(&module.name, req) {
                slots.entry(req.clone()).or_insert_with(|| resource_slot(r));
            }
        }

        NameEnv { slots }
    }

    pub fn get(&self, name: &str) -> Option<&Slot> {
        self.slots.get(name)
    }

    pub fn ty(&self, name: &str) -> Option<&BaseType> {
        self.get(name).and_then(|s| s.ty.as_ref())
    }

    pub fn enum_variants(&self) -> HashSet<String> {
        let mut out = HashSet::new();
        for slot in self.slots.values() {
            if let Some(BaseType::Complex(ComplexBaseType::Enum(vars))) = &slot.ty {
                out.extend(vars.iter().cloned());
            }
        }
        out
    }
}

fn resource_slot(r: &Resource) -> Slot {
    match (r.kind.as_str(), r.res_type.as_str()) {
        ("var", "Var") => Slot {
            kind: SlotKind::Var,
            ty: r.base.clone(),
            modeled: true,
        },
        ("var", "Atomic") => Slot {
            kind: SlotKind::Atomic,
            ty: r.base.clone(),
            modeled: true,
        },
        _ => Slot {
            kind: SlotKind::SyncResource,
            ty: r.base.clone(),
            modeled: true,
        },
    }
}
