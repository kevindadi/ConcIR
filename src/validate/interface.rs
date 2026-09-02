//! E8xx: function concurrency interface and imported signatures.

use std::collections::HashSet;

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::fqn;
use crate::validate::types::{build_resource_type_map, ResType};

/// E801–E805: lock-effect well-formedness, `may_block` vs body, import sig match.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let rt_map = build_resource_type_map(program);
    check_declared_interfaces(program, &rt_map, diags);
    check_import_sigs(program, diags);
}

fn check_declared_interfaces(
    program: &Program,
    rt_map: &std::collections::HashMap<String, ResType>,
    diags: &mut Vec<Diagnostic>,
) {
    for (mi, m) in program.modules.iter().enumerate() {
        for (fi, f) in m.functions.iter().enumerate() {
            let fn_path = Program::fn_path(mi, fi);
            check_lock_effect_names(program, m, f, &fn_path, rt_map, diags);
            check_may_block_vs_body(m, f, &fn_path, diags);
        }
    }
}

fn check_lock_effect_names(
    program: &Program,
    module: &Module,
    f: &Function,
    fn_path: &str,
    rt_map: &std::collections::HashMap<String, ResType>,
    diags: &mut Vec<Diagnostic>,
) {
    for (field, names) in [
        ("acquires", f.locks.acquires.as_slice()),
        ("releases", f.locks.releases.as_slice()),
        ("requires_held", f.locks.requires_held.as_slice()),
    ] {
        let mut seen = HashSet::new();
        for (i, name) in names.iter().enumerate() {
            let path = format!("{fn_path}.locks.{field}[{i}]");
            if !seen.insert(name.as_str()) {
                diags.push(
                    Diagnostic::error(
                        "E801",
                        format!(
                            "duplicate lock '{name}' in {field} of function '{}'",
                            f.name
                        ),
                    )
                    .with_path(path.clone())
                    .with_location(Program::fn_location(module, f))
                    .with_fix("list each lock at most once in this field"),
                );
            }
            if program.lookup_resource(&module.name, name).is_none() {
                diags.push(
                    Diagnostic::error(
                        "E801",
                        format!(
                            "lock '{name}' in {field} of function '{}' is not a declared resource",
                            f.name
                        ),
                    )
                    .with_path(path)
                    .with_location(Program::fn_location(module, f))
                    .with_fix("name a Mutex or RwLock visible in this module"),
                );
                continue;
            }
            let Some(rt) = rt_map.get(name).or_else(|| {
                let q = fqn::qualify(&module.name, name);
                rt_map.get(&q)
            }) else {
                continue;
            };
            if !matches!(rt, ResType::Mutex | ResType::RwLock) {
                diags.push(
                    Diagnostic::error(
                        "E801",
                        format!(
                            "lock '{name}' in {field} of function '{}' is not a Mutex or RwLock",
                            f.name
                        ),
                    )
                    .with_path(path)
                    .with_location(Program::fn_location(module, f))
                    .with_fix("use a Mutex or RwLock name"),
                );
            }
        }
    }
}

fn check_may_block_vs_body(
    module: &Module,
    f: &Function,
    fn_path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let Some(declared) = f.may_block else {
        return;
    };
    if f.body.is_empty() {
        return;
    }
    let inferred = f.body_may_block();
    if declared && !inferred {
        diags.push(
            Diagnostic::warning(
                "E802",
                format!(
                    "function '{}' declares may_block: true but its body has no blocking operation",
                    f.name
                ),
            )
            .with_path(format!("{fn_path}.may_block"))
            .with_location(Program::fn_location(module, f))
            .with_fix("set may_block to false, or add a blocking statement"),
        );
    } else if !declared && inferred {
        diags.push(
            Diagnostic::error(
                "E802",
                format!(
                    "function '{}' declares may_block: false but its body contains a blocking operation",
                    f.name
                ),
            )
            .with_path(format!("{fn_path}.may_block"))
            .with_location(Program::fn_location(module, f))
            .with_fix("set may_block to true, or remove the blocking operation"),
        );
    }
}

fn check_import_sigs(program: &Program, diags: &mut Vec<Diagnostic>) {
    for (mi, m) in program.modules.iter().enumerate() {
        for (i, req) in m.requires.functions.iter().enumerate() {
            let Some(sig) = req.sig() else {
                continue;
            };
            let path = format!("modules[{mi}].requires.functions[{i}]");
            if !fqn::is_fqn(&sig.name) {
                diags.push(
                    Diagnostic::error(
                        "E804",
                        format!(
                            "imported function signature '{}' must use an FQN (module::entity)",
                            sig.name
                        ),
                    )
                    .with_path(path)
                    .with_fix("write the name as module::function"),
                );
                continue;
            }
            let Some((_, defn)) = program.lookup_function(&m.name, &sig.name) else {
                continue;
            };
            if let Some(kind) = &sig.kind {
                if kind != &defn.kind {
                    diags.push(
                        Diagnostic::error(
                            "E804",
                            format!(
                                "imported signature '{}' has kind '{kind}', but the definition is '{}'",
                                sig.name, defn.kind
                            ),
                        )
                        .with_path(format!("{path}.kind"))
                        .with_fix("copy kind from the defining function"),
                    );
                }
            }
            if let Some(imported) = sig.may_block {
                if let Some(defined) = defn.effective_may_block() {
                    if imported != defined {
                        diags.push(
                            Diagnostic::error(
                                "E804",
                                format!(
                                    "imported signature '{}' has may_block: {imported}, but the \
                                     definition is {defined}",
                                    sig.name
                                ),
                            )
                            .with_path(format!("{path}.may_block"))
                            .with_fix("copy may_block from the defining function"),
                        );
                    }
                }
            }
            if !sig.locks.is_empty() && sig.locks != defn.locks {
                diags.push(
                    Diagnostic::error(
                        "E804",
                        format!(
                            "imported signature '{}' lock protocol does not match the definition",
                            sig.name
                        ),
                    )
                    .with_path(format!("{path}.locks"))
                    .with_fix(
                        "copy acquires / releases / requires_held from the defining function",
                    ),
                );
            }
            if !sig.params.is_empty() && sig.params != defn.params {
                diags.push(
                    Diagnostic::error(
                        "E804",
                        format!(
                            "imported signature '{}' params do not match the definition",
                            sig.name
                        ),
                    )
                    .with_path(format!("{path}.params"))
                    .with_fix("copy params from the defining function"),
                );
            }
            if sig.returns.is_some() && sig.returns != defn.returns {
                diags.push(
                    Diagnostic::error(
                        "E804",
                        format!(
                            "imported signature '{}' returns do not match the definition",
                            sig.name
                        ),
                    )
                    .with_path(format!("{path}.returns"))
                    .with_fix("copy returns from the defining function"),
                );
            }
        }
    }
}
