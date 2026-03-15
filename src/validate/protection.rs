use std::collections::HashSet;

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::validate::types::{build_resource_type_map, ResType};

/// E7xx: Protection mapping checks.
pub fn check(program: &Program, source: &str, diags: &mut Vec<Diagnostic>) {
    let rt_map = build_resource_type_map(program);

    let mut protected_vars = HashSet::new();

    for prot in &program.protections {
        let var_name = &prot.var_name.value;
        let lock_name = &prot.lock_name.value;

        // E701: left side must be Var
        if let Some(rt) = rt_map.get(var_name) {
            match rt {
                ResType::Var(_) => {
                    protected_vars.insert(var_name.clone());
                }
                ResType::Atomic(_) => {
                    diags.push(
                        Diagnostic::error(
                            "E703",
                            format!("Atomic resource '{var_name}' should not be in protection mapping"),
                        )
                        .with_span(prot.var_name.span, source)
                        .with_fix("remove this protection entry; Atomic resources don't need lock protection"),
                    );
                }
                _ => {
                    diags.push(
                        Diagnostic::error(
                            "E701",
                            format!("protection target '{var_name}' is not a Var-typed resource"),
                        )
                        .with_span(prot.var_name.span, source)
                        .with_fix("use a Var-typed resource name on the left side of '->'"),
                    );
                }
            }
        }

        // E702: right side must be Mutex or RwLock
        if let Some(rt) = rt_map.get(lock_name) {
            if !matches!(rt, ResType::Mutex | ResType::RwLock) {
                diags.push(
                    Diagnostic::error(
                        "E702",
                        format!("protection lock '{lock_name}' is not a Mutex or RwLock"),
                    )
                    .with_span(prot.lock_name.span, source)
                    .with_fix("use a Mutex or RwLock resource name on the right side of '->'"),
                );
            }
        }
    }

    // E704: Var without protection (warning)
    for r in &program.resources {
        if let ResourceKind::Var(VarType::Var(_)) = &r.kind {
            if !protected_vars.contains(&r.name.value) {
                diags.push(
                    Diagnostic::warning(
                        "E704",
                        format!(
                            "Var resource '{}' has no protection mapping",
                            r.name.value
                        ),
                    )
                    .with_span(r.name.span, source)
                    .with_fix("add a protection mapping or change to Atomic type"),
                );
            }
        }
    }
}
