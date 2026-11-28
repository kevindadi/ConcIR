use concir_ir::ast::{BaseType, ComplexBaseType};
use concir_sem::ids::{ModuleId, SlotRef};
use concir_sem::program::{ResKind, SemFunction, SemOp};

use crate::{child_tags, sanitize, Gen, Hole, TagRecord};

pub(crate) fn expected_rust_ty(g: &Gen, from_module: ModuleId, expected: &str) -> String {
    match expected.trim() {
        "" | "()" => "()".to_string(),
        "Int" => "i64".to_string(),
        "Bool" => "bool".to_string(),
        "Float" => "f64".to_string(),
        other => {
            if let Some(td) = g.program.resolve_type_def(from_module, other) {
                if let Some(rname) = g.named_types.get(&(td.module, td.name.clone())) {
                    return format!("crate::domain::{rname}");
                }
            }
            other.to_string()
        }
    }
}

pub(crate) fn render_op(
    g: &mut Gen,
    func: &concir_sem::program::SemFunction,
    op: &SemOp,
    idx: usize,
    body_len: usize,
    sid: &str,
    guards: &[String],
) -> Result<(), String> {
    let next = idx + 1;
    let mod_name = g.program.module_name(func.module).to_string();
    match op {
        SemOp::Nop => {
            g.emit(format!("                pc = {next};"));
        }
        SemOp::AssignLocal { target, expr } => {
            let e = g.render_expr(&mod_name, &func.name, sid, expr);
            match target {
                SlotRef::Local(i) => {
                    g.emit(format!("                _l{i} = {e};"));
                }
                SlotRef::Discard => {}
                SlotRef::Shared(r) => {
                    let w = g.render_shared_write(*r, &e);
                    g.emit(format!("                {w}"));
                }
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::ReadShared { resource, dst } => {
            let rd = g.render_shared_read(*resource);
            match dst {
                Some(SlotRef::Local(i)) => {
                    g.emit(format!("                _l{i} = {rd};"));
                }
                Some(SlotRef::Shared(r)) => {
                    let w = g.render_shared_write(*r, "__v");
                    g.emit(format!("                {{ let __v = {rd}; {w} }}"));
                }
                _ => {
                    g.emit(format!("                let _ = {rd};"));
                }
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::WriteShared { resource, expr } => {
            let e = g.render_expr(&mod_name, &func.name, sid, expr);
            let w = g.render_shared_write(*resource, "__v");
            g.emit(format!("                {{ let __v = {e}; {w} }}"));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::MutexLock { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "                _g_{f} = Some(crate::cir_trace::lock(&shared.{f}, tag, {rq:?}, {sid:?}));"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::MutexUnlock { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "                crate::cir_trace::unlock(_g_{f}.take(), tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::RwLockRead { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "                _rg_{f} = Some(crate::cir_trace::rwlock_read_lock(&shared.{f}, tag, {rq:?}, {sid:?}));"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::RwLockWrite { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "                _wg_{f} = Some(crate::cir_trace::rwlock_write_lock(&shared.{f}, tag, {rq:?}, {sid:?}));"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::RwLockUnlock { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "                if let Some(__wg) = _wg_{f}.take() {{ crate::cir_trace::rwlock_write_unlock(Some(__wg), tag, {rq:?}, {sid:?}); }} else if let Some(__rg) = _rg_{f}.take() {{ crate::cir_trace::rwlock_read_unlock(Some(__rg), tag, {rq:?}, {sid:?}); }}"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::CondvarWait { condvar, lock } => {
            let c = g.field(*condvar);
            let l = g.field(*lock);
            let rq = g.resource_fqn(*condvar);
            g.emit(format!(
                "                _g_{l} = Some(crate::cir_trace::condvar_wait(&shared.{c}, _g_{l}.take().expect(\"wait without lock\"), tag, {rq:?}, {sid:?}));"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::CondvarNotify { condvar } => {
            let c = g.field(*condvar);
            let rq = g.resource_fqn(*condvar);
            g.emit(format!(
                "                crate::cir_trace::notify_one(&shared.{c}, tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::CondvarNotifyAll { condvar } => {
            let c = g.field(*condvar);
            let rq = g.resource_fqn(*condvar);
            g.emit(format!(
                "                crate::cir_trace::notify_all(&shared.{c}, tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::SemaphoreAcquire { resource, count } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "                shared.{f}.acquire_sid({count}, tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::SemaphoreRelease { resource, count } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "                shared.{f}.release_sid({count}, tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::Goto { target } => {
            g.emit(format!("                pc = {target};"));
        }
        SemOp::Branch {
            cond,
            then,
            else_target,
        } => {
            let c = g.render_expr(&mod_name, &func.name, sid, cond);
            g.emit(format!(
                "                pc = if {c} {{ {then} }} else {{ {else_target} }};"
            ));
        }
        SemOp::Switch {
            var,
            cases,
            default,
        } => {
            let v = g.render_expr(&mod_name, &func.name, sid, var);
            g.emit(format!("                pc = match {v} {{"));
            for (label, target) in cases {
                g.emit(format!("                    {label:?} => {target},"));
            }
            g.emit(format!("                    _ => {default},"));
            g.emit("                };");
        }
        SemOp::Return { value } => {
            if let Some(e) = value {
                let _ = g.render_expr(&mod_name, &func.name, sid, e);
            }
            g.emit("                return;");
        }
        SemOp::SeqHole {
            id,
            reads,
            writes,
            updates_modeled,
            expected_type,
            req,
            ..
        } => {
            let expected_ty = expected_rust_ty(g, func.module, expected_type);
            let hole_line = g.line();
            g.holes.push(Hole {
                id: id.clone(),
                module: mod_name.clone(),
                function: func.name.clone(),
                sid: sid.to_string(),
                expected: expected_ty.clone(),
                file: g.current_file.clone(),
                line: hole_line,
                reads: reads.clone(),
                writes: writes.clone(),
                req: req.clone(),
            });
            g.emit(format!(
                "                let _hole_val: {expected_ty} = /* HOLE({id}) expected: {expected_ty} */ Default::default();"
            ));
            if !updates_modeled.is_empty() {
                g.emit("                {");
                for (ui, u) in updates_modeled.iter().enumerate() {
                    let rhs = g.render_expr(&mod_name, &func.name, sid, &u.expr);
                    g.emit(format!("                    let __u{ui} = {rhs};"));
                }
                for (ui, u) in updates_modeled.iter().enumerate() {
                    match u.target {
                        SlotRef::Local(i) => {
                            g.emit(format!("                    _l{i} = __u{ui};"));
                        }
                        SlotRef::Shared(r) => {
                            let w = g.render_shared_write(r, &format!("__u{ui}"));
                            g.emit(format!("                    {w}"));
                        }
                        SlotRef::Discard => {}
                    }
                }
                g.emit("                }");
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::Scope { funcs } => {
            let tags = child_tags(func, sid);
            g.tags.push(TagRecord {
                sid: sid.to_string(),
                function: func.name.clone(),
                tags: tags.clone(),
            });
            g.emit(format!("                crate::cir_trace::scope(tag, {sid:?});"));
            g.emit("                {");
            g.emit("                    let mut __hs = Vec::new();");
            for (i, f) in funcs.iter().enumerate() {
                let cf = g.fn_ref(*f);
                let t = tags
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("t{sid}_{}", i + 1));
                g.emit("                    {");
                g.emit("                        let __sh = shared.clone();");
                g.emit(format!("                        __hs.push(std::thread::spawn(move || {cf}(__sh, {t:?})));"));
                g.emit("                    }");
            }
            g.emit("                    for __h in __hs { __h.join().unwrap(); }");
            g.emit("                }");
            g.emit(format!("                pc = {next};"));
        }
        SemOp::Spawn {
            func: child,
            handle,
        } => {
            let tags = child_tags(func, sid);
            g.tags.push(TagRecord {
                sid: sid.to_string(),
                function: func.name.clone(),
                tags: tags.clone(),
            });
            let cf = g.fn_ref(*child);
            let t = tags.first().cloned().unwrap_or_else(|| format!("t{sid}"));
            let h = format!("_h_{}", sanitize(handle));
            let child_fqn = g.function_fqn(*child);
            g.emit("                {");
            g.emit("                    let __sh = shared.clone();");
            g.emit(format!(
                "                    let {h} = crate::cir_trace::spawn(tag, {child_fqn:?}, {sid:?}, move || {cf}(__sh, {t:?}));"
            ));
            g.emit("                }");
            g.emit(format!("                pc = {next};"));
        }
        SemOp::Join { handle } => {
            let h = format!("_h_{}", sanitize(handle));
            g.emit(format!(
                "                crate::cir_trace::join({h}, tag, \"\", {sid:?});"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::Call {
            func: callee,
            args,
            dst,
        } => {
            let cf = g.fn_ref(*callee);
            for a in args {
                let _ = g.render_expr(&mod_name, &func.name, sid, a);
            }
            if dst.is_some() {
                g.emit(format!(
                    "                let _ = {cf}(shared.clone(), tag);"
                ));
            } else {
                g.emit(format!("                {cf}(shared.clone(), tag);"));
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::AtomicLoad { resource, dst } => {
            let rd = g.render_shared_read(*resource);
            match dst {
                SlotRef::Local(i) => {
                    g.emit(format!("                _l{i} = {rd};"));
                }
                SlotRef::Shared(r) => {
                    let w = g.render_shared_write(*r, "__v");
                    g.emit(format!("                {{ let __v = {rd}; {w} }}"));
                }
                SlotRef::Discard => {
                    g.emit(format!("                let _ = {rd};"));
                }
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::AtomicStore { resource, value } => {
            let e = g.render_expr(&mod_name, &func.name, sid, value);
            let w = g.render_shared_write(*resource, "__v");
            g.emit(format!("                {{ let __v = {e}; {w} }}"));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::AtomicCas {
            resource,
            expected,
            desired,
            dst,
        } => {
            let f = g.field(*resource);
            let ex = g.render_expr(&mod_name, &func.name, sid, expected);
            let de = g.render_expr(&mod_name, &func.name, sid, desired);
            if g.is_std_atomic(*resource) {
                let assign = match dst {
                    SlotRef::Local(i) => format!("_l{i} = __old;"),
                    SlotRef::Shared(r) => g.render_shared_write(*r, "__old"),
                    SlotRef::Discard => "let _ = __old;".to_string(),
                };
                g.emit(format!(
                    "                {{ let __old = match shared.{f}.compare_exchange({ex}, {de}, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst) {{ Ok(v) | Err(v) => v }}; {assign} }}"
                ));
            } else {
                let assign = match dst {
                    SlotRef::Local(i) => format!("_l{i} = __old;"),
                    SlotRef::Shared(r) => g.render_shared_write(*r, "__old"),
                    SlotRef::Discard => "let _ = __old;".to_string(),
                };
                g.emit(format!(
                    "                {{ let mut __g = shared.{f}.lock().unwrap(); let __old = __g.clone(); if __old == {ex} {{ *__g = {de}; }} drop(__g); {assign} }}"
                ));
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::ChannelSend { channel, value } => {
            let f = g.field(*channel);
            let rq = g.resource_fqn(*channel);
            let e = g.render_expr(&mod_name, &func.name, sid, value);
            g.emit(format!(
                "                shared.{f}.send_sid({e}, tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::ChannelRecv { channel, dst } => {
            let f = g.field(*channel);
            let rq = g.resource_fqn(*channel);
            match dst {
                SlotRef::Local(i) => {
                    g.emit(format!(
                        "                _l{i} = shared.{f}.recv_sid(tag, {rq:?}, {sid:?});"
                    ));
                }
                SlotRef::Shared(r) => {
                    let w = g.render_shared_write(*r, "__v");
                    g.emit(format!(
                        "                {{ let __v = shared.{f}.recv_sid(tag, {rq:?}, {sid:?}); {w} }}"
                    ));
                }
                SlotRef::Discard => {
                    g.emit(format!(
                        "                let _ = shared.{f}.recv_sid(tag, {rq:?}, {sid:?});"
                    ));
                }
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::Unsupported { construct } => {
            return Err(format!("unsupported construct in codegen: {construct}"));
        }
    }
    let _ = (body_len, guards);
    Ok(())
}

