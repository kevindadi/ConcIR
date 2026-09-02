use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::validate::types::{build_resource_type_map, ResType};

/// E3xx: Resource-operation compatibility checks.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let rt_map = build_resource_type_map(program);

    program.walk_stmts(|mi, fi, si, m, f, stmt| {
        let path = Program::stmt_path(mi, fi, si);
        let loc = Program::stmt_location(m, f, stmt);
        check_op(&rt_map, diags, &path, &loc, &stmt.op);
        if let Op::Select { branches, .. } = &stmt.op {
            for branch in branches {
                match &branch.guard {
                    SelectGuard::ChannelRecv { channel, .. } => {
                        check_rt(
                            &rt_map,
                            diags,
                            &path,
                            &loc,
                            channel,
                            |rt| matches!(rt, ResType::Channel(_)),
                            "E306",
                            "channel_recv requires a Channel",
                        );
                    }
                    SelectGuard::CondvarWait { condvar, lock } => {
                        check_rt(
                            &rt_map,
                            diags,
                            &path,
                            &loc,
                            condvar,
                            |rt| matches!(rt, ResType::Condvar),
                            "E303",
                            "condvar_wait requires a Condvar",
                        );
                        if let Some(lock_rt) = rt_map.get(lock) {
                            if !matches!(lock_rt, ResType::Mutex | ResType::RwLock) {
                                diags.push(
                                    Diagnostic::error(
                                        "E304",
                                        format!("wait lock '{lock}' is not a Mutex or RwLock"),
                                    )
                                    .with_path(path.clone())
                                    .with_location(&loc)
                                    .with_fix("specify a Mutex or RwLock as the wait lock"),
                                );
                            }
                        }
                    }
                    SelectGuard::SemaphoreAcquire { resource } => {
                        check_rt(
                            &rt_map,
                            diags,
                            &path,
                            &loc,
                            resource,
                            |rt| matches!(rt, ResType::Semaphore),
                            "E305",
                            "semaphore_* requires a Semaphore",
                        );
                    }
                }
            }
        }
    });
}

fn check_op(
    rt_map: &std::collections::HashMap<String, ResType>,
    diags: &mut Vec<Diagnostic>,
    path: &str,
    location: &str,
    op: &Op,
) {
    match op {
        Op::MutexLock { resource } | Op::MutexUnlock { resource } => {
            check_rt(
                rt_map,
                diags,
                path,
                location,
                resource,
                |rt| matches!(rt, ResType::Mutex),
                "E301",
                "mutex_lock/unlock requires a Mutex",
            );
        }
        Op::RwLockRead { resource }
        | Op::RwLockWrite { resource }
        | Op::RwLockUnlock { resource } => {
            check_rt(
                rt_map,
                diags,
                path,
                location,
                resource,
                |rt| matches!(rt, ResType::RwLock),
                "E302",
                "rwlock_* requires an RwLock",
            );
        }
        Op::CondvarWait { condvar, lock } => {
            check_rt(
                rt_map,
                diags,
                path,
                location,
                condvar,
                |rt| matches!(rt, ResType::Condvar),
                "E303",
                "condvar_wait requires a Condvar",
            );
            if let Some(lock_rt) = rt_map.get(lock) {
                if !matches!(lock_rt, ResType::Mutex | ResType::RwLock) {
                    diags.push(
                        Diagnostic::error(
                            "E304",
                            format!("wait lock '{lock}' is not a Mutex or RwLock"),
                        )
                        .with_path(path.to_string())
                        .with_location(location)
                        .with_fix("specify a Mutex or RwLock as the wait lock"),
                    );
                }
            }
        }
        Op::CondvarNotify { condvar } | Op::CondvarNotifyAll { condvar } => {
            check_rt(
                rt_map,
                diags,
                path,
                location,
                condvar,
                |rt| matches!(rt, ResType::Condvar),
                "E303",
                "condvar_notify requires a Condvar",
            );
        }
        Op::SemaphoreAcquire { resource, .. } | Op::SemaphoreRelease { resource, .. } => {
            check_rt(
                rt_map,
                diags,
                path,
                location,
                resource,
                |rt| matches!(rt, ResType::Semaphore),
                "E305",
                "semaphore_* requires a Semaphore",
            );
        }
        Op::ChannelSend { channel, .. } | Op::ChannelRecv { channel, .. } => {
            check_rt(
                rt_map,
                diags,
                path,
                location,
                channel,
                |rt| matches!(rt, ResType::Channel(_)),
                "E306",
                "channel_send/recv requires a Channel",
            );
        }
        Op::AtomicLoad { resource, .. }
        | Op::AtomicStore { resource, .. }
        | Op::AtomicCas { resource, .. } => {
            check_rt(
                rt_map,
                diags,
                path,
                location,
                resource,
                |rt| matches!(rt, ResType::Atomic(_)),
                "E307",
                "atomic_* requires an Atomic",
            );
        }
        Op::SeqHole { reads, writes, .. } => {
            for name in reads.iter().chain(writes.iter()) {
                if let Some(rt) = rt_map.get(name) {
                    if !matches!(rt, ResType::Var(_) | ResType::Atomic(_)) {
                        diags.push(
                            Diagnostic::error(
                                "E310",
                                format!(
                                    "seq_hole footprint '{name}' is a sync primitive; sequential \
                                     holes may name only Var or Atomic"
                                ),
                            )
                            .with_path(path.to_string())
                            .with_location(location)
                            .with_fix(
                                "move lock/wait/send into the skeleton; keep seq_hole to sequential data",
                            ),
                        );
                    }
                }
            }
        }
        Op::ReadShared { resource, .. } | Op::WriteShared { resource, .. } => {
            if let Some(rt) = rt_map.get(resource) {
                if !matches!(rt, ResType::Var(_)) {
                    diags.push(
                        Diagnostic::error(
                            "E308",
                            format!("cannot read/write non-Var resource '{resource}'"),
                        )
                        .with_path(path.to_string())
                        .with_location(location)
                        .with_fix("use a Var-typed resource"),
                    );
                }
            }
        }
        _ => {}
    }
}

fn check_rt(
    rt_map: &std::collections::HashMap<String, ResType>,
    diags: &mut Vec<Diagnostic>,
    path: &str,
    location: &str,
    name: &str,
    ok: impl Fn(&ResType) -> bool,
    code: &'static str,
    msg: &str,
) {
    let Some(rt) = rt_map.get(name) else {
        return;
    };
    if !ok(rt) {
        diags.push(
            Diagnostic::error(code, format!("{msg} (resource '{name}')"))
                .with_path(path.to_string())
                .with_location(location)
                .with_fix("use the matching resource type"),
        );
    }
}
