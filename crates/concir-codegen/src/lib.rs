mod emit_domain;
mod emit_module;
mod emit_stmt;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use concir_ir::ast::{BaseType, ComplexBaseType};
use concir_ir::expr::{BinOp, CmpOp, Lit};
use concir_sem::eval::LExpr;
use concir_sem::ids::{ModuleId, ResourceId, SlotRef};
use concir_sem::program::{ResKind, SemOp, SemProgram};
use concir_sem::value::Value;


const TRACE_RUNTIME: &str = include_str!("../templates/cir_trace.rs");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodegenResource {
    pub fqn: String,
    pub field: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hole {
    pub id: String,
    #[serde(default)]
    pub module: String,
    pub function: String,
    pub sid: String,
    pub expected: String,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub line: usize,
    #[serde(default)]
    pub reads: Vec<String>,
    #[serde(default)]
    pub writes: Vec<String>,
    #[serde(default)]
    pub req: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeHole {
    pub id: String,
    pub type_name: String,
    pub module: String,
    pub file: String,
    pub line: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub req: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagRecord {
    pub sid: String,
    pub function: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodegenMap {
    pub program: String,
    pub sids: BTreeMap<String, SidLocation>,
    pub holes: Vec<Hole>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub type_holes: Vec<TypeHole>,
    pub tags: Vec<TagRecord>,
    #[serde(default)]
    pub resources: Vec<CodegenResource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidLocation {
    pub file: String,
    pub line: usize,
}

pub struct Generated {
    pub cargo_toml: String,
    pub main_rs: String,
    pub trace_rs: String,
    pub domain_rs: Option<String>,
    pub module_files: BTreeMap<String, String>,
    pub map: CodegenMap,
}


pub(crate) struct Gen<'a> {
    pub(crate) program: &'a SemProgram,
    pub(crate) lines: Vec<String>,
    pub(crate) current_file: String,
    pub(crate) sids: BTreeMap<String, SidLocation>,
    pub(crate) holes: Vec<Hole>,
    pub(crate) type_holes: Vec<TypeHole>,
    pub(crate) tags: Vec<TagRecord>,
    pub(crate) hole_seq: usize,
    /// `(module, typedef_name)` -> Rust identifier in `crate::domain`.
    pub(crate) named_types: BTreeMap<(ModuleId, String), String>,
    /// Structural key of anonymous `Struct` -> Rust identifier in `crate::domain`.
    pub(crate) anon_structs: BTreeMap<String, (String, BTreeMap<String, BaseType>)>,
    pub(crate) domain_lines: Vec<String>,
}


pub(crate) fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

pub(crate) fn rust_ty(bt: &BaseType) -> Result<String, String> {
    match bt {
        BaseType::Primitive(p) => match p.as_str() {
            "Bool" => Ok("bool".into()),
            "Int" => Ok("i64".into()),
            "Float" => Ok("f64".into()),
            "String" => Ok("String".into()),
            other => Err(format!("unsupported primitive type '{other}'")),
        },
        BaseType::Complex(c) => match c {
            ComplexBaseType::BoundedInt { .. } => Ok("i64".into()),
            ComplexBaseType::Enum(_) => Ok("String".into()),
            ComplexBaseType::Array(def) => {
                let elem = rust_ty(&def.elem)?;
                Ok(format!("[{elem}; {}]", def.len.max(0)))
            }
            ComplexBaseType::Struct(_) | ComplexBaseType::Hole { .. } => {
                Err("composite/hole type requires Gen context".into())
            }
        },
    }
}

pub(crate) fn default_expr(ty: &str) -> String {
    match ty {
        "bool" => "false".into(),
        "i64" => "0".into(),
        "f64" => "0.0".into(),
        "String" => "String::new()".into(),
        "()" => "()".into(),
        _ if ty.starts_with('[') && ty.ends_with(']') => {
            if let Some((elem_ty, _len)) = ty[1..ty.len() - 1].rsplit_once(';') {
                format!("std::array::from_fn(|_| {})", default_expr(elem_ty.trim()))
            } else {
                "Default::default()".into()
            }
        }
        _ => "Default::default()".into(),
    }
}

pub(crate) fn has_large_array(bt: &BaseType) -> bool {
    match bt {
        BaseType::Complex(ComplexBaseType::Array(def)) => def.len > 32 || has_large_array(&def.elem),
        BaseType::Complex(ComplexBaseType::Struct(fields)) => fields.values().any(has_large_array),
        _ => false,
    }
}

pub(crate) fn struct_key(fields: &BTreeMap<String, BaseType>) -> String {
    serde_json::to_string(fields).unwrap_or_default()
}

pub(crate) fn render_lit(l: &Lit) -> Result<String, String> {
    Ok(match l {
        Lit::Bool(b) => b.to_string(),
        Lit::Int(i) => i.to_string(),
        Lit::Float(f) => format!("{f:?}f64"),
        Lit::String(s) => format!("{s:?}.to_string()"),
        Lit::Enum(s) => format!("{s:?}.to_string()"),
    })
}


/// Ops whose trace event is emitted when the statement is *reached* (the model
/// step may block without advancing); every other observable op reports after
/// its completing step.
pub fn event_at_attempt(op: &SemOp) -> bool {
    matches!(
        op,
        SemOp::MutexUnlock { .. }
            | SemOp::RwLockUnlock { .. }
            | SemOp::CondvarNotify { .. }
            | SemOp::CondvarNotifyAll { .. }
            | SemOp::SemaphoreRelease { .. }
            | SemOp::ChannelSend { .. }
            | SemOp::ChannelRecv { .. }
            | SemOp::Spawn { .. }
            | SemOp::Scope { .. }
            | SemOp::Join { .. }
    )
}

pub fn is_observable(op: &SemOp) -> bool {
    matches!(
        op,
        SemOp::MutexLock { .. }
            | SemOp::MutexUnlock { .. }
            | SemOp::RwLockRead { .. }
            | SemOp::RwLockWrite { .. }
            | SemOp::RwLockUnlock { .. }
            | SemOp::CondvarWait { .. }
            | SemOp::CondvarNotify { .. }
            | SemOp::CondvarNotifyAll { .. }
            | SemOp::SemaphoreAcquire { .. }
            | SemOp::SemaphoreRelease { .. }
            | SemOp::ChannelSend { .. }
            | SemOp::ChannelRecv { .. }
            | SemOp::Spawn { .. }
            | SemOp::Scope { .. }
            | SemOp::Join { .. }
    )
}

/// Child thread tags for a spawn/scope statement. `conform` mirrors this.
pub fn child_tags(f: &concir_sem::program::SemFunction, sid: &str) -> Vec<String> {
    let idx = match f.stmt_index(sid) {
        Some(i) => i,
        None => return Vec::new(),
    };
    match &f.body.get(idx).map(|s| &s.op) {
        Some(SemOp::Scope { funcs }) => (0..funcs.len())
            .map(|i| format!("t{}_{}", sid, i + 1))
            .collect(),
        Some(SemOp::Spawn { .. }) => vec![format!("t{sid}")],
        _ => Vec::new(),
    }
}

pub(crate) fn ty_contains_struct(bt: &BaseType) -> bool {
    match bt {
        BaseType::Complex(ComplexBaseType::Struct(_)) => true,
        BaseType::Complex(ComplexBaseType::Array(def)) => ty_contains_struct(&def.elem),
        _ => false,
    }
}

pub(crate) fn program_needs_domain(program: &SemProgram) -> bool {
    if !program.types().is_empty() {
        return true;
    }
    for r in program.resources() {
        if r.ty.as_ref().is_some_and(ty_contains_struct) {
            return true;
        }
    }
    for f in program.functions() {
        if f.slots.iter().any(|s| ty_contains_struct(&s.ty)) {
            return true;
        }
    }
    false
}

pub fn generate(program: &SemProgram) -> Result<Generated, String> {
    let mut g = Gen::new(program);
    let needs_domain = program_needs_domain(program);
    let multi_file = program.module_names().len() > 1;
    let entry_module = program.entry_module();

    // Header + Shared struct.
    g.emit("// Generated by `concir-backend codegen`. Do not edit the skeleton.");
    g.emit("mod cir_trace;");
    if needs_domain {
        g.emit("pub(crate) mod domain;");
    }
    g.emit("use std::sync::{Arc, Condvar, Mutex};");
    g.emit("");
    g.emit("#[allow(dead_code)]");
    g.emit("pub(crate) mod __res {");
    g.emit("    use std::sync::{Condvar, Mutex, RwLock};");
    for r in program.resources() {
        let f = g.field(r.id);
        match r.kind {
            ResKind::Mutex => {
                g.emit(format!(
                    "    #[allow(non_upper_case_globals)] pub(crate) static {f}: Mutex<()> = Mutex::new(());"
                ));
            }
            ResKind::RwLock => {
                g.emit(format!(
                    "    #[allow(non_upper_case_globals)] pub(crate) static {f}: RwLock<()> = RwLock::new(());"
                ));
            }
            ResKind::Condvar => {
                g.emit(format!(
                    "    #[allow(non_upper_case_globals)] pub(crate) static {f}: Condvar = Condvar::new();"
                ));
            }
            _ => {}
        }
    }
    g.emit("}");
    g.emit("");
    g.emit("#[allow(dead_code)]");
    g.emit("pub(crate) struct Shared {");
    let mut codegen_resources = Vec::new();
    for r in program.resources() {
        let f = g.field(r.id);
        let kind_str = match r.kind {
            ResKind::Mutex => "Mutex",
            ResKind::Condvar => "Condvar",
            ResKind::Semaphore => "Semaphore",
            ResKind::Channel => "Channel",
            ResKind::RwLock => "RwLock",
            ResKind::Var => "Var",
            ResKind::Atomic => "Atomic",
        };
        codegen_resources.push(CodegenResource {
            fqn: g.resource_fqn(r.id),
            field: f.clone(),
            kind: kind_str.to_string(),
        });
        let ty = match r.kind {
            ResKind::Mutex => {
                let payload = g.resource_payload_ty(r.id)?;
                format!("Arc<Mutex<{payload}>>")
            }
            ResKind::RwLock => {
                let payload = g.resource_payload_ty(r.id)?;
                format!("Arc<std::sync::RwLock<{payload}>>")
            }
            ResKind::Condvar => "Arc<Condvar>".to_string(),
            ResKind::Semaphore => "Arc<cir_trace::Semaphore>".to_string(),
            ResKind::Channel => {
                let bt = r.ty.as_ref().ok_or("channel resource without a base type")?;
                let rt = g.resolve_rust_ty(r.module, r.raw_ty.as_ref(), bt, false)?;
                format!("Arc<cir_trace::Channel<{rt}>>")
            }
            ResKind::Var => {
                let bt = r.ty.as_ref().ok_or("value resource without a type")?;
                let rt = g.resolve_rust_ty(r.module, r.raw_ty.as_ref(), bt, false)?;
                format!("Arc<crate::cir_trace::SyncVar<{rt}>>")
            }
            ResKind::Atomic => {
                let bt = r.ty.as_ref().ok_or("atomic resource without a type")?;
                let rt = g.resolve_rust_ty(r.module, r.raw_ty.as_ref(), bt, false)?;
                match rt.as_str() {
                    "i64" => "Arc<std::sync::atomic::AtomicI64>".to_string(),
                    "bool" => "Arc<std::sync::atomic::AtomicBool>".to_string(),
                    _ => format!("Arc<Mutex<{rt}>>"),
                }
            }
        };
        g.emit(format!("    pub(crate) {f}: {ty},"));
    }
    g.emit("}");
    g.emit("");

    // Function bodies, one `mod` per CIR module.
    let mut module_order: Vec<ModuleId> = Vec::new();
    for f in program.functions() {
        if !module_order.contains(&f.module) {
            module_order.push(f.module);
        }
    }
    let mut module_files: BTreeMap<String, String> = BTreeMap::new();
    for m in module_order {
        let mod_name = program.module_name(m).to_string();
        let s_mod = sanitize(&mod_name);
        if multi_file && m != entry_module {
            // Emit non-entry module into `src/<sanitized_mod>.rs`.
            let rel_file = format!("src/{s_mod}.rs");
            g.emit(format!("#[path = \"{s_mod}.rs\"]"));
            g.emit(format!("pub(crate) mod {s_mod};"));
            g.emit("");

            let saved_lines = std::mem::take(&mut g.lines);
            let saved_file = std::mem::replace(&mut g.current_file, rel_file.clone());
            g.emit(format!(
                "// Generated module `{mod_name}` (`{rel_file}`). Do not edit the skeleton."
            ));
            g.emit("");
            for func in program.functions().iter().filter(|f| f.module == m) {
                if func.is_nobody {
                    let name = format!("cf_{}", sanitize(&func.name));
                    let placeholder = g.hole(&mod_name, &func.name, "", "()");
                    g.emit(format!(
                        "pub(crate) fn {name}(_shared: std::sync::Arc<crate::Shared>, _tag: &str) {{ let _: () = {placeholder}; }}"
                    ));
                    g.emit("");
                    continue;
                }
                emit_module::generate_function(&mut g, func)?;
            }
            let mod_src = g.lines.join("\n") + "\n";
            module_files.insert(rel_file, mod_src);
            g.lines = saved_lines;
            g.current_file = saved_file;
        } else {
            g.emit(format!("pub(crate) mod {s_mod} {{"));
            for func in program.functions().iter().filter(|f| f.module == m) {
                if func.is_nobody {
                    let name = format!("cf_{}", sanitize(&func.name));
                    let placeholder = g.hole(&mod_name, &func.name, "", "()");
                    g.emit(format!(
                        "    pub(crate) fn {name}(_shared: std::sync::Arc<crate::Shared>, _tag: &str) {{ let _: () = {placeholder}; }}"
                    ));
                    g.emit("");
                    continue;
                }
                emit_module::generate_function(&mut g, func)?;
            }
            g.emit("}");
            g.emit("");
        }
    }

    // Rust main.
    let entry = program.function(program.entry());
    let entry_name = format!(
        "crate::{}::cf_{}",
        sanitize(program.module_name(entry.module)),
        sanitize(&entry.name)
    );
    g.emit("fn main() {");
    g.emit("    let shared = Arc::new(Shared {");
    for r in program.resources() {
        let f = g.field(r.id);
        let init = match r.kind {
            ResKind::Mutex => {
                if let Some(bt) = &r.ty {
                    let ty = g.resolve_rust_ty(r.module, r.raw_ty.as_ref(), bt, false)?;
                    let v = default_expr(&ty);
                    format!("Arc::new(Mutex::new({v}))")
                } else {
                    "Arc::new(Mutex::new(()))".to_string()
                }
            }
            ResKind::RwLock => {
                if let Some(bt) = &r.ty {
                    let ty = g.resolve_rust_ty(r.module, r.raw_ty.as_ref(), bt, false)?;
                    let v = default_expr(&ty);
                    format!("Arc::new(std::sync::RwLock::new({v}))")
                } else {
                    "Arc::new(std::sync::RwLock::new(()))".to_string()
                }
            }
            ResKind::Condvar => "Arc::new(Condvar::new())".to_string(),
            ResKind::Semaphore => format!("cir_trace::Semaphore::new({})", r.permits),
            ResKind::Channel => {
                let bt = r.ty.as_ref().ok_or("channel resource without a base type")?;
                let rt = g.resolve_rust_ty(r.module, r.raw_ty.as_ref(), bt, false)?;
                format!("cir_trace::Channel::<{rt}>::new({})", r.capacity)
            }
            ResKind::Var => {
                let bt = r.ty.as_ref().ok_or("value resource without a type")?;
                let ty = g.resolve_rust_ty(r.module, r.raw_ty.as_ref(), bt, false)?;
                let v = match &r.init {
                    Some(v) => g.render_value_typed(v, r.module, r.raw_ty.as_ref(), bt)?,
                    None => default_expr(&ty),
                };
                format!("Arc::new(cir_trace::SyncVar::new({v}))")
            }
            ResKind::Atomic => {
                let bt = r.ty.as_ref().ok_or("atomic resource without a type")?;
                let ty = g.resolve_rust_ty(r.module, r.raw_ty.as_ref(), bt, false)?;
                let v = match &r.init {
                    Some(v) => g.render_value_typed(v, r.module, r.raw_ty.as_ref(), bt)?,
                    None => default_expr(&ty),
                };
                match ty.as_str() {
                    "i64" => format!("Arc::new(std::sync::atomic::AtomicI64::new({v}))"),
                    "bool" => format!("Arc::new(std::sync::atomic::AtomicBool::new({v}))"),
                    _ => format!("Arc::new(Mutex::new({v}))"),
                }
            }
        };
        g.emit(format!("        {f}: {init},"));
    }
    g.emit("    });");
    g.emit(format!("    {entry_name}(shared, \"t0\");"));
    g.emit("    cir_trace::finish();");
    g.emit("}");

    let domain_rs = g.build_domain_rs()?;

    let map = CodegenMap {
        program: program_name(program),
        sids: g.sids.clone(),
        holes: g.holes.clone(),
        type_holes: g.type_holes.clone(),
        tags: g.tags.clone(),
        resources: codegen_resources,
    };
    Ok(Generated {
        cargo_toml: cargo_toml(),
        main_rs: g.lines.join("\n") + "\n",
        trace_rs: TRACE_RUNTIME.to_string(),
        domain_rs,
        module_files,
        map,
    })
}

fn program_name(_program: &SemProgram) -> String {
    "generated".to_string()
}

fn cargo_toml() -> String {
    "[package]\nname = \"cir_generated\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\ndefault = [\"cir_trace\"]\ncir_trace = []\n\n[dependencies]\n".to_string()
}


pub fn write_project(out_dir: &std::path::Path, generated: &Generated) -> std::io::Result<()> {
    std::fs::create_dir_all(out_dir.join("src"))?;
    std::fs::write(out_dir.join("Cargo.toml"), &generated.cargo_toml)?;
    std::fs::write(out_dir.join("src/main.rs"), &generated.main_rs)?;
    std::fs::write(out_dir.join("src/cir_trace.rs"), &generated.trace_rs)?;
    if let Some(domain_rs) = &generated.domain_rs {
        std::fs::write(out_dir.join("src/domain.rs"), domain_rs)?;
    }
    for (rel_path, content) in &generated.module_files {
        let target = out_dir.join(rel_path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, content)?;
    }
    let map = serde_json::to_string_pretty(&generated.map).unwrap_or_default();
    std::fs::write(out_dir.join("codegen.json"), map + "\n")?;
    Ok(())
}

