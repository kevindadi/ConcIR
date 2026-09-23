pub mod compat;
pub mod concurrency;
pub mod control;
pub mod dataflow;
pub mod interface;
pub mod locks;
pub mod names;
pub mod protection;
pub mod structure;
pub mod types;

use crate::ast::Program;
use crate::diagnostic::{Diagnostic, Severity, ValidationReport};

/// Run all validation passes on a parsed ConcIR program, returning the full report.
/// Order: E0xx → E1xx → E2xx → E3xx → E7xx → E4xx → E5xx → E8xx → E6xx → E9xx.
pub fn validate(program: &Program) -> ValidationReport {
    let mut diags = Vec::new();

    structure::check(program, &mut diags);
    names::check(program, &mut diags);
    crate::typedef::check(program, &mut diags);
    types::check(program, &mut diags);
    compat::check(program, &mut diags);
    protection::check(program, &mut diags);
    concurrency::check(program, &mut diags);
    locks::check(program, &mut diags);
    interface::check(program, &mut diags);
    control::check(program, &mut diags);
    dataflow::check(program, &mut diags);

    unused_resources(program, &mut diags);

    let valid = !diags.iter().any(|d| d.severity == Severity::Error);

    ValidationReport {
        valid,
        diagnostics: diags,
    }
}

/// W201: a resource declared by a module that no statement, protection entry or
/// dependency references. Warning only; it does not change PASS/INVALID.
fn unused_resources(program: &Program, diags: &mut Vec<Diagnostic>) {
    use crate::ast::Op;
    use std::collections::HashSet;

    fn last(s: &str) -> &str {
        s.rsplit("::").next().unwrap_or(s)
    }
    fn note_resource(used: &mut HashSet<String>, r: &str) {
        used.insert(last(r).to_string());
    }

    for module in &program.modules {
        let mut used: HashSet<String> = HashSet::new();
        for r in &module.requires.resources {
            note_resource(&mut used, r);
        }
        for prot in &module.protection {
            note_resource(&mut used, &prot.var);
            note_resource(&mut used, &prot.lock);
        }
        for f in &module.functions {
            for st in &f.body {
                match &st.op {
                    Op::ReadShared { resource, .. } | Op::WriteShared { resource, .. }
                    | Op::AtomicLoad { resource, .. } | Op::AtomicStore { resource, .. }
                    | Op::AtomicCas { resource, .. } | Op::MutexLock { resource }
                    | Op::MutexUnlock { resource } | Op::RwLockRead { resource }
                    | Op::RwLockWrite { resource } | Op::RwLockUnlock { resource }
                    | Op::SemaphoreAcquire { resource, .. }
                    | Op::SemaphoreRelease { resource, .. } => note_resource(&mut used, resource),
                    Op::ChannelSend { channel, .. } | Op::ChannelRecv { channel, .. } => {
                        note_resource(&mut used, channel)
                    }
                    Op::CondvarWait { condvar, lock } => {
                        note_resource(&mut used, condvar);
                        note_resource(&mut used, lock);
                    }
                    Op::CondvarNotify { condvar } | Op::CondvarNotifyAll { condvar } => {
                        note_resource(&mut used, condvar)
                    }
                    Op::AbstractStep { reads, writes, .. }
                    | Op::SeqHole { reads, writes, .. } => {
                        for r in reads.iter().chain(writes.iter()) {
                            note_resource(&mut used, r);
                        }
                    }
                    _ => {}
                }
            }
        }
        for res in &module.resources {
            if !used.contains(res.name.as_str()) {
                diags.push(Diagnostic {
                    code: "W201",
                    severity: Severity::Warning,
                    message: format!(
                        "resource '{}::{}' is declared but never used",
                        module.name, res.name
                    ),
                    location: Some(format!("{}::{}", module.name, res.name)),
                    path: None,
                    fix_hint: Some("remove the resource, or use it in a statement/protection".into()),
                });
            }
        }
    }
}
