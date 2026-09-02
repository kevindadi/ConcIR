//! Module-level named types.
//!
//! A [`BaseType::Primitive`] that is not `Bool` / `Int` / `Float` / `String`
//! is a type name: same-module short name, or an FQN listed in
//! `requires.types`.

use std::collections::{HashMap, HashSet};

use crate::ast::{BaseType, ComplexBaseType, Program, TypeDef};
use crate::diagnostic::Diagnostic;
use crate::fqn;

pub fn is_builtin(name: &str) -> bool {
    matches!(name, "Bool" | "Int" | "Float" | "String")
}

/// Resolved named types, keyed by FQN (`module::Type`).
#[derive(Debug, Clone, Default)]
pub struct TypeEnv {
    resolved: HashMap<String, BaseType>,
}

impl TypeEnv {
    pub fn from_program(program: &Program) -> Self {
        let mut defs: HashMap<String, (String, &TypeDef)> = HashMap::new();
        for m in &program.modules {
            for t in &m.types {
                defs.insert(fqn::fqn(&m.name, &t.name), (m.name.clone(), t));
            }
        }
        let mut resolved = HashMap::new();
        for (key, (owner, def)) in &defs {
            let mut stack = Vec::new();
            if let Ok(ty) = resolve_def(&defs, owner, &def.ty, &mut stack) {
                resolved.insert(key.clone(), ty);
            }
        }
        TypeEnv { resolved }
    }

    pub fn resolve(&self, from_module: &str, ty: &BaseType) -> Option<BaseType> {
        resolve_with_map(&self.resolved, from_module, ty)
    }

    pub fn resolve_name(&self, from_module: &str, name: &str) -> Option<BaseType> {
        if is_builtin(name) {
            return Some(BaseType::Primitive(name.to_string()));
        }
        let key = if fqn::is_fqn(name) {
            name.to_string()
        } else {
            fqn::fqn(from_module, name)
        };
        self.resolved.get(&key).cloned()
    }
}

fn resolve_with_map(
    resolved: &HashMap<String, BaseType>,
    from_module: &str,
    ty: &BaseType,
) -> Option<BaseType> {
    match ty {
        BaseType::Primitive(p) if is_builtin(p) => Some(ty.clone()),
        BaseType::Primitive(p) => {
            let key = if fqn::is_fqn(p) {
                p.clone()
            } else {
                fqn::fqn(from_module, p)
            };
            resolved.get(&key).cloned()
        }
        BaseType::Complex(ComplexBaseType::Struct(fields)) => {
            let mut out = std::collections::BTreeMap::new();
            for (k, v) in fields {
                out.insert(k.clone(), resolve_with_map(resolved, from_module, v)?);
            }
            Some(BaseType::Complex(ComplexBaseType::Struct(out)))
        }
        BaseType::Complex(ComplexBaseType::Array(def)) => {
            let elem = resolve_with_map(resolved, from_module, &def.elem)?;
            Some(BaseType::Complex(ComplexBaseType::Array(Box::new(
                crate::ast::ArrayDef { elem, len: def.len },
            ))))
        }
        other => Some(other.clone()),
    }
}

fn resolve_def(
    defs: &HashMap<String, (String, &TypeDef)>,
    owner: &str,
    ty: &BaseType,
    stack: &mut Vec<String>,
) -> Result<BaseType, ()> {
    match ty {
        BaseType::Primitive(p) if is_builtin(p) => Ok(ty.clone()),
        BaseType::Primitive(p) => {
            let key = if fqn::is_fqn(p) {
                p.clone()
            } else {
                fqn::fqn(owner, p)
            };
            if stack.contains(&key) {
                return Err(());
            }
            let Some((next_owner, def)) = defs.get(&key) else {
                return Err(());
            };
            stack.push(key);
            let out = resolve_def(defs, next_owner, &def.ty, stack)?;
            stack.pop();
            Ok(out)
        }
        BaseType::Complex(ComplexBaseType::Struct(fields)) => {
            let mut out = std::collections::BTreeMap::new();
            for (k, v) in fields {
                out.insert(k.clone(), resolve_def(defs, owner, v, stack)?);
            }
            Ok(BaseType::Complex(ComplexBaseType::Struct(out)))
        }
        BaseType::Complex(ComplexBaseType::Array(def)) => {
            let elem = resolve_def(defs, owner, &def.elem, stack)?;
            Ok(BaseType::Complex(ComplexBaseType::Array(Box::new(
                crate::ast::ArrayDef { elem, len: def.len },
            ))))
        }
        other => Ok(other.clone()),
    }
}

/// E110–E113: named-type declarations and uses.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    check_declarations(program, diags);
    check_uses(program, diags);
}

fn check_declarations(program: &Program, diags: &mut Vec<Diagnostic>) {
    for (mi, m) in program.modules.iter().enumerate() {
        let mut seen = HashSet::new();
        for (i, t) in m.types.iter().enumerate() {
            let path = format!("modules[{mi}].types[{i}]");
            if is_builtin(&t.name) {
                diags.push(
                    Diagnostic::error(
                        "E112",
                        format!(
                            "type name '{}' in module '{}' is a builtin primitive",
                            t.name, m.name
                        ),
                    )
                    .with_path(format!("{path}.name"))
                    .with_fix("choose a name other than Bool, Int, Float, or String"),
                );
            }
            if !fqn::is_ident(&t.name) {
                diags.push(
                    Diagnostic::error(
                        "E110",
                        format!("type name '{}' is not an identifier", t.name),
                    )
                    .with_path(format!("{path}.name"))
                    .with_fix("use [A-Za-z_][A-Za-z0-9_]*"),
                );
            }
            if !seen.insert(t.name.clone()) {
                diags.push(
                    Diagnostic::error(
                        "E110",
                        format!("duplicate type '{}' in module '{}'", t.name, m.name),
                    )
                    .with_path(path.clone())
                    .with_fix("rename one of the types"),
                );
            }
        }
    }

    let defs: HashMap<String, (String, &TypeDef)> = program
        .modules
        .iter()
        .flat_map(|m| {
            m.types
                .iter()
                .map(move |t| (fqn::fqn(&m.name, &t.name), (m.name.clone(), t)))
        })
        .collect();

    for (mi, m) in program.modules.iter().enumerate() {
        for (i, t) in m.types.iter().enumerate() {
            let path = format!("modules[{mi}].types[{i}].type");
            let mut stack = vec![fqn::fqn(&m.name, &t.name)];
            match resolve_def(&defs, &m.name, &t.ty, &mut stack) {
                Err(())
                    if has_cycle(&defs, &m.name, &t.ty, &mut vec![fqn::fqn(&m.name, &t.name)]) =>
                {
                    diags.push(
                        Diagnostic::error(
                            "E113",
                            format!("type '{}' in module '{}' is a cyclic alias", t.name, m.name),
                        )
                        .with_path(path)
                        .with_fix("break the cycle; aliases must bottom out in a concrete type"),
                    );
                }
                Err(()) => {
                    if let Some(name) = first_undefined(&t.ty) {
                        diags.push(
                            Diagnostic::error(
                                "E111",
                                format!("type '{}' refers to undefined type '{name}'", t.name),
                            )
                            .with_path(path)
                            .with_fix("declare the named type in this module or import it"),
                        );
                    }
                }
                Ok(_) => {}
            }
        }
    }
}

fn has_cycle(
    defs: &HashMap<String, (String, &TypeDef)>,
    owner: &str,
    ty: &BaseType,
    stack: &mut Vec<String>,
) -> bool {
    let BaseType::Primitive(p) = ty else {
        if let BaseType::Complex(ComplexBaseType::Struct(fields)) = ty {
            return fields.values().any(|v| has_cycle(defs, owner, v, stack));
        }
        if let BaseType::Complex(ComplexBaseType::Array(def)) = ty {
            return has_cycle(defs, owner, &def.elem, stack);
        }
        return false;
    };
    if is_builtin(p) {
        return false;
    }
    let key = if fqn::is_fqn(p) {
        p.clone()
    } else {
        fqn::fqn(owner, p)
    };
    if stack.contains(&key) {
        return true;
    }
    let Some((next_owner, def)) = defs.get(&key) else {
        return false;
    };
    stack.push(key);
    let cycled = has_cycle(defs, next_owner, &def.ty, stack);
    stack.pop();
    cycled
}

fn first_undefined(ty: &BaseType) -> Option<String> {
    match ty {
        BaseType::Primitive(p) if !is_builtin(p) => Some(p.clone()),
        BaseType::Complex(ComplexBaseType::Struct(fields)) => {
            fields.values().find_map(first_undefined)
        }
        BaseType::Complex(ComplexBaseType::Array(def)) => first_undefined(&def.elem),
        _ => None,
    }
}

fn check_uses(program: &Program, diags: &mut Vec<Diagnostic>) {
    let env = TypeEnv::from_program(program);
    for (mi, m) in program.modules.iter().enumerate() {
        for (i, r) in m.resources.iter().enumerate() {
            if let Some(base) = &r.base {
                check_ty_use(
                    &env,
                    &m.name,
                    base,
                    format!("modules[{mi}].resources[{i}].base"),
                    diags,
                );
            }
        }
        for (fi, f) in m.functions.iter().enumerate() {
            let fn_path = Program::fn_path(mi, fi);
            for (pi, p) in f.params.iter().enumerate() {
                check_ty_use(
                    &env,
                    &m.name,
                    &p.param_type,
                    format!("{fn_path}.params[{pi}].type"),
                    diags,
                );
            }
            if let Some(ret) = &f.returns {
                check_ty_use(
                    &env,
                    &m.name,
                    &ret.param_type,
                    format!("{fn_path}.returns.type"),
                    diags,
                );
            }
            for (li, local) in f.locals.iter().enumerate() {
                check_ty_use(
                    &env,
                    &m.name,
                    &local.local_type,
                    format!("{fn_path}.locals[{li}].type"),
                    diags,
                );
            }
        }
    }
}

fn check_ty_use(
    env: &TypeEnv,
    from_module: &str,
    ty: &BaseType,
    path: String,
    diags: &mut Vec<Diagnostic>,
) {
    match ty {
        BaseType::Primitive(p) if is_builtin(p) => {}
        BaseType::Primitive(p) => {
            if env.resolve_name(from_module, p).is_none() {
                diags.push(
                    Diagnostic::error("E111", format!("undefined type '{p}'"))
                        .with_path(path)
                        .with_fix(
                            "declare the type in this module, or import it as an FQN in \
                             requires.types",
                        ),
                );
            }
        }
        BaseType::Complex(ComplexBaseType::Struct(fields)) => {
            for (k, v) in fields {
                check_ty_use(env, from_module, v, format!("{path}.{k}"), diags);
            }
        }
        BaseType::Complex(ComplexBaseType::Array(def)) => {
            check_ty_use(env, from_module, &def.elem, format!("{path}.elem"), diags);
        }
        _ => {}
    }
}
