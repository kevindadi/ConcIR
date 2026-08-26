use crate::ast::*;
use crate::diagnostic::Diagnostic;

/// E0xx: Structural validation (post-deserialization checks that serde cannot enforce).
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    check_resources(program, diags);
    check_functions(program, diags);
}

fn check_resources(program: &Program, diags: &mut Vec<Diagnostic>) {
    for (mi, m) in program.modules.iter().enumerate() {
        for (i, r) in m.resources.iter().enumerate() {
            let path_prefix = format!("modules[{mi}].resources[{i}]");

            // E008: kind must be "sync" or "var"
            if r.kind != "sync" && r.kind != "var" {
                diags.push(
                    Diagnostic::error(
                        "E008",
                        format!("resource '{}' has invalid kind '{}'", r.name, r.kind),
                    )
                    .with_path(format!("{path_prefix}.kind"))
                    .with_fix("kind must be \"sync\" or \"var\""),
                );
                continue;
            }

            if r.kind == "sync" {
                check_sync_resource(r, &path_prefix, diags);
            } else {
                check_var_resource(r, &path_prefix, diags);
            }
        }
    }
}

fn check_sync_resource(r: &Resource, path: &str, diags: &mut Vec<Diagnostic>) {
    let valid_types = ["Mutex", "RwLock", "Condvar", "Semaphore", "Channel"];
    if !valid_types.contains(&r.res_type.as_str()) {
        diags.push(
            Diagnostic::error(
                "E008",
                format!(
                    "sync resource '{}' has invalid type '{}'; expected one of: {}",
                    r.name,
                    r.res_type,
                    valid_types.join(", ")
                ),
            )
            .with_path(format!("{path}.type"))
            .with_fix("use a valid sync type: Mutex, RwLock, Condvar, Semaphore, or Channel"),
        );
    }

    // E009: mode is required and must be "Sync" or "Async"
    match &r.mode {
        None => {
            diags.push(
                Diagnostic::error(
                    "E001",
                    format!("sync resource '{}' is missing 'mode' field", r.name),
                )
                .with_path(path.to_string())
                .with_fix("add \"mode\": \"Sync\" or \"mode\": \"Async\""),
            );
        }
        Some(m) if m != "Sync" && m != "Async" => {
            diags.push(
                Diagnostic::error(
                    "E009",
                    format!("resource '{}' has invalid mode '{m}'", r.name),
                )
                .with_path(format!("{path}.mode"))
                .with_fix("mode must be \"Sync\" or \"Async\""),
            );
        }
        _ => {}
    }

    // E001: Semaphore requires count
    if r.res_type == "Semaphore" && r.count.is_none() {
        diags.push(
            Diagnostic::error(
                "E001",
                format!("Semaphore resource '{}' is missing 'count' field", r.name),
            )
            .with_path(path.to_string())
            .with_fix("add \"count\": <initial_permits>"),
        );
    }

    // E001: Channel requires base (payload type) and capacity (message store)
    if r.res_type == "Channel" && r.base.is_none() {
        diags.push(
            Diagnostic::error(
                "E001",
                format!("Channel resource '{}' is missing 'base' field", r.name),
            )
            .with_path(path.to_string())
            .with_fix("add \"base\": \"<type>\" to specify the channel payload type"),
        );
    }
    if r.res_type == "Channel" {
        match r.capacity {
            None => {
                diags.push(
                    Diagnostic::error(
                        "E001",
                        format!("Channel resource '{}' is missing 'capacity' field", r.name),
                    )
                    .with_path(path.to_string())
                    .with_fix(
                        "add \"capacity\": <n> (n ≥ 1 bounded buffer, 0 rendezvous); this is \
                         the in-flight message store",
                    ),
                );
            }
            Some(c) if c < 0 => {
                diags.push(
                    Diagnostic::error(
                        "E001",
                        format!("Channel resource '{}' has negative capacity {c}", r.name),
                    )
                    .with_path(format!("{path}.capacity"))
                    .with_fix("use capacity ≥ 0 (0 = rendezvous, n ≥ 1 = n payload slots)"),
                );
            }
            _ => {}
        }
    }
}

fn check_var_resource(r: &Resource, path: &str, diags: &mut Vec<Diagnostic>) {
    let valid_types = ["Var", "Atomic"];
    if !valid_types.contains(&r.res_type.as_str()) {
        diags.push(
            Diagnostic::error(
                "E008",
                format!(
                    "var resource '{}' has invalid type '{}'; expected Var or Atomic",
                    r.name, r.res_type
                ),
            )
            .with_path(format!("{path}.type"))
            .with_fix("use \"Var\" or \"Atomic\""),
        );
    }

    // E001: base is required
    if r.base.is_none() {
        diags.push(
            Diagnostic::error(
                "E001",
                format!("var resource '{}' is missing 'base' field", r.name),
            )
            .with_path(path.to_string())
            .with_fix("add \"base\": \"<type>\""),
        );
    }

    // E001: init is required
    if r.init.is_none() {
        diags.push(
            Diagnostic::error(
                "E001",
                format!("var resource '{}' is missing 'init' field", r.name),
            )
            .with_path(path.to_string())
            .with_fix("add \"init\": <initial_value>"),
        );
    }

    // E208: init value type matches base
    if let (Some(base), Some(init)) = (&r.base, &r.init) {
        check_init_type_match(&r.name, base, init, path, diags);
    }
}

fn check_init_type_match(
    name: &str,
    base: &BaseType,
    init: &serde_json::Value,
    path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let mismatch = match base {
        BaseType::Primitive(p) => match p.as_str() {
            "Bool" => !init.is_boolean(),
            "Int" => !init.is_i64(),
            "Float" => !init.is_f64() && !init.is_i64(),
            "String" => !init.is_string(),
            _ => false,
        },
        BaseType::Complex(ComplexBaseType::Enum(variants)) => {
            if let Some(s) = init.as_str() {
                !variants.contains(&s.to_string())
            } else {
                true
            }
        }
        BaseType::Complex(ComplexBaseType::Struct(_)) => !init.is_object(),
        BaseType::Complex(ComplexBaseType::Array(_)) => !init.is_array(),
        BaseType::Complex(ComplexBaseType::BoundedInt { lo, hi }) => match init.as_i64() {
            Some(v) => v < *lo || v > *hi,
            None => true,
        },
    };

    if mismatch {
        diags.push(
            Diagnostic::error(
                "E208",
                format!(
                    "resource '{}' init value does not match base type {base}",
                    name
                ),
            )
            .with_path(format!("{path}.init"))
            .with_fix("change init value to match the declared base type"),
        );
    }
}

fn check_functions(program: &Program, diags: &mut Vec<Diagnostic>) {
    for (mi, m) in program.modules.iter().enumerate() {
        for (fi, f) in m.functions.iter().enumerate() {
            let fn_path = Program::fn_path(mi, fi);

            // E010: function kind must be normal/async/closure
            if !["normal", "async", "closure"].contains(&f.kind.as_str()) {
                diags.push(
                    Diagnostic::error(
                        "E010",
                        format!("function '{}' has invalid kind '{}'", f.name, f.kind),
                    )
                    .with_path(format!("{fn_path}.kind"))
                    .with_fix("kind must be \"normal\", \"async\", or \"closure\""),
                );
            }

            // E005: sid format
            for (si, stmt) in f.body.iter().enumerate() {
                if !is_valid_sid(&stmt.sid) {
                    diags.push(
                        Diagnostic::error(
                            "E005",
                            format!("invalid sid format '{}' in function '{}'", stmt.sid, f.name),
                        )
                        .with_path(format!("{fn_path}.body[{si}].sid"))
                        .with_fix("sid must be \"s\" followed by a number, e.g. \"s1\", \"s10\""),
                    );
                }
            }
        }
    }
}

fn is_valid_sid(sid: &str) -> bool {
    sid.starts_with('s') && sid.len() > 1 && sid[1..].chars().all(|c| c.is_ascii_digit())
}
