use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::validate::types::{build_resource_type_map, ResType};

/// E3xx: Resource-operation compatibility checks.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let rt_map = build_resource_type_map(program);

    for (fi, f) in program.functions.iter().enumerate() {
        for (si, stmt) in f.body.iter().enumerate() {
            if let Op::ResOp {
                ref resource,
                ref action,
                ref args,
            } = stmt.op
            {
                let rt = match rt_map.get(resource) {
                    Some(r) => r,
                    None => continue,
                };
                let op_path = format!("functions[{fi}].body[{si}].op");
                check_action_compat(diags, &op_path, resource, rt, action);

                // E304: wait(lock_name) — lock_name must be Mutex or RwLock
                if action == "wait" {
                    if let Some(lock_name) = args.first() {
                        if let Some(lock_rt) = rt_map.get(lock_name.as_str()) {
                            if !matches!(lock_rt, ResType::Mutex | ResType::RwLock) {
                                diags.push(
                                    Diagnostic::error(
                                        "E304",
                                        format!(
                                            "wait() lock '{lock_name}' is not a Mutex or RwLock"
                                        ),
                                    )
                                    .with_path(format!("{op_path}[3]"))
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
}

fn check_action_compat(
    diags: &mut Vec<Diagnostic>,
    path: &str,
    res_name: &str,
    rt: &ResType,
    action: &str,
) {
    match action {
        "lock" | "drop" if !matches!(rt, ResType::Mutex | ResType::RwLock) => {
            diags.push(
                Diagnostic::error(
                    "E301",
                    format!("cannot lock/drop non-Mutex/RwLock resource '{res_name}'"),
                )
                .with_path(path.to_string())
                .with_fix("use the correct action for this resource type"),
            );
        }
        "read" => match rt {
            ResType::RwLock => {}
            ResType::Var(_) => {}
            ResType::Mutex => {
                diags.push(
                    Diagnostic::error(
                        "E302",
                        format!("cannot read-lock Mutex '{res_name}'; use 'lock' instead"),
                    )
                    .with_path(path.to_string())
                    .with_fix("change action to 'lock', or change resource to RwLock"),
                );
            }
            _ => {
                diags.push(
                    Diagnostic::error(
                        "E308",
                        format!("cannot read/write non-Var resource '{res_name}'"),
                    )
                    .with_path(path.to_string())
                    .with_fix("use the correct action for this resource type"),
                );
            }
        },
        "write" if !matches!(rt, ResType::Var(_)) => {
            diags.push(
                Diagnostic::error(
                    "E308",
                    format!("cannot read/write non-Var resource '{res_name}'"),
                )
                .with_path(path.to_string())
                .with_fix("use a Var-typed resource or change the action"),
            );
        }
        "wait" | "notify" | "notify_all" if !matches!(rt, ResType::Condvar) => {
            diags.push(
                Diagnostic::error(
                    "E303",
                    format!("cannot wait/notify on non-Condvar resource '{res_name}'"),
                )
                .with_path(path.to_string())
                .with_fix("use a Condvar resource or change the action"),
            );
        }
        "acquire" | "release" if !matches!(rt, ResType::Semaphore) => {
            diags.push(
                Diagnostic::error(
                    "E305",
                    format!("cannot acquire/release non-Semaphore resource '{res_name}'"),
                )
                .with_path(path.to_string())
                .with_fix("use a Semaphore resource or change the action"),
            );
        }
        "send" | "recv" if !matches!(rt, ResType::Channel(_)) => {
            diags.push(
                Diagnostic::error(
                    "E306",
                    format!("cannot send/recv on non-Channel resource '{res_name}'"),
                )
                .with_path(path.to_string())
                .with_fix("use a Channel resource or change the action"),
            );
        }
        "load" | "store" | "cas" if !matches!(rt, ResType::Atomic(_)) => {
            diags.push(
                Diagnostic::error(
                    "E307",
                    format!("cannot load/store/cas on non-Atomic resource '{res_name}'"),
                )
                .with_path(path.to_string())
                .with_fix("use an Atomic resource or change the action"),
            );
        }
        _ => {}
    }
}
