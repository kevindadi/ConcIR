use std::collections::HashSet;

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::validate::types::{build_resource_type_map, ResType};

/// E7xx: Protection mapping checks.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let rt_map = build_resource_type_map(program);

    let mut protected_vars = HashSet::new();
    let mut seen_vars = HashSet::new();

    for (pi, prot) in program.protection.iter().enumerate() {
        let var_name = &prot.var;
        let lock_name = &prot.lock;
        let prot_path = format!("protection[{pi}]");

        // E705: duplicate protection
        if !seen_vars.insert(var_name.clone()) {
            diags.push(
                Diagnostic::error(
                    "E705",
                    format!("duplicate protection entry for Var '{var_name}'"),
                )
                .with_path(prot_path.clone())
                .with_fix("remove the duplicate entry"),
            );
        }

        // E701 / E703: left side must be Var
        if let Some(rt) = rt_map.get(var_name) {
            match rt {
                ResType::Var(_) => {
                    protected_vars.insert(var_name.clone());
                }
                ResType::Atomic(_) => {
                    diags.push(
                        Diagnostic::error(
                            "E703",
                            format!(
                                "Atomic resource '{var_name}' should not be in protection mapping"
                            ),
                        )
                        .with_path(format!("{prot_path}.var"))
                        .with_fix(
                            "remove this protection entry; Atomic resources don't need lock protection",
                        ),
                    );
                }
                _ => {
                    diags.push(
                        Diagnostic::error(
                            "E701",
                            format!("protection target '{var_name}' is not a Var-typed resource"),
                        )
                        .with_path(format!("{prot_path}.var"))
                        .with_fix("use a Var-typed resource name"),
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
                    .with_path(format!("{prot_path}.lock"))
                    .with_fix("use a Mutex or RwLock resource name"),
                );
            }
        }
    }

    // E704: Var without protection (warning)
    for (i, r) in program.resources.iter().enumerate() {
        if r.kind == "var" && r.res_type == "Var" && !protected_vars.contains(&r.name) {
            diags.push(
                Diagnostic::warning(
                    "E704",
                    format!("Var resource '{}' has no protection mapping", r.name),
                )
                .with_path(format!("resources[{i}]"))
                .with_fix("add a protection mapping or change to Atomic type"),
            );
        }
    }
}
