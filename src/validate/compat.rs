use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::validate::types::{build_resource_type_map, ResType};

/// E3xx: Resource-operation compatibility checks.
pub fn check(program: &Program, source: &str, diags: &mut Vec<Diagnostic>) {
    let rt_map = build_resource_type_map(program);

    for f in &program.functions {
        for stmt in &f.statements {
            if let Op::ResOp(ref res, ref action) = stmt.op {
                let rt = match rt_map.get(&res.value) {
                    Some(r) => r,
                    None => continue,
                };
                check_action_compat(diags, source, stmt, &res.value, rt, action);

                // E304: wait(lock_name) — lock_name must be Mutex or RwLock
                if let Action::Wait(ref lock_ident) = action {
                    if let Some(lock_rt) = rt_map.get(&lock_ident.value) {
                        if !matches!(lock_rt, ResType::Mutex | ResType::RwLock) {
                            diags.push(
                                Diagnostic::error(
                                    "E304",
                                    format!(
                                        "wait() lock '{}' is not a Mutex or RwLock",
                                        lock_ident.value
                                    ),
                                )
                                .with_span(lock_ident.span, source)
                                .with_fix(
                                    "specify a Mutex or RwLock resource as the wait lock",
                                ),
                            );
                        }
                    }
                }
            }
        }
    }
}

fn check_action_compat(
    diags: &mut Vec<Diagnostic>,
    source: &str,
    stmt: &Statement,
    res_name: &str,
    rt: &ResType,
    action: &Action,
) {
    match action {
        Action::Lock | Action::Drop => {
            if !matches!(rt, ResType::Mutex | ResType::RwLock) {
                diags.push(
                    Diagnostic::error(
                        "E301",
                        format!("cannot lock/drop non-Mutex/RwLock resource '{res_name}'"),
                    )
                    .with_span(stmt.span, source)
                    .with_fix("use the correct action for this resource type"),
                );
            }
        }
        Action::Read => match rt {
            ResType::RwLock => {}
            ResType::Var(_) => {}
            ResType::Mutex => {
                diags.push(
                    Diagnostic::error(
                        "E302",
                        format!("cannot read-lock Mutex '{res_name}'; use 'lock' instead"),
                    )
                    .with_span(stmt.span, source)
                    .with_fix("change action to 'lock', or change resource to RwLock"),
                );
            }
            _ => {
                diags.push(
                    Diagnostic::error(
                        "E308",
                        format!("cannot read/write non-Var resource '{res_name}'"),
                    )
                    .with_span(stmt.span, source)
                    .with_fix("use the correct action for this resource type"),
                );
            }
        },
        Action::Write(_) => {
            if !matches!(rt, ResType::Var(_)) {
                diags.push(
                    Diagnostic::error(
                        "E308",
                        format!("cannot read/write non-Var resource '{res_name}'"),
                    )
                    .with_span(stmt.span, source)
                    .with_fix("use a Var-typed resource or change the action"),
                );
            }
        }
        Action::Wait(_) | Action::Notify | Action::NotifyAll => {
            if !matches!(rt, ResType::Condvar) {
                diags.push(
                    Diagnostic::error(
                        "E303",
                        format!("cannot wait/notify on non-Condvar resource '{res_name}'"),
                    )
                    .with_span(stmt.span, source)
                    .with_fix("use a Condvar resource or change the action"),
                );
            }
        }
        Action::Acquire | Action::Release => {
            if !matches!(rt, ResType::Semaphore) {
                diags.push(
                    Diagnostic::error(
                        "E305",
                        format!("cannot acquire/release non-Semaphore resource '{res_name}'"),
                    )
                    .with_span(stmt.span, source)
                    .with_fix("use a Semaphore resource or change the action"),
                );
            }
        }
        Action::Send(_) | Action::Recv => {
            if !matches!(rt, ResType::Channel(_)) {
                diags.push(
                    Diagnostic::error(
                        "E306",
                        format!("cannot send/recv on non-Channel resource '{res_name}'"),
                    )
                    .with_span(stmt.span, source)
                    .with_fix("use a Channel resource or change the action"),
                );
            }
        }
        Action::Load | Action::Store(_) | Action::Cas(_, _) => {
            if !matches!(rt, ResType::Atomic(_)) {
                diags.push(
                    Diagnostic::error(
                        "E307",
                        format!("cannot load/store/cas on non-Atomic resource '{res_name}'"),
                    )
                    .with_span(stmt.span, source)
                    .with_fix("use an Atomic resource or change the action"),
                );
            }
        }
    }
}
