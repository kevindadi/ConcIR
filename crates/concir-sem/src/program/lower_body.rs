use std::collections::BTreeMap;

use concir_ir::ast::{self, BaseType, Function, Program};
use concir_ir::env::NameEnv;
use concir_ir::expr;
use concir_ir::fqn;

use crate::eval::LExpr;
use crate::ids::{FunctionId, ModuleId, ResourceId, SlotRef, StatementId};
use crate::outcome::{BackendError, BackendResult, Unsupported};
use crate::value::from_json;

use super::{Lowerer, ResKind, SemFunction, SemModeledUpdate, SemOp, SemSlot, SemStmt, SlotClass};

pub(super) fn lower_function_body(
    program: &Program,
    m: &ast::Module,
    f: &Function,
    fid: FunctionId,
    l: &mut Lowerer,
) -> BackendResult<SemFunction> {
    let env = NameEnv::build(program, m, f);
    let module = l.module_ids[&m.name];

    // ── slots: params, locals, optional return ──
    let mut slots: Vec<SemSlot> = Vec::new();
    let mut name_to_slot: BTreeMap<String, usize> = BTreeMap::new();
    let mut slot_class: BTreeMap<usize, SlotClass> = BTreeMap::new();

    for p in &f.params {
        let ty = env
            .ty(&p.name)
            .cloned()
            .unwrap_or_else(|| p.param_type.clone());
        let idx = slots.len();
        slots.push(SemSlot {
            name: p.name.clone(),
            ty,
            raw_ty: p.param_type.clone(),
            modeled: p.modeled,
            class: SlotClass::Param,
            init: None,
        });
        name_to_slot.insert(p.name.clone(), idx);
        slot_class.insert(idx, SlotClass::Param);
    }
    for local in &f.locals {
        let ty = env
            .ty(&local.name)
            .cloned()
            .unwrap_or_else(|| local.local_type.clone());
        let init = match &local.init {
            Some(j) => Some(from_json(j, &ty).map_err(|e| {
                BackendError::invalid("E203", format!("local '{}' init: {e}", local.name))
            })?),
            None => None,
        };
        let idx = slots.len();
        slots.push(SemSlot {
            name: local.name.clone(),
            ty,
            raw_ty: local.local_type.clone(),
            modeled: local.modeled,
            class: SlotClass::Local,
            init,
        });
        name_to_slot.insert(local.name.clone(), idx);
        slot_class.insert(idx, SlotClass::Local);
    }
    let returns = f.returns.as_ref().map(|ret| {
        let ty = env
            .ty(&ret.name)
            .cloned()
            .unwrap_or_else(|| ret.param_type.clone());
        SemSlot {
            name: ret.name.clone(),
            ty,
            raw_ty: ret.param_type.clone(),
            modeled: ret.modeled,
            class: SlotClass::Return,
            init: None,
        }
    });
    if let Some(ret) = &returns {
        if !name_to_slot.contains_key(&ret.name) {
            let idx = slots.len();
            slots.push(ret.clone());
            name_to_slot.insert(ret.name.clone(), idx);
            slot_class.insert(idx, SlotClass::Return);
        }
    }

    // ── resource name bindings, matching NameEnv visibility ──
    let mut name_to_resource: BTreeMap<String, ResourceId> = BTreeMap::new();
    for owner in &program.modules {
        if owner.name != m.name {
            continue;
        }
        for r in &owner.resources {
            if let Some(rid) = l.resource_of(module, &r.name) {
                name_to_resource.insert(r.name.clone(), rid);
                name_to_resource.insert(fqn::fqn(&m.name, &r.name), rid);
            }
        }
    }
    for req in &m.requires.resources {
        if let Some(rid) = l.resource_of(module, req) {
            name_to_resource.insert(req.clone(), rid);
        }
    }
    let resource_kinds: BTreeMap<ResourceId, ResKind> =
        l.resources.iter().map(|r| (r.id, r.kind)).collect();
    let resource_tys: BTreeMap<ResourceId, BaseType> = l
        .resources
        .iter()
        .filter_map(|r| r.ty.clone().map(|t| (r.id, t)))
        .collect();

    let mut sid_to_idx = BTreeMap::new();
    for (i, s) in f.body.iter().enumerate() {
        sid_to_idx.insert(s.sid.clone(), i);
    }

    let ctx = BodyCtx {
        module_name: &m.name,
        fn_name: &f.name,
        module,
        env: &env,
        slots: &slots,
        name_to_slot: &name_to_slot,
        slot_class: &slot_class,
        name_to_resource: &name_to_resource,
        resource_kinds: &resource_kinds,
        resource_tys: &resource_tys,
        sid_to_idx: &sid_to_idx,
    };

    let mut body = Vec::new();
    for (i, s) in f.body.iter().enumerate() {
        let id = StatementId(i as u32);
        let op = lower_op(l, &ctx, s)?;
        body.push(SemStmt {
            id,
            sid: s.sid.clone(),
            req: s.effective_req(),
            milestone: s.milestone.clone(),
            op,
        });
    }

    let requires_held = f
        .locks
        .requires_held
        .iter()
        .filter_map(|name| l.resource_of(module, name))
        .collect();

    Ok(SemFunction {
        id: fid,
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
        params: slots
            .iter()
            .filter(|s| s.class == SlotClass::Param)
            .cloned()
            .collect(),
        returns,
        slots,
        body,
        sid_to_idx,
        requires_held,
        num_modeled_params: f.params.iter().filter(|p| p.modeled).count(),
        bound: f.bound,
    })
}

struct BodyCtx<'a> {
    module_name: &'a str,
    fn_name: &'a str,
    module: ModuleId,
    env: &'a NameEnv,
    slots: &'a [SemSlot],
    name_to_slot: &'a BTreeMap<String, usize>,
    slot_class: &'a BTreeMap<usize, SlotClass>,
    name_to_resource: &'a BTreeMap<String, ResourceId>,
    resource_kinds: &'a BTreeMap<ResourceId, ResKind>,
    resource_tys: &'a BTreeMap<ResourceId, BaseType>,
    sid_to_idx: &'a BTreeMap<String, usize>,
}

impl<'a> BodyCtx<'a> {
    fn at(&self, s: &ast::Stmt) -> String {
        fqn::location(self.module_name, self.fn_name, &s.sid)
    }

    fn value_slot(&self, name: &str, at: &str) -> BackendResult<SlotRef> {
        if let Some(idx) = self.name_to_slot.get(name) {
            return Ok(SlotRef::Local(*idx));
        }
        if let Some(rid) = self.name_to_resource.get(name) {
            return match self.resource_kinds.get(rid) {
                Some(ResKind::Var) | Some(ResKind::Atomic) => Ok(SlotRef::Shared(*rid)),
                _ => Err(BackendError::invalid(
                    "E934",
                    format!("'{name}' is a sync resource and cannot appear as a value"),
                )
                .at(at)),
            };
        }
        Err(BackendError::invalid("E931", format!("undefined name '{name}' in expression")).at(at))
    }

    fn writable_slot(&self, name: &str, at: &str) -> BackendResult<SlotRef> {
        if name == "_" {
            return Ok(SlotRef::Discard);
        }
        if let Some(idx) = self.name_to_slot.get(name) {
            if self.slot_class.get(idx) == Some(&SlotClass::Return) {
                return Err(BackendError::invalid(
                    "E921",
                    format!("return slot '{name}' is not a writable destination"),
                )
                .at(at));
            }
            return Ok(SlotRef::Local(*idx));
        }
        if let Some(rid) = self.name_to_resource.get(name) {
            return match self.resource_kinds.get(rid) {
                Some(ResKind::Var) | Some(ResKind::Atomic) => Ok(SlotRef::Shared(*rid)),
                _ => Err(BackendError::invalid(
                    "E934",
                    format!("'{name}' is a sync resource and cannot be a destination"),
                )
                .at(at)),
            };
        }
        Err(BackendError::invalid("E921", format!("'{name}' is not a writable slot")).at(at))
    }

    fn expr(&self, text: &str, at: &str) -> BackendResult<LExpr> {
        let parsed = expr::parse(text, self.env)
            .map_err(|e| BackendError::invalid("E931", e.message).at(at))?;
        self.lower_expr(&parsed, at)
    }

    fn lower_expr(&self, e: &expr::Expr, at: &str) -> BackendResult<LExpr> {
        Ok(match e {
            expr::Expr::Lit(l) => LExpr::Lit(l.clone()),
            expr::Expr::Name(n) => LExpr::Slot(self.value_slot(n, at)?),
            expr::Expr::Field { base, field } => LExpr::Field {
                base: Box::new(self.lower_expr(base, at)?),
                field: field.clone(),
            },
            expr::Expr::UnaryNeg(inner) => LExpr::Neg(Box::new(self.lower_expr(inner, at)?)),
            expr::Expr::BinOp { op, lhs, rhs } => LExpr::Bin {
                op: *op,
                lhs: Box::new(self.lower_expr(lhs, at)?),
                rhs: Box::new(self.lower_expr(rhs, at)?),
            },
            expr::Expr::Cmp { op, lhs, rhs } => LExpr::Cmp {
                op: *op,
                lhs: Box::new(self.lower_expr(lhs, at)?),
                rhs: Box::new(self.lower_expr(rhs, at)?),
            },
            expr::Expr::Struct { fields } => LExpr::Struct {
                fields: fields
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), self.lower_expr(v, at)?)))
                    .collect::<BackendResult<Vec<_>>>()?,
            },
        })
    }

    fn expr_is_float(&self, e: &LExpr) -> bool {
        match e {
            LExpr::Lit(concir_ir::expr::Lit::Float(_)) => true,
            LExpr::Lit(_) => false,
            LExpr::Slot(SlotRef::Local(i)) => self
                .slots
                .get(*i)
                .map(|s| is_float_ty(&s.ty))
                .unwrap_or(false),
            LExpr::Slot(SlotRef::Shared(r)) => {
                self.resource_tys.get(r).map(is_float_ty).unwrap_or(false)
            }
            LExpr::Slot(SlotRef::Discard) => false,
            LExpr::Field { base, .. } => self.expr_is_float(base),
            LExpr::Neg(x) => self.expr_is_float(x),
            LExpr::Bin { lhs, rhs, .. } | LExpr::Cmp { lhs, rhs, .. } => {
                self.expr_is_float(lhs) || self.expr_is_float(rhs)
            }
            LExpr::Struct { fields } => fields.iter().any(|(_, v)| self.expr_is_float(v)),
        }
    }

    fn reject_float_control(&self, e: &LExpr, at: &str) -> BackendResult<()> {
        if self.expr_is_float(e) {
            return Err(BackendError::unsupported(
                "float control flow",
                "a branch/switch decision depends on a Float value",
            )
            .at(at));
        }
        Ok(())
    }

    fn target(&self, sid: &str, at: &str) -> BackendResult<usize> {
        self.sid_to_idx.get(sid).copied().ok_or_else(|| {
            BackendError::invalid("E602", format!("control target '{sid}' does not exist")).at(at)
        })
    }

    fn resource(&self, name: &str, at: &str) -> BackendResult<ResourceId> {
        self.name_to_resource.get(name).copied().ok_or_else(|| {
            BackendError::invalid("E100", format!("unknown resource '{name}'")).at(at)
        })
    }
}

fn is_float_ty(ty: &BaseType) -> bool {
    matches!(ty, BaseType::Primitive(p) if p == "Float")
}

fn unsupported_op(
    l: &mut Lowerer,
    ctx: &BodyCtx,
    s: &ast::Stmt,
    construct: &str,
    detail: &str,
) -> SemOp {
    l.unsupported
        .push(Unsupported::new(construct, detail).at(fqn::location(
            ctx.module_name,
            ctx.fn_name,
            &s.sid,
        )));
    SemOp::Unsupported {
        construct: construct.to_string(),
    }
}

fn lower_op(l: &mut Lowerer, ctx: &BodyCtx, s: &ast::Stmt) -> BackendResult<SemOp> {
    let at = ctx.at(s);
    Ok(match &s.op {
        ast::Op::Nop => SemOp::Nop,
        ast::Op::AssignLocal { target, expr } => {
            let t = ctx.writable_slot(target, &at)?;
            if !matches!(t, SlotRef::Local(_)) {
                return Err(BackendError::invalid(
                    "E936",
                    format!("assign_local target '{target}' is not a local or parameter"),
                )
                .at(&at));
            }
            SemOp::AssignLocal {
                target: t,
                expr: ctx.expr(expr, &at)?,
            }
        }
        ast::Op::ReadShared { resource, dst } => {
            let r = ctx.resource(resource, &at)?;
            if ctx.resource_kinds.get(&r) != Some(&ResKind::Var) {
                return Err(BackendError::invalid(
                    "E308",
                    format!("read_shared requires a Var, found '{resource}'"),
                )
                .at(&at));
            }
            let dst = match dst {
                Some(d) => Some(ctx.writable_slot(d, &at)?),
                None => None,
            };
            SemOp::ReadShared { resource: r, dst }
        }
        ast::Op::WriteShared { resource, expr } => {
            let r = ctx.resource(resource, &at)?;
            if ctx.resource_kinds.get(&r) != Some(&ResKind::Var) {
                return Err(BackendError::invalid(
                    "E308",
                    format!("write_shared requires a Var, found '{resource}'"),
                )
                .at(&at));
            }
            SemOp::WriteShared {
                resource: r,
                expr: ctx.expr(expr, &at)?,
            }
        }
        ast::Op::AtomicLoad { resource, dst } => {
            let r = ctx.resource(resource, &at)?;
            if ctx.resource_kinds.get(&r) != Some(&ResKind::Atomic) {
                return Err(BackendError::invalid(
                    "E309",
                    format!("atomic_load requires an Atomic, found '{resource}'"),
                )
                .at(&at));
            }
            SemOp::AtomicLoad {
                resource: r,
                dst: ctx.writable_slot(dst, &at)?,
            }
        }
        ast::Op::AtomicStore { resource, value } => {
            let r = ctx.resource(resource, &at)?;
            if ctx.resource_kinds.get(&r) != Some(&ResKind::Atomic) {
                return Err(BackendError::invalid(
                    "E204",
                    format!("atomic_store requires an Atomic, found '{resource}'"),
                )
                .at(&at));
            }
            SemOp::AtomicStore {
                resource: r,
                value: ctx.expr(value, &at)?,
            }
        }
        ast::Op::AtomicCas {
            resource,
            expected,
            desired,
            dst,
        } => {
            let r = ctx.resource(resource, &at)?;
            if ctx.resource_kinds.get(&r) != Some(&ResKind::Atomic) {
                return Err(BackendError::invalid(
                    "E205",
                    format!("atomic_cas requires an Atomic, found '{resource}'"),
                )
                .at(&at));
            }
            SemOp::AtomicCas {
                resource: r,
                expected: ctx.expr(expected, &at)?,
                desired: ctx.expr(desired, &at)?,
                dst: ctx.writable_slot(dst, &at)?,
            }
        }
        ast::Op::MutexLock { resource } => {
            let r = ctx.resource(resource, &at)?;
            if ctx.resource_kinds.get(&r) != Some(&ResKind::Mutex) {
                return Err(BackendError::invalid(
                    "E301",
                    format!("mutex_lock requires a Mutex, found '{resource}'"),
                )
                .at(&at));
            }
            SemOp::MutexLock { resource: r }
        }
        ast::Op::MutexUnlock { resource } => {
            let r = ctx.resource(resource, &at)?;
            if ctx.resource_kinds.get(&r) != Some(&ResKind::Mutex) {
                return Err(BackendError::invalid(
                    "E301",
                    format!("mutex_unlock requires a Mutex, found '{resource}'"),
                )
                .at(&at));
            }
            SemOp::MutexUnlock { resource: r }
        }
        ast::Op::ChannelSend { channel, value } => {
            let r = ctx.resource(channel, &at)?;
            if ctx.resource_kinds.get(&r) != Some(&ResKind::Channel) {
                return Err(BackendError::invalid(
                    "E302",
                    format!("channel_send requires a Channel, found '{channel}'"),
                )
                .at(&at));
            }
            SemOp::ChannelSend {
                channel: r,
                value: ctx.expr(value, &at)?,
            }
        }
        ast::Op::ChannelRecv { channel, dst } => {
            let r = ctx.resource(channel, &at)?;
            if ctx.resource_kinds.get(&r) != Some(&ResKind::Channel) {
                return Err(BackendError::invalid(
                    "E302",
                    format!("channel_recv requires a Channel, found '{channel}'"),
                )
                .at(&at));
            }
            SemOp::ChannelRecv {
                channel: r,
                dst: ctx.writable_slot(dst, &at)?,
            }
        }
        ast::Op::CondvarWait { condvar, lock } => {
            let cv = ctx.resource(condvar, &at)?;
            if ctx.resource_kinds.get(&cv) != Some(&ResKind::Condvar) {
                return Err(BackendError::invalid(
                    "E303",
                    format!("condvar_wait requires a Condvar, found '{condvar}'"),
                )
                .at(&at));
            }
            if l.resource(cv).mode.as_deref() == Some("Async") {
                return Ok(unsupported_op(
                    l,
                    ctx,
                    s,
                    "async condvar_wait",
                    "Async-mode Condvar wait is not the same primitive as std Condvar::wait",
                ));
            }
            let lk = ctx.resource(lock, &at)?;
            if ctx.resource_kinds.get(&lk) != Some(&ResKind::Mutex) {
                return Err(BackendError::invalid(
                    "E303",
                    format!("condvar_wait lock must be a Mutex, found '{lock}'"),
                )
                .at(&at));
            }
            SemOp::CondvarWait {
                condvar: cv,
                lock: lk,
            }
        }
        ast::Op::CondvarNotify { condvar } => {
            let cv = ctx.resource(condvar, &at)?;
            if ctx.resource_kinds.get(&cv) != Some(&ResKind::Condvar) {
                return Err(BackendError::invalid(
                    "E303",
                    format!("condvar_notify requires a Condvar, found '{condvar}'"),
                )
                .at(&at));
            }
            SemOp::CondvarNotify { condvar: cv }
        }
        ast::Op::CondvarNotifyAll { condvar } => {
            let cv = ctx.resource(condvar, &at)?;
            if ctx.resource_kinds.get(&cv) != Some(&ResKind::Condvar) {
                return Err(BackendError::invalid(
                    "E303",
                    format!("condvar_notify_all requires a Condvar, found '{condvar}'"),
                )
                .at(&at));
            }
            SemOp::CondvarNotifyAll { condvar: cv }
        }
        ast::Op::SemaphoreAcquire { resource, count } => {
            let r = ctx.resource(resource, &at)?;
            if ctx.resource_kinds.get(&r) != Some(&ResKind::Semaphore) {
                return Err(BackendError::invalid(
                    "E304",
                    format!("semaphore_acquire requires a Semaphore, found '{resource}'"),
                )
                .at(&at));
            }
            SemOp::SemaphoreAcquire {
                resource: r,
                count: count.unwrap_or(1),
            }
        }
        ast::Op::SemaphoreRelease { resource, count } => {
            let r = ctx.resource(resource, &at)?;
            if ctx.resource_kinds.get(&r) != Some(&ResKind::Semaphore) {
                return Err(BackendError::invalid(
                    "E304",
                    format!("semaphore_release requires a Semaphore, found '{resource}'"),
                )
                .at(&at));
            }
            SemOp::SemaphoreRelease {
                resource: r,
                count: count.unwrap_or(1),
            }
        }
        ast::Op::Func { func, args, dst } => {
            let callee = l.resolve_function(ctx.module, func).ok_or_else(|| {
                BackendError::invalid("E101", format!("unknown function '{func}'")).at(&at)
            })?;
            let f = l.function(callee);
            if f.is_nobody && !f.is_transparent_nobody() {
                return Ok(unsupported_op(
                    l,
                    ctx,
                    s,
                    "external function",
                    &format!("call to body-less '{func}' with declared effects/blocking/return"),
                ));
            }
            if args.len() != f.num_modeled_params {
                return Err(BackendError::invalid(
                    "E920",
                    format!(
                        "call to '{func}' has {} args, expected {} modeled params",
                        args.len(),
                        f.num_modeled_params
                    ),
                )
                .at(&at));
            }
            let lowered_args = args
                .iter()
                .map(|a| ctx.expr(a, &at))
                .collect::<BackendResult<Vec<_>>>()?;
            let dst = match dst {
                Some(d) => Some(ctx.writable_slot(d, &at)?),
                None => None,
            };
            SemOp::Call {
                func: callee,
                args: lowered_args,
                dst,
            }
        }
        ast::Op::Spawn {
            func,
            handle,
            args: _,
        } => {
            let callee = l.resolve_function(ctx.module, func).ok_or_else(|| {
                BackendError::invalid("E101", format!("unknown function '{func}'")).at(&at)
            })?;
            let f = l.function(callee);
            if f.num_modeled_params != 0 {
                return Err(BackendError::invalid(
                    "E922",
                    format!("spawn target '{func}' has modeled parameters"),
                )
                .at(&at));
            }
            if f.is_nobody && !f.is_transparent_nobody() {
                return Ok(unsupported_op(
                    l,
                    ctx,
                    s,
                    "external function",
                    &format!("spawn of body-less '{func}' with declared effects"),
                ));
            }
            SemOp::Spawn {
                func: callee,
                handle: handle.clone(),
            }
        }
        ast::Op::Scope { funcs } => {
            let mut ids = Vec::new();
            for func in funcs {
                let callee = l.resolve_function(ctx.module, func).ok_or_else(|| {
                    BackendError::invalid("E101", format!("unknown function '{func}'")).at(&at)
                })?;
                let f = l.function(callee);
                if f.num_modeled_params != 0 {
                    return Err(BackendError::invalid(
                        "E922",
                        format!("scope target '{func}' has modeled parameters"),
                    )
                    .at(&at));
                }
                if f.is_nobody && !f.is_transparent_nobody() {
                    return Ok(unsupported_op(
                        l,
                        ctx,
                        s,
                        "external function",
                        &format!("scope target body-less '{func}' with declared effects"),
                    ));
                }
                ids.push(callee);
            }
            SemOp::Scope { funcs: ids }
        }
        ast::Op::Join { handle } => SemOp::Join {
            handle: handle.clone(),
        },
        ast::Op::Goto { target } => SemOp::Goto {
            target: ctx.target(target, &at)?,
        },
        ast::Op::Branch {
            cond,
            then,
            else_target,
        } => {
            let c = ctx.expr(cond, &at)?;
            if !c.is_comparison() {
                return Err(BackendError::invalid(
                    "E201",
                    format!("branch condition \"{cond}\" is not a comparison"),
                )
                .at(&at));
            }
            ctx.reject_float_control(&c, &at)?;
            SemOp::Branch {
                cond: c,
                then: ctx.target(then, &at)?,
                else_target: ctx.target(else_target, &at)?,
            }
        }
        ast::Op::Switch {
            var,
            cases,
            default,
        } => {
            let slot = ctx.value_slot(var, &at)?;
            let var_expr = LExpr::Slot(slot);
            ctx.reject_float_control(&var_expr, &at)?;
            let mut lowered = BTreeMap::new();
            for (label, target) in cases {
                lowered.insert(label.clone(), ctx.target(target, &at)?);
            }
            SemOp::Switch {
                var: var_expr,
                cases: lowered,
                default: ctx.target(default, &at)?,
            }
        }
        ast::Op::Return { value } => SemOp::Return {
            value: match value {
                Some(v) => Some(ctx.expr(v, &at)?),
                None => None,
            },
        },
        ast::Op::RwLockRead { resource } => {
            let r = ctx.resource(resource, &at)?;
            if ctx.resource_kinds.get(&r) != Some(&ResKind::RwLock) {
                return Err(BackendError::invalid(
                    "E301",
                    format!("rwlock_read requires an RwLock, found '{resource}'"),
                )
                .at(&at));
            }
            SemOp::RwLockRead { resource: r }
        }
        ast::Op::RwLockWrite { resource } => {
            let r = ctx.resource(resource, &at)?;
            if ctx.resource_kinds.get(&r) != Some(&ResKind::RwLock) {
                return Err(BackendError::invalid(
                    "E301",
                    format!("rwlock_write requires an RwLock, found '{resource}'"),
                )
                .at(&at));
            }
            SemOp::RwLockWrite { resource: r }
        }
        ast::Op::RwLockUnlock { resource } => {
            let r = ctx.resource(resource, &at)?;
            if ctx.resource_kinds.get(&r) != Some(&ResKind::RwLock) {
                return Err(BackendError::invalid(
                    "E301",
                    format!("rwlock_unlock requires an RwLock, found '{resource}'"),
                )
                .at(&at));
            }
            SemOp::RwLockUnlock { resource: r }
        }
        ast::Op::Select { .. } => {
            unsupported_op(l, ctx, s, "select", "select is not supported in v1")
        }
        ast::Op::AsyncCall { .. } => {
            unsupported_op(l, ctx, s, "async_call", "async_call is not supported in v1")
        }
        ast::Op::Await { .. } => unsupported_op(l, ctx, s, "await", "await is not supported in v1"),
        ast::Op::AbstractStep { .. } => unsupported_op(
            l,
            ctx,
            s,
            "abstract_step",
            "opaque concurrent steps are not given semantics by the precise backend",
        ),
        ast::Op::SeqHole {
            id,
            desc,
            reads,
            writes,
            updates_modeled,
            expected_type,
            req,
        } => {
            let mut lowered_updates = Vec::with_capacity(updates_modeled.len());
            for u in updates_modeled {
                let target = ctx.writable_slot(&u.target, &at)?;
                match target {
                    SlotRef::Local(_) => {}
                    SlotRef::Shared(rid)
                        if ctx.resource_kinds.get(&rid) == Some(&ResKind::Var) => {}
                    SlotRef::Shared(_) => {
                        return Err(BackendError::invalid(
                            "E310",
                            format!(
                                "seq_hole '{}' modeled update target '{}' must be a local or shared Var",
                                id, u.target
                            ),
                        )
                        .at(&at));
                    }
                    SlotRef::Discard => {
                        return Err(BackendError::invalid(
                            "E921",
                            format!("seq_hole '{}' modeled update target cannot be '_'", id),
                        )
                        .at(&at));
                    }
                }
                let expr = ctx.expr(&u.expr, &at)?;
                lowered_updates.push(SemModeledUpdate { target, expr });
            }
            let mut merged_req = s.req.clone();
            for r in req {
                if !merged_req.contains(r) {
                    merged_req.push(r.clone());
                }
            }
            SemOp::SeqHole {
                id: id.clone(),
                desc: desc.clone(),
                reads: reads.clone(),
                writes: writes.clone(),
                updates_modeled: lowered_updates,
                expected_type: expected_type.clone().unwrap_or_else(|| "()".to_string()),
                req: merged_req,
            }
        }
    })
}

