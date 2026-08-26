use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::validate::types::{build_resource_type_map, ResType};

/// E3xx: Resource-operation compatibility checks.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let rt_map = build_resource_type_map(program);

    program.walk_blocks(|mi, fi, si, _, _, block| {
        let Some(call) = &block.call else {
            return;
        };
        let path = format!("{}.call", Program::block_path(mi, fi, si));
        match call {
            Call::MutexLock { resource, .. } | Call::MutexUnlock { resource, .. } => {
                check_rt(&rt_map, diags, &path, resource, |rt| {
                    matches!(rt, ResType::Mutex)
                }, "E301", "mutex_lock/unlock requires a Mutex");
            }
            Call::RwLockRead { resource, .. }
            | Call::RwLockWrite { resource, .. }
            | Call::RwLockUnlock { resource, .. } => {
                check_rt(&rt_map, diags, &path, resource, |rt| {
                    matches!(rt, ResType::RwLock)
                }, "E302", "rwlock_* requires an RwLock");
            }
            Call::CondvarWait { condvar, lock, .. } => {
                check_rt(&rt_map, diags, &path, condvar, |rt| {
                    matches!(rt, ResType::Condvar)
                }, "E303", "condvar_wait requires a Condvar");
                if let Some(lock_rt) = rt_map.get(lock) {
                    if !matches!(lock_rt, ResType::Mutex | ResType::RwLock) {
                        diags.push(
                            Diagnostic::error(
                                "E304",
                                format!("wait lock '{lock}' is not a Mutex or RwLock"),
                            )
                            .with_path(path.clone())
                            .with_fix("specify a Mutex or RwLock as the wait lock"),
                        );
                    }
                }
            }
            Call::CondvarNotify { condvar, .. } | Call::CondvarNotifyAll { condvar, .. } => {
                check_rt(&rt_map, diags, &path, condvar, |rt| {
                    matches!(rt, ResType::Condvar)
                }, "E303", "condvar_notify requires a Condvar");
            }
            Call::SemaphoreAcquire { resource, .. } | Call::SemaphoreRelease { resource, .. } => {
                check_rt(&rt_map, diags, &path, resource, |rt| {
                    matches!(rt, ResType::Semaphore)
                }, "E305", "semaphore_* requires a Semaphore");
            }
            Call::ChannelSend { channel, .. } | Call::ChannelRecv { channel, .. } => {
                check_rt(&rt_map, diags, &path, channel, |rt| {
                    matches!(rt, ResType::Channel(_))
                }, "E306", "channel_send/recv requires a Channel");
            }
            Call::AtomicLoad { resource, .. }
            | Call::AtomicStore { resource, .. }
            | Call::AtomicCas { resource, .. } => {
                check_rt(&rt_map, diags, &path, resource, |rt| {
                    matches!(rt, ResType::Atomic(_))
                }, "E307", "atomic_* requires an Atomic");
            }
            _ => {}
        }
        for stmt in &block.statements {
            if let Stmt::ReadShared { resource, .. } | Stmt::WriteShared { resource, .. } = stmt {
                if let Some(rt) = rt_map.get(resource) {
                    if !matches!(rt, ResType::Var(_)) {
                        diags.push(
                            Diagnostic::error(
                                "E308",
                                format!("cannot read/write non-Var resource '{resource}'"),
                            )
                            .with_path(format!("{}.statements", Program::block_path(mi, fi, si)))
                            .with_fix("use a Var-typed resource"),
                        );
                    }
                }
            }
        }
    });
}

fn check_rt(
    rt_map: &std::collections::HashMap<String, ResType>,
    diags: &mut Vec<Diagnostic>,
    path: &str,
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
                .with_fix("use the matching resource type"),
        );
    }
}
