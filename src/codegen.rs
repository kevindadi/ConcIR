//! CIR -> Rust skeleton generator (`concir-backend codegen`).
//!
//! The generated program is a standard-library-only cargo project whose
//! concurrency operations are annotated with `cir_trace::ev(tag, sid)` and a
//! trailing `// @cir <sid>` comment. Everything the translation cannot express
//! becomes a `// HOLE(<id>)` with a compilable placeholder, so a downstream
//! filler can only write sequential local computation inside a hole.
//!
//! Scope of this first version: a single module, `Mutex`, `Condvar`,
//! `Semaphore`, `Var`/`Atomic` resources, and the control-flow / synchronization
//! statements listed in `doc/backend-usage.md`. Channels, cross-module programs,
//! `RwLock`, and composite values are reported as unsupported.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::ast::{BaseType, ComplexBaseType};
use crate::expr::{BinOp, CmpOp, Lit};
use crate::sem::eval::LExpr;
use crate::sem::ids::{ModuleId, ResourceId, SlotRef};
use crate::sem::program::{ResKind, SemOp, SemProgram};
use crate::sem::value::Value;

const TRACE_RUNTIME: &str = r#"// Generated cir_trace runtime v2 (std only, operation-bound events).
use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
#[cfg(feature = "cir_trace")]
use std::sync::OnceLock;

/// Lock-protected shared variable cell (zero synthetic Mutex in MIR).
#[allow(dead_code)]
pub struct SyncVar<T> {
    inner: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for SyncVar<T> {}
unsafe impl<T: Send> Send for SyncVar<T> {}

#[allow(dead_code)]
impl<T: Clone> SyncVar<T> {
    pub fn new(v: T) -> Self {
        SyncVar {
            inner: UnsafeCell::new(v),
        }
    }
    pub fn read(&self) -> T {
        unsafe { (*self.inner.get()).clone() }
    }
    pub fn write(&self, v: T) {
        unsafe {
            *self.inner.get() = v;
        }
    }
}

thread_local! {
    static TAG: std::cell::RefCell<String> = std::cell::RefCell::new("t0".to_string());
}

#[allow(dead_code)]
pub fn tag_str() -> String {
    TAG.with(|t| t.borrow().clone())
}

#[allow(dead_code)]
pub fn set_tag(tag: &str) {
    TAG.with(|t| *t.borrow_mut() = tag.to_string());
}

#[cfg(feature = "cir_trace")]
static EVENTS: OnceLock<Mutex<Vec<(String, String, String, String)>>> = OnceLock::new();

/// Record an operation-bound event: (tag, op, resource, sid).
#[allow(dead_code)]
pub fn record(tag: &str, op: &str, resource: &str, sid: &str) {
    #[cfg(feature = "cir_trace")]
    {
        let m = EVENTS.get_or_init(|| Mutex::new(Vec::new()));
        m.lock().unwrap().push((tag.to_string(), op.to_string(),
                                resource.to_string(), sid.to_string()));
    }
    #[cfg(not(feature = "cir_trace"))]
    {
        let _ = (tag, op, resource, sid);
    }
}

/// Legacy standalone annotation (disabled in codegen v2; kept for tooling).
#[allow(dead_code)]
pub fn ev(tag: &str, sid: &str) {
    record(tag, "ev", "", sid);
}

#[allow(dead_code)]
pub fn finish() {
    #[cfg(feature = "cir_trace")]
    {
        if let Ok(path) = std::env::var("CIR_TRACE_OUT") {
            let m = EVENTS.get_or_init(|| Mutex::new(Vec::new()));
            let guard = m.lock().unwrap();
            let mut out = String::new();
            for (t, op, r, s) in guard.iter() {
                out.push_str(&format!(
                    "{{\"t\":\"{}\",\"sid\":\"{}\",\"op\":\"{}\",\"r\":\"{}\"}}\n",
                    t, s, op, r));
            }
            let _ = std::fs::write(path, out);
        }
    }
}

// ---- operation-bound helpers -------------------------------------------

pub fn lock<'a, T>(m: &'a Mutex<T>, tag: &str, resource: &str, sid: &str)
    -> std::sync::MutexGuard<'a, T> {
    let g = m.lock().unwrap();
    record(tag, "mutex_lock", resource, sid);
    g
}

pub fn unlock<T>(g: Option<std::sync::MutexGuard<'_, T>>, tag: &str,
                 resource: &str, sid: &str) {
    record(tag, "mutex_unlock", resource, sid);
    drop(g);
}

#[allow(dead_code)]
pub fn rwlock_read_lock<'a, T>(l: &'a RwLock<T>, tag: &str, resource: &str, sid: &str)
    -> RwLockReadGuard<'a, T> {
    let g = l.read().unwrap();
    record(tag, "rwlock_read_lock", resource, sid);
    g
}

#[allow(dead_code)]
pub fn rwlock_read_unlock<T>(g: Option<RwLockReadGuard<'_, T>>, tag: &str,
                             resource: &str, sid: &str) {
    record(tag, "rwlock_read_unlock", resource, sid);
    drop(g);
}

#[allow(dead_code)]
pub fn rwlock_write_lock<'a, T>(l: &'a RwLock<T>, tag: &str, resource: &str, sid: &str)
    -> RwLockWriteGuard<'a, T> {
    let g = l.write().unwrap();
    record(tag, "rwlock_write_lock", resource, sid);
    g
}

#[allow(dead_code)]
pub fn rwlock_write_unlock<T>(g: Option<RwLockWriteGuard<'_, T>>, tag: &str,
                              resource: &str, sid: &str) {
    record(tag, "rwlock_write_unlock", resource, sid);
    drop(g);
}

pub fn condvar_wait<'a, T>(cv: &Condvar, g: std::sync::MutexGuard<'a, T>,
                           tag: &str, resource: &str, sid: &str)
    -> std::sync::MutexGuard<'a, T> {
    let g = cv.wait(g).unwrap();
    record(tag, "condvar_wait", resource, sid);
    g
}

pub fn notify_one(cv: &Condvar, tag: &str, resource: &str, sid: &str) {
    record(tag, "condvar_notify", resource, sid);
    cv.notify_one();
}

pub fn notify_all(cv: &Condvar, tag: &str, resource: &str, sid: &str) {
    record(tag, "condvar_notify_all", resource, sid);
    cv.notify_all();
}

pub fn scope(tag: &str, sid: &str) {
    record(tag, "scope", "", sid);
}

pub fn spawn<F>(tag: &str, child: &str, sid: &str, f: F) -> std::thread::JoinHandle<()>
where F: FnOnce() + Send + 'static {
    record(tag, "spawn", child, sid);
    std::thread::spawn(f)
}

pub fn join(h: std::thread::JoinHandle<()>, tag: &str, child: &str, sid: &str) {
    record(tag, "join", child, sid);
    h.join().unwrap();
}

pub struct Semaphore {
    count: Mutex<i64>,
    cv: Condvar,
}

impl Semaphore {
    pub fn new(n: i64) -> Arc<Self> {
        Arc::new(Semaphore { count: Mutex::new(n), cv: Condvar::new() })
    }
    pub fn acquire(&self, n: i64) {
        let mut c = self.count.lock().unwrap();
        while *c < n {
            c = self.cv.wait(c).unwrap();
        }
        *c -= n;
    }
    pub fn release(&self, n: i64) {
        let mut c = self.count.lock().unwrap();
        *c += n;
        self.cv.notify_all();
    }
    pub fn acquire_sid(&self, n: i64, tag: &str, resource: &str, sid: &str) {
        self.acquire(n);
        record(tag, "sem_acquire", resource, sid);
    }
    pub fn release_sid(&self, n: i64, tag: &str, resource: &str, sid: &str) {
        record(tag, "sem_release", resource, sid);
        self.release(n);
    }
}

#[allow(dead_code)]
pub struct Channel<T> {
    buffer: Mutex<VecDeque<T>>,
    cap: usize,
    send_cv: Condvar,
    recv_cv: Condvar,
}

impl<T: Send> Channel<T> {
    pub fn new(cap: usize) -> Arc<Self> {
        Arc::new(Channel {
            buffer: Mutex::new(VecDeque::new()),
            cap,
            send_cv: Condvar::new(),
            recv_cv: Condvar::new(),
        })
    }
    pub fn send(&self, v: T) {
        let mut b = self.buffer.lock().unwrap();
        while self.cap != 0 && b.len() >= self.cap {
            b = self.send_cv.wait(b).unwrap();
        }
        if self.cap == 0 {
            b.push_back(v);
            self.recv_cv.notify_one();
            while !b.is_empty() {
                b = self.send_cv.wait(b).unwrap();
            }
        } else {
            b.push_back(v);
            self.recv_cv.notify_one();
        }
    }
    pub fn recv(&self) -> T {
        let mut b = self.buffer.lock().unwrap();
        while b.is_empty() {
            b = self.recv_cv.wait(b).unwrap();
        }
        let v = b.pop_front().unwrap();
        self.send_cv.notify_one();
        v
    }
    pub fn send_sid(&self, v: T, tag: &str, resource: &str, sid: &str) {
        record(tag, "channel_send", resource, sid);
        self.send(v);
    }
    pub fn recv_sid(&self, tag: &str, resource: &str, sid: &str) -> T {
        record(tag, "channel_recv", resource, sid);
        self.recv()
    }
}
"#;

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

struct Gen<'a> {
    program: &'a SemProgram,
    lines: Vec<String>,
    current_file: String,
    sids: BTreeMap<String, SidLocation>,
    holes: Vec<Hole>,
    type_holes: Vec<TypeHole>,
    tags: Vec<TagRecord>,
    hole_seq: usize,
    /// `(module, typedef_name)` -> Rust identifier in `crate::domain`.
    named_types: BTreeMap<(ModuleId, String), String>,
    /// Structural key of anonymous `Struct` -> Rust identifier in `crate::domain`.
    anon_structs: BTreeMap<String, (String, BTreeMap<String, BaseType>)>,
    domain_lines: Vec<String>,
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn rust_ty(bt: &BaseType) -> Result<String, String> {
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

fn default_expr(ty: &str) -> String {
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

fn has_large_array(bt: &BaseType) -> bool {
    match bt {
        BaseType::Complex(ComplexBaseType::Array(def)) => def.len > 32 || has_large_array(&def.elem),
        BaseType::Complex(ComplexBaseType::Struct(fields)) => fields.values().any(has_large_array),
        _ => false,
    }
}

fn struct_key(fields: &BTreeMap<String, BaseType>) -> String {
    serde_json::to_string(fields).unwrap_or_default()
}

fn render_lit(l: &Lit) -> Result<String, String> {
    Ok(match l {
        Lit::Bool(b) => b.to_string(),
        Lit::Int(i) => i.to_string(),
        Lit::Float(f) => format!("{f:?}f64"),
        Lit::String(s) => format!("{s:?}.to_string()"),
        Lit::Enum(s) => format!("{s:?}.to_string()"),
    })
}

impl<'a> Gen<'a> {
    fn new(program: &'a SemProgram) -> Self {
        let mut name_counts: BTreeMap<String, usize> = BTreeMap::new();
        for t in program.types() {
            *name_counts.entry(sanitize(&t.name)).or_insert(0) += 1;
        }
        let mut named_types = BTreeMap::new();
        for t in program.types() {
            let short = sanitize(&t.name);
            let rust_name = if name_counts.get(&short).copied().unwrap_or(0) > 1 {
                format!("{}_{}", sanitize(program.module_name(t.module)), short)
            } else {
                short
            };
            named_types.insert((t.module, t.name.clone()), rust_name);
        }
        Gen {
            program,
            lines: Vec::new(),
            current_file: "src/main.rs".to_string(),
            sids: BTreeMap::new(),
            holes: Vec::new(),
            type_holes: Vec::new(),
            tags: Vec::new(),
            hole_seq: 0,
            named_types,
            anon_structs: BTreeMap::new(),
            domain_lines: Vec::new(),
        }
    }

    fn resolve_rust_ty(
        &mut self,
        from_module: ModuleId,
        raw_ty: Option<&BaseType>,
        resolved_ty: &BaseType,
        in_domain_mod: bool,
    ) -> Result<String, String> {
        let prefix = if in_domain_mod { "" } else { "crate::domain::" };
        if let Some(BaseType::Primitive(p)) = raw_ty {
            if !crate::typedef::is_builtin(p) {
                if let Some(td) = self.program.resolve_type_def(from_module, p) {
                    if let Some(rname) = self.named_types.get(&(td.module, td.name.clone())) {
                        return Ok(format!("{prefix}{rname}"));
                    }
                }
            }
        }
        match resolved_ty {
            BaseType::Primitive(p) => match p.as_str() {
                "Bool" => Ok("bool".into()),
                "Int" => Ok("i64".into()),
                "Float" => Ok("f64".into()),
                "String" => Ok("String".into()),
                other => {
                    if let Some(td) = self.program.resolve_type_def(from_module, other) {
                        if let Some(rname) = self.named_types.get(&(td.module, td.name.clone())) {
                            return Ok(format!("{prefix}{rname}"));
                        }
                    }
                    Err(format!("unsupported primitive type '{other}'"))
                }
            },
            BaseType::Complex(ComplexBaseType::BoundedInt { .. }) => Ok("i64".into()),
            BaseType::Complex(ComplexBaseType::Enum(_)) => Ok("String".into()),
            BaseType::Complex(ComplexBaseType::Array(def)) => {
                let raw_elem = match raw_ty {
                    Some(BaseType::Complex(ComplexBaseType::Array(rdef))) => Some(&rdef.elem),
                    _ => None,
                };
                let elem = self.resolve_rust_ty(from_module, raw_elem, &def.elem, in_domain_mod)?;
                Ok(format!("[{elem}; {}]", def.len.max(0)))
            }
            BaseType::Complex(ComplexBaseType::Struct(fields)) => {
                let key = struct_key(fields);
                let name = if let Some((existing, _)) = self.anon_structs.get(&key) {
                    existing.clone()
                } else {
                    let idx = self.anon_structs.len() + 1;
                    let nm = format!("AnonStruct{idx}");
                    self.anon_structs
                        .insert(key, (nm.clone(), fields.clone()));
                    nm
                };
                Ok(format!("{prefix}{name}"))
            }
            BaseType::Complex(ComplexBaseType::Hole { .. }) => Ok("()".into()),
        }
    }

    fn render_value_typed(
        &mut self,
        v: &Value,
        from_module: ModuleId,
        raw_ty: Option<&BaseType>,
        resolved_ty: &BaseType,
    ) -> Result<String, String> {
        if matches!(resolved_ty, BaseType::Complex(ComplexBaseType::Hole { .. })) {
            let ty = self.resolve_rust_ty(from_module, raw_ty, resolved_ty, false)?;
            return Ok(format!("{ty}::default()"));
        }
        Ok(match v {
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => format!("{f:?}f64"),
            Value::Str(s) => format!("{s:?}.to_string()"),
            Value::Enum(s) => format!("{s:?}.to_string()"),
            Value::Struct(fields) => {
                let struct_ty = self.resolve_rust_ty(from_module, raw_ty, resolved_ty, false)?;
                let field_tys = match resolved_ty {
                    BaseType::Complex(ComplexBaseType::Struct(ft)) => Some(ft),
                    _ => None,
                };
                let raw_field_tys = match raw_ty {
                    Some(BaseType::Complex(ComplexBaseType::Struct(ft))) => Some(ft),
                    _ => None,
                };
                let mut parts = Vec::new();
                for (k, fv) in fields {
                    let fk = sanitize(k);
                    let f_res_ty = field_tys
                        .and_then(|m| m.get(k))
                        .cloned()
                        .unwrap_or_else(|| BaseType::Primitive("Int".into()));
                    let f_raw_ty = raw_field_tys.and_then(|m| m.get(k));
                    let rendered =
                        self.render_value_typed(fv, from_module, f_raw_ty, &f_res_ty)?;
                    parts.push(format!("{fk}: {rendered}"));
                }
                format!("{struct_ty} {{ {} }}", parts.join(", "))
            }
            Value::Array(items) => {
                let elem_res_ty = match resolved_ty {
                    BaseType::Complex(ComplexBaseType::Array(def)) => def.elem.clone(),
                    _ => BaseType::Primitive("Int".into()),
                };
                let elem_raw_ty = match raw_ty {
                    Some(BaseType::Complex(ComplexBaseType::Array(def))) => Some(&def.elem),
                    _ => None,
                };
                let mut parts = Vec::new();
                for item in items {
                    parts.push(self.render_value_typed(
                        item,
                        from_module,
                        elem_raw_ty,
                        &elem_res_ty,
                    )?);
                }
                format!("[{}]", parts.join(", "))
            }
        })
    }

    fn build_domain_rs(&mut self) -> Result<Option<String>, String> {
        let has_named = !self.program.types().is_empty();
        let has_anon = !self.anon_structs.is_empty();
        if !has_named && !has_anon {
            return Ok(None);
        }
        self.domain_lines.clear();
        self.domain_lines
            .push("// Generated domain types (`src/domain.rs`).".to_string());
        self.domain_lines.push("#![allow(dead_code)]".to_string());
        self.domain_lines.push(String::new());

        let mut th_seq = 0usize;
        let types: Vec<_> = self.program.types().to_vec();
        for t in &types {
            let mod_name = self.program.module_name(t.module).to_string();
            let rust_name = self
                .named_types
                .get(&(t.module, t.name.clone()))
                .cloned()
                .unwrap_or_else(|| sanitize(&t.name));
            match &t.ty {
                BaseType::Complex(ComplexBaseType::Hole { description, req }) => {
                    th_seq += 1;
                    let id = format!("th{th_seq}");
                    let line = self.domain_lines.len() + 1;
                    self.type_holes.push(TypeHole {
                        id: id.clone(),
                        type_name: rust_name.clone(),
                        module: mod_name,
                        file: "src/domain.rs".to_string(),
                        line,
                        description: description.clone(),
                        req: req.clone(),
                    });
                    self.domain_lines
                        .push(format!("/* TYPE_HOLE({id}) type: {rust_name} */"));
                    self.domain_lines
                        .push("#[derive(Clone, Debug, Default, PartialEq)]".to_string());
                    self.domain_lines.push(format!("pub struct {rust_name} {{}}"));
                    self.domain_lines.push(format!(
                        "const _: fn() = || {{ fn assert_domain_bounds<T: Send + Sync + Clone + Default + 'static>() {{}} assert_domain_bounds::<{rust_name}>(); }};"
                    ));
                    self.domain_lines.push(String::new());
                }
                BaseType::Complex(ComplexBaseType::Struct(fields)) => {
                    let raw_fields = match &t.raw_ty {
                        BaseType::Complex(ComplexBaseType::Struct(rf)) => Some(rf),
                        _ => None,
                    };
                    let large_arr = fields.values().any(has_large_array);
                    if large_arr {
                        self.domain_lines
                            .push("#[derive(Clone, Debug, PartialEq)]".to_string());
                    } else {
                        self.domain_lines
                            .push("#[derive(Clone, Debug, Default, PartialEq)]".to_string());
                    }
                    self.domain_lines.push(format!("pub struct {rust_name} {{"));
                    let mut default_fields = Vec::new();
                    for (fname, fty) in fields {
                        let raw_fty = raw_fields.and_then(|rf| rf.get(fname));
                        let rt = self.resolve_rust_ty(t.module, raw_fty, fty, true)?;
                        let sname = sanitize(fname);
                        self.domain_lines.push(format!("    pub {sname}: {rt},"));
                        default_fields.push((sname, default_expr(&rt)));
                    }
                    self.domain_lines.push("}".to_string());
                    if large_arr {
                        self.domain_lines
                            .push(format!("impl Default for {rust_name} {{"));
                        self.domain_lines.push("    fn default() -> Self {".to_string());
                        self.domain_lines.push("        Self {".to_string());
                        for (sname, dexpr) in default_fields {
                            self.domain_lines.push(format!("            {sname}: {dexpr},"));
                        }
                        self.domain_lines.push("        }".to_string());
                        self.domain_lines.push("    }".to_string());
                        self.domain_lines.push("}".to_string());
                    }
                    self.domain_lines.push(format!(
                        "const _: fn() = || {{ fn assert_domain_bounds<T: Send + Sync + Clone + Default + 'static>() {{}} assert_domain_bounds::<{rust_name}>(); }};"
                    ));
                    self.domain_lines.push(String::new());
                }
                other => {
                    let rt = self.resolve_rust_ty(t.module, None, other, true)?;
                    self.domain_lines.push(format!("pub type {rust_name} = {rt};"));
                    self.domain_lines.push(String::new());
                }
            }
        }

        // Emit any anonymous structs collected while lowering fields/resources/slots.
        let mut emitted_anon: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        loop {
            let pending: Vec<(String, BTreeMap<String, BaseType>)> = self
                .anon_structs
                .values()
                .filter(|(nm, _)| !emitted_anon.contains(nm))
                .cloned()
                .collect();
            if pending.is_empty() {
                break;
            }
            for (nm, fields) in pending {
                emitted_anon.insert(nm.clone());
                let large_arr = fields.values().any(has_large_array);
                if large_arr {
                    self.domain_lines
                        .push("#[derive(Clone, Debug, PartialEq)]".to_string());
                } else {
                    self.domain_lines
                        .push("#[derive(Clone, Debug, Default, PartialEq)]".to_string());
                }
                self.domain_lines.push(format!("pub struct {nm} {{"));
                let mut default_fields = Vec::new();
                for (fname, fty) in &fields {
                    let rt = self.resolve_rust_ty(ModuleId(0), None, fty, true)?;
                    let sname = sanitize(fname);
                    self.domain_lines.push(format!("    pub {sname}: {rt},"));
                    default_fields.push((sname, default_expr(&rt)));
                }
                self.domain_lines.push("}".to_string());
                if large_arr {
                    self.domain_lines.push(format!("impl Default for {nm} {{"));
                    self.domain_lines.push("    fn default() -> Self {".to_string());
                    self.domain_lines.push("        Self {".to_string());
                    for (sname, dexpr) in default_fields {
                        self.domain_lines.push(format!("            {sname}: {dexpr},"));
                    }
                    self.domain_lines.push("        }".to_string());
                    self.domain_lines.push("    }".to_string());
                    self.domain_lines.push("}".to_string());
                }
                self.domain_lines.push(format!(
                    "const _: fn() = || {{ fn assert_domain_bounds<T: Send + Sync + Clone + Default + 'static>() {{}} assert_domain_bounds::<{nm}>(); }};"
                ));
                self.domain_lines.push(String::new());
            }
        }

        Ok(Some(self.domain_lines.join("\n") + "\n"))
    }

    fn resource_payload_ty(&mut self, r: ResourceId) -> Result<String, String> {
        let res = self.program.resource(r).clone();
        match &res.ty {
            Some(bt) => self.resolve_rust_ty(res.module, res.raw_ty.as_ref(), bt, false),
            None => Ok("()".to_string()),
        }
    }

    fn line(&self) -> usize {
        self.lines.len() + 1
    }

    fn emit(&mut self, code: impl AsRef<str>) -> usize {
        let start = self.line();
        for l in code.as_ref().lines() {
            self.lines.push(l.to_string());
        }
        start
    }

    fn resource_fqn(&self, r: ResourceId) -> String {
        let res = self.program.resource(r);
        crate::fqn::fqn(self.program.module_name(res.module), &res.name)
    }

    fn function_fqn(&self, f: crate::sem::ids::FunctionId) -> String {
        let fun = self.program.function(f);
        crate::fqn::fqn(self.program.module_name(fun.module), &fun.name)
    }

    fn field(&self, r: ResourceId) -> String {
        let res = self.program.resource(r);
        format!(
            "r_{}__{}",
            sanitize(self.program.module_name(res.module)),
            sanitize(&res.name)
        )
    }

    /// Cross-module path to a generated function.
    fn fn_ref(&self, f: crate::sem::ids::FunctionId) -> String {
        let func = self.program.function(f);
        format!(
            "crate::{}::cf_{}",
            sanitize(self.program.module_name(func.module)),
            sanitize(&func.name)
        )
    }

    #[allow(unused)]
    fn tag_expr(&self, tag: &str) -> String {
        format!("{tag:?}")
    }

    fn hole(&mut self, module: &str, function: &str, sid: &str, expected: &str) -> String {
        self.hole_seq += 1;
        let id = format!("h{}", self.hole_seq);
        let line = self.line();
        self.holes.push(Hole {
            id: id.clone(),
            module: module.to_string(),
            function: function.to_string(),
            sid: sid.to_string(),
            expected: expected.to_string(),
            file: self.current_file.clone(),
            line,
            reads: Vec::new(),
            writes: Vec::new(),
            req: Vec::new(),
        });
        format!("/* HOLE({id}) expected: {expected} */ Default::default()")
    }

    fn is_std_atomic(&self, r: ResourceId) -> bool {
        let res = self.program.resource(r);
        if res.kind != ResKind::Atomic {
            return false;
        }
        matches!(
            res.ty.as_ref().and_then(|bt| rust_ty(bt).ok()).as_deref(),
            Some("i64" | "bool")
        )
    }

    fn render_shared_read(&self, r: ResourceId) -> String {
        let f = self.field(r);
        let res = self.program.resource(r);
        match res.kind {
            ResKind::Var => format!("shared.{f}.read()"),
            ResKind::Atomic if self.is_std_atomic(r) => {
                format!("shared.{f}.load(std::sync::atomic::Ordering::SeqCst)")
            }
            _ => format!("shared.{f}.lock().unwrap().clone()"),
        }
    }

    fn render_shared_write(&self, r: ResourceId, val_expr: &str) -> String {
        let f = self.field(r);
        let res = self.program.resource(r);
        match res.kind {
            ResKind::Var => format!("shared.{f}.write({val_expr});"),
            ResKind::Atomic if self.is_std_atomic(r) => {
                format!("shared.{f}.store({val_expr}, std::sync::atomic::Ordering::SeqCst);")
            }
            _ => format!("*shared.{f}.lock().unwrap() = {val_expr};"),
        }
    }

    fn render_expr(&mut self, module: &str, function: &str, sid: &str, e: &LExpr) -> String {
        match e {
            LExpr::Lit(l) => match render_lit(l) {
                Ok(s) => s,
                Err(_) => self.hole(module, function, sid, "literal"),
            },
            LExpr::Slot(SlotRef::Local(i)) => format!("_l{i}.clone()"),
            LExpr::Slot(SlotRef::Discard) => "()".into(),
            LExpr::Slot(SlotRef::Shared(r)) => self.render_shared_read(*r),
            LExpr::Field { base, field } => {
                let b = self.render_expr(module, function, sid, base);
                format!("({b}).{}", sanitize(field))
            }
            LExpr::Neg(inner) => format!("(-{})", self.render_expr(module, function, sid, inner)),
            LExpr::Bin { op, lhs, rhs } => {
                let a = self.render_expr(module, function, sid, lhs);
                let b = self.render_expr(module, function, sid, rhs);
                let o = match op {
                    BinOp::Add => "+",
                    BinOp::Sub => "-",
                    BinOp::Mul => "*",
                    BinOp::Div => "/",
                    BinOp::Mod => "%",
                };
                format!("({a} {o} {b})")
            }
            LExpr::Cmp { op, lhs, rhs } => {
                let a = self.render_expr(module, function, sid, lhs);
                let b = self.render_expr(module, function, sid, rhs);
                let o = match op {
                    CmpOp::Eq => "==",
                    CmpOp::Ne => "!=",
                    CmpOp::Lt => "<",
                    CmpOp::Le => "<=",
                    CmpOp::Gt => ">",
                    CmpOp::Ge => ">=",
                };
                format!("({a} {o} {b})")
            }
            _ => self.hole(module, function, sid, "expression"),
        }
    }
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
pub fn child_tags(f: &crate::sem::program::SemFunction, sid: &str) -> Vec<String> {
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

fn ty_contains_struct(bt: &BaseType) -> bool {
    match bt {
        BaseType::Complex(ComplexBaseType::Struct(_)) => true,
        BaseType::Complex(ComplexBaseType::Array(def)) => ty_contains_struct(&def.elem),
        _ => false,
    }
}

fn program_needs_domain(program: &SemProgram) -> bool {
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
                generate_function(&mut g, func)?;
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
                generate_function(&mut g, func)?;
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

enum StructuredNode {
    Stmt(usize),
    While {
        branch_idx: usize,
        body: Vec<StructuredNode>,
        back_goto_idx: usize,
    },
    IfElse {
        branch_idx: usize,
        then_nodes: Vec<StructuredNode>,
        then_goto_idx: Option<usize>,
        else_nodes: Vec<StructuredNode>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockMode {
    Mutex,
    RwRead,
    RwWrite,
}

fn try_structure_function(func: &crate::sem::program::SemFunction) -> Option<Vec<StructuredNode>> {
    use std::collections::BTreeSet;

    let mut total_jump_edges = 0usize;
    for (idx, stmt) in func.body.iter().enumerate() {
        match &stmt.op {
            SemOp::Switch { .. } | SemOp::Unsupported { .. } => return None,
            SemOp::Goto { .. } => total_jump_edges += 1,
            SemOp::Branch { .. } => total_jump_edges += 2,
            SemOp::MutexUnlock { .. } | SemOp::RwLockUnlock { .. } => {
                // Ensure every unlock corresponds to an earlier lock in structured scope;
                // balanced SCIR/CIR functions satisfy this, and rustc verifies lexical drop.
                let _ = idx;
            }
            _ => {}
        }
    }

    let mut consumed_edges: BTreeSet<(usize, usize)> = BTreeSet::new();
    let nodes = try_structure_range(&func.body, 0, func.body.len(), &mut consumed_edges)?;
    if consumed_edges.len() != total_jump_edges {
        return None;
    }
    // Also verify mutex/rwlock locks/unlocks do not cross branch boundaries asymmetrically
    // (e.g. locked inside `if` without unlocking inside the same `if`).
    let mut held: BTreeMap<ResourceId, LockMode> = BTreeMap::new();
    if !check_lexical_locks(&func.body, &nodes, &mut held) {
        return None;
    }
    Some(nodes)
}

/// Returns `true` if `nodes` preserves lexical `MutexGuard`/`RwLockGuard` scoping.
fn check_lexical_locks(
    body: &[crate::sem::program::SemStmt],
    nodes: &[StructuredNode],
    held: &mut BTreeMap<ResourceId, LockMode>,
) -> bool {
    check_lexical_locks_inner(body, nodes, held).is_some()
}

fn check_lexical_locks_inner(
    body: &[crate::sem::program::SemStmt],
    nodes: &[StructuredNode],
    held: &mut BTreeMap<ResourceId, LockMode>,
) -> Option<bool> {
    for node in nodes {
        match node {
            StructuredNode::Stmt(idx) => match &body[*idx].op {
                SemOp::MutexLock { resource } => {
                    if held.insert(*resource, LockMode::Mutex).is_some() {
                        return None;
                    }
                }
                SemOp::MutexUnlock { resource } => {
                    if held.remove(resource) != Some(LockMode::Mutex) {
                        return None;
                    }
                }
                SemOp::RwLockRead { resource } => {
                    if held.insert(*resource, LockMode::RwRead).is_some() {
                        return None;
                    }
                }
                SemOp::RwLockWrite { resource } => {
                    if held.insert(*resource, LockMode::RwWrite).is_some() {
                        return None;
                    }
                }
                SemOp::RwLockUnlock { resource } => {
                    if !matches!(
                        held.remove(resource),
                        Some(LockMode::RwRead | LockMode::RwWrite)
                    ) {
                        return None;
                    }
                }
                SemOp::CondvarWait { lock, .. } => {
                    if held.get(lock) != Some(&LockMode::Mutex) {
                        return None;
                    }
                }
                SemOp::Return { .. } => return Some(true),
                _ => {}
            },
            StructuredNode::While { body: inner, .. } => {
                let before = held.clone();
                let div = check_lexical_locks_inner(body, inner, held)?;
                if !div && *held != before {
                    return None;
                }
                *held = before;
            }
            StructuredNode::IfElse {
                then_nodes,
                else_nodes,
                ..
            } => {
                let before = held.clone();
                let mut then_held = before.clone();
                let mut else_held = before.clone();
                let then_div = check_lexical_locks_inner(body, then_nodes, &mut then_held)?;
                let else_div = check_lexical_locks_inner(body, else_nodes, &mut else_held)?;
                // Any lock acquired inside a branch must be unlocked inside that branch unless diverging.
                if !then_div && then_held != before {
                    return None;
                }
                if !else_div && else_held != before {
                    return None;
                }
                if then_div && else_div {
                    return Some(true);
                }
                *held = before;
            }
        }
    }
    Some(false)
}

fn try_structure_range(
    body: &[crate::sem::program::SemStmt],
    start: usize,
    end: usize,
    consumed: &mut std::collections::BTreeSet<(usize, usize)>,
) -> Option<Vec<StructuredNode>> {
    let mut out = Vec::new();
    let mut i = start;
    while i < end {
        match &body[i].op {
            SemOp::Branch {
                then, else_target, ..
            } => {
                if *then != i + 1 || *else_target <= i + 1 || *else_target > end {
                    return None;
                }
                // Check natural while loop: `stmt[else_target - 1]` is `Goto { target: i }`.
                if let SemOp::Goto { target } = &body[*else_target - 1].op
                    && *target == i
                {
                    consumed.insert((i, *then));
                    consumed.insert((i, *else_target));
                    consumed.insert((*else_target - 1, i));
                    let inner = try_structure_range(body, i + 1, *else_target - 1, consumed)?;
                    out.push(StructuredNode::While {
                        branch_idx: i,
                        body: inner,
                        back_goto_idx: *else_target - 1,
                    });
                    i = *else_target;
                    continue;
                }
                // Check if-else diamond: `stmt[else_target - 1]` is forward `Goto { target: merge }`.
                if *else_target >= i + 2
                    && let SemOp::Goto { target: merge } = &body[*else_target - 1].op
                    && *merge >= *else_target
                    && *merge <= end
                {
                    consumed.insert((i, *then));
                    consumed.insert((i, *else_target));
                    consumed.insert((*else_target - 1, *merge));
                    let then_nodes = try_structure_range(body, i + 1, *else_target - 1, consumed)?;
                    let else_nodes = try_structure_range(body, *else_target, *merge, consumed)?;
                    out.push(StructuredNode::IfElse {
                        branch_idx: i,
                        then_nodes,
                        then_goto_idx: Some(*else_target - 1),
                        else_nodes,
                    });
                    i = *merge;
                    continue;
                }
                // One-armed `if` (`then` falls through to `else_target` or returns).
                consumed.insert((i, *then));
                consumed.insert((i, *else_target));
                let then_nodes = try_structure_range(body, i + 1, *else_target, consumed)?;
                out.push(StructuredNode::IfElse {
                    branch_idx: i,
                    then_nodes,
                    then_goto_idx: None,
                    else_nodes: Vec::new(),
                });
                i = *else_target;
            }
            SemOp::Goto { .. } | SemOp::Switch { .. } | SemOp::Unsupported { .. } => {
                return None;
            }
            _ => {
                out.push(StructuredNode::Stmt(i));
                i += 1;
            }
        }
    }
    Some(out)
}

fn generate_function(g: &mut Gen, func: &crate::sem::program::SemFunction) -> Result<(), String> {
    if let Some(nodes) = try_structure_function(func) {
        return generate_structured_function(g, func, &nodes);
    }
    generate_pc_function(g, func)
}

fn generate_structured_function(
    g: &mut Gen,
    func: &crate::sem::program::SemFunction,
    nodes: &[StructuredNode],
) -> Result<(), String> {
    let name = format!("cf_{}", sanitize(&func.name));
    g.emit(format!(
        "    pub(crate) fn {name}(#[allow(unused_variables)] shared: std::sync::Arc<crate::Shared>, #[allow(unused_variables)] tag: &str) {{"
    ));
    for (i, slot) in func.slots.iter().enumerate() {
        let ty = g.resolve_rust_ty(func.module, Some(&slot.raw_ty), &slot.ty, false)?;
        let init = match &slot.init {
            Some(v) => g.render_value_typed(v, func.module, Some(&slot.raw_ty), &slot.ty)?,
            None => default_expr(&ty),
        };
        g.emit(format!(
            "        #[allow(unused_mut, unused_variables)] let mut _l{i}: {ty} = {init};"
        ));
    }
    let mut rw_modes: BTreeMap<ResourceId, LockMode> = BTreeMap::new();
    emit_structured_nodes(g, func, nodes, "        ", &mut rw_modes)?;
    g.emit("    }");
    g.emit("");
    Ok(())
}

fn record_sid_line(g: &mut Gen, func: &crate::sem::program::SemFunction, sid: &str, indent: &str) {
    let loc = g.line();
    g.sids.insert(
        format!("{}::{}", g.program.module_name(func.module), sid),
        SidLocation {
            file: g.current_file.clone(),
            line: loc,
        },
    );
    g.emit(format!("{indent}// @cir {sid}"));
}

fn emit_structured_nodes(
    g: &mut Gen,
    func: &crate::sem::program::SemFunction,
    nodes: &[StructuredNode],
    indent: &str,
    rw_modes: &mut BTreeMap<ResourceId, LockMode>,
) -> Result<(), String> {
    let mod_name = g.program.module_name(func.module).to_string();
    for node in nodes {
        match node {
            StructuredNode::While {
                branch_idx,
                body,
                back_goto_idx,
            } => {
                let br_stmt = &func.body[*branch_idx];
                let SemOp::Branch { cond, .. } = &br_stmt.op else {
                    unreachable!();
                };
                record_sid_line(g, func, &br_stmt.sid, indent);
                let c = g.render_expr(&mod_name, &func.name, &br_stmt.sid, cond);
                g.emit(format!("{indent}while {c} {{"));
                let inner_indent = format!("{indent}    ");
                let mut inner_modes = rw_modes.clone();
                emit_structured_nodes(g, func, body, &inner_indent, &mut inner_modes)?;
                let back_sid = &func.body[*back_goto_idx].sid;
                record_sid_line(g, func, back_sid, &inner_indent);
                g.emit(format!("{indent}}}"));
            }
            StructuredNode::IfElse {
                branch_idx,
                then_nodes,
                then_goto_idx,
                else_nodes,
            } => {
                let br_stmt = &func.body[*branch_idx];
                let SemOp::Branch { cond, .. } = &br_stmt.op else {
                    unreachable!();
                };
                record_sid_line(g, func, &br_stmt.sid, indent);
                let c = g.render_expr(&mod_name, &func.name, &br_stmt.sid, cond);
                let inner_indent = format!("{indent}    ");
                g.emit(format!("{indent}if {c} {{"));
                let mut then_modes = rw_modes.clone();
                emit_structured_nodes(g, func, then_nodes, &inner_indent, &mut then_modes)?;
                if let Some(goto_idx) = then_goto_idx {
                    let goto_sid = &func.body[*goto_idx].sid;
                    record_sid_line(g, func, goto_sid, &inner_indent);
                }
                if else_nodes.is_empty() {
                    g.emit(format!("{indent}}}"));
                } else {
                    g.emit(format!("{indent}}} else {{"));
                    let mut else_modes = rw_modes.clone();
                    emit_structured_nodes(g, func, else_nodes, &inner_indent, &mut else_modes)?;
                    g.emit(format!("{indent}}}"));
                }
            }
            StructuredNode::Stmt(idx) => {
                let stmt = &func.body[*idx];
                let sid = &stmt.sid;
                record_sid_line(g, func, sid, indent);
                emit_structured_stmt(g, func, &stmt.op, sid, indent, rw_modes)?;
            }
        }
    }
    Ok(())
}

fn emit_structured_stmt(
    g: &mut Gen,
    func: &crate::sem::program::SemFunction,
    op: &SemOp,
    sid: &str,
    indent: &str,
    rw_modes: &mut BTreeMap<ResourceId, LockMode>,
) -> Result<(), String> {
    let mod_name = g.program.module_name(func.module).to_string();
    match op {
        SemOp::Nop => {}
        SemOp::AssignLocal { target, expr } => {
            let e = g.render_expr(&mod_name, &func.name, sid, expr);
            match target {
                SlotRef::Local(i) => {
                    g.emit(format!("{indent}_l{i} = {e};"));
                }
                SlotRef::Discard => {}
                SlotRef::Shared(r) => {
                    let w = g.render_shared_write(*r, &e);
                    g.emit(format!("{indent}{w}"));
                }
            }
        }
        SemOp::ReadShared { resource, dst } => {
            let rd = g.render_shared_read(*resource);
            match dst {
                Some(SlotRef::Local(i)) => {
                    g.emit(format!("{indent}_l{i} = {rd};"));
                }
                Some(SlotRef::Shared(r)) => {
                    let w = g.render_shared_write(*r, "__v");
                    g.emit(format!("{indent}{{ let __v = {rd}; {w} }}"));
                }
                _ => {
                    g.emit(format!("{indent}let _ = {rd};"));
                }
            }
        }
        SemOp::WriteShared { resource, expr } => {
            let e = g.render_expr(&mod_name, &func.name, sid, expr);
            let w = g.render_shared_write(*resource, "__v");
            g.emit(format!("{indent}{{ let __v = {e}; {w} }}"));
        }
        SemOp::MutexLock { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            let payload = g.resource_payload_ty(*resource)?;
            g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
            g.emit(format!(
                "{indent}#[allow(unused_mut, unused_variables)] let mut _g_{f} = crate::cir_trace::lock(&shared.{f}, tag, {rq:?}, {sid:?});"
            ));
            if payload == "()" {
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut _g_{f} = crate::__res::{f}.lock().unwrap();"
                ));
            } else {
                let def_val = default_expr(&payload);
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut __spn_g_{f} = crate::__res::{f}.lock().unwrap();"
                ));
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut __payload_g_{f}: {payload} = {def_val};"
                ));
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut _g_{f}: &mut {payload} = &mut __payload_g_{f};"
                ));
            }
        }
        SemOp::MutexUnlock { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            let payload = g.resource_payload_ty(*resource)?;
            g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
            g.emit(format!(
                "{indent}crate::cir_trace::unlock(Some(_g_{f}), tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
            if payload == "()" {
                g.emit(format!("{indent}drop(_g_{f});"));
            } else {
                g.emit(format!(
                    "{indent}{{ let _ = _g_{f}; drop(__payload_g_{f}); drop(__spn_g_{f}); }}"
                ));
            }
        }
        SemOp::RwLockRead { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            let payload = g.resource_payload_ty(*resource)?;
            rw_modes.insert(*resource, LockMode::RwRead);
            g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
            g.emit(format!(
                "{indent}#[allow(unused_mut, unused_variables)] let mut _rg_{f} = crate::cir_trace::rwlock_read_lock(&shared.{f}, tag, {rq:?}, {sid:?});"
            ));
            if payload == "()" {
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut _rg_{f} = crate::__res::{f}.read().unwrap();"
                ));
            } else {
                let def_val = default_expr(&payload);
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut __spn_rg_{f} = crate::__res::{f}.read().unwrap();"
                ));
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_variables)] let __payload_rg_{f}: {payload} = {def_val};"
                ));
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_variables)] let _rg_{f}: &{payload} = &__payload_rg_{f};"
                ));
            }
        }
        SemOp::RwLockWrite { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            let payload = g.resource_payload_ty(*resource)?;
            rw_modes.insert(*resource, LockMode::RwWrite);
            g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
            g.emit(format!(
                "{indent}#[allow(unused_mut, unused_variables)] let mut _wg_{f} = crate::cir_trace::rwlock_write_lock(&shared.{f}, tag, {rq:?}, {sid:?});"
            ));
            if payload == "()" {
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut _wg_{f} = crate::__res::{f}.write().unwrap();"
                ));
            } else {
                let def_val = default_expr(&payload);
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut __spn_wg_{f} = crate::__res::{f}.write().unwrap();"
                ));
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut __payload_wg_{f}: {payload} = {def_val};"
                ));
                g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                g.emit(format!(
                    "{indent}#[allow(unused_mut, unused_variables)] let mut _wg_{f}: &mut {payload} = &mut __payload_wg_{f};"
                ));
            }
        }
        SemOp::RwLockUnlock { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            let payload = g.resource_payload_ty(*resource)?;
            match rw_modes.remove(resource) {
                Some(LockMode::RwWrite) => {
                    g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
                    g.emit(format!(
                        "{indent}crate::cir_trace::rwlock_write_unlock(Some(_wg_{f}), tag, {rq:?}, {sid:?});"
                    ));
                    g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                    if payload == "()" {
                        g.emit(format!("{indent}drop(_wg_{f});"));
                    } else {
                        g.emit(format!(
                            "{indent}{{ let _ = _wg_{f}; drop(__payload_wg_{f}); drop(__spn_wg_{f}); }}"
                        ));
                    }
                }
                _ => {
                    g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
                    g.emit(format!(
                        "{indent}crate::cir_trace::rwlock_read_unlock(Some(_rg_{f}), tag, {rq:?}, {sid:?});"
                    ));
                    g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
                    if payload == "()" {
                        g.emit(format!("{indent}drop(_rg_{f});"));
                    } else {
                        g.emit(format!(
                            "{indent}{{ let _ = _rg_{f}; drop(__payload_rg_{f}); drop(__spn_rg_{f}); }}"
                        ));
                    }
                }
            }
        }
        SemOp::CondvarWait { condvar, lock } => {
            let c = g.field(*condvar);
            let l = g.field(*lock);
            let rq = g.resource_fqn(*condvar);
            let lock_payload = g.resource_payload_ty(*lock)?;
            g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
            g.emit(format!(
                "{indent}{{ _g_{l} = crate::cir_trace::condvar_wait(&shared.{c}, _g_{l}, tag, {rq:?}, {sid:?}); }}"
            ));
            g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
            if lock_payload == "()" {
                g.emit(format!("{indent}{{ _g_{l} = crate::__res::{c}.wait(_g_{l}).unwrap(); }}"));
            } else {
                g.emit(format!(
                    "{indent}{{ __spn_g_{l} = crate::__res::{c}.wait(__spn_g_{l}).unwrap(); }}"
                ));
            }
        }
        SemOp::CondvarNotify { condvar } => {
            let c = g.field(*condvar);
            let rq = g.resource_fqn(*condvar);
            g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
            g.emit(format!(
                "{indent}crate::cir_trace::notify_one(&shared.{c}, tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
            g.emit(format!("{indent}crate::__res::{c}.notify_one();"));
        }
        SemOp::CondvarNotifyAll { condvar } => {
            let c = g.field(*condvar);
            let rq = g.resource_fqn(*condvar);
            g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
            g.emit(format!(
                "{indent}crate::cir_trace::notify_all(&shared.{c}, tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
            g.emit(format!("{indent}crate::__res::{c}.notify_all();"));
        }
        SemOp::SemaphoreAcquire { resource, count } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "{indent}shared.{f}.acquire_sid({count}, tag, {rq:?}, {sid:?});"
            ));
        }
        SemOp::SemaphoreRelease { resource, count } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "{indent}shared.{f}.release_sid({count}, tag, {rq:?}, {sid:?});"
            ));
        }
        SemOp::Return { value } => {
            if let Some(e) = value {
                let _ = g.render_expr(&mod_name, &func.name, sid, e);
            }
            g.emit(format!("{indent}return;"));
        }
        SemOp::SeqHole {
            id,
            reads,
            writes,
            updates_modeled,
            expected_type,
            req,
            ..
        } => {
            let expected_ty = expected_rust_ty(g, func.module, expected_type);
            let hole_line = g.line();
            g.holes.push(Hole {
                id: id.clone(),
                module: mod_name.clone(),
                function: func.name.clone(),
                sid: sid.to_string(),
                expected: expected_ty.clone(),
                file: g.current_file.clone(),
                line: hole_line,
                reads: reads.clone(),
                writes: writes.clone(),
                req: req.clone(),
            });
            g.emit(format!(
                "{indent}let _hole_val: {expected_ty} = /* HOLE({id}) expected: {expected_ty} */ Default::default();"
            ));
            if !updates_modeled.is_empty() {
                g.emit(format!("{indent}{{"));
                for (ui, u) in updates_modeled.iter().enumerate() {
                    let rhs = g.render_expr(&mod_name, &func.name, sid, &u.expr);
                    g.emit(format!("{indent}    let __u{ui} = {rhs};"));
                }
                for (ui, u) in updates_modeled.iter().enumerate() {
                    match u.target {
                        SlotRef::Local(i) => {
                            g.emit(format!("{indent}    _l{i} = __u{ui};"));
                        }
                        SlotRef::Shared(r) => {
                            let w = g.render_shared_write(r, &format!("__u{ui}"));
                            g.emit(format!("{indent}    {w}"));
                        }
                        SlotRef::Discard => {}
                    }
                }
                g.emit(format!("{indent}}}"));
            }
        }
        SemOp::Scope { funcs } => {
            let tags = child_tags(func, sid);
            g.tags.push(TagRecord {
                sid: sid.to_string(),
                function: func.name.clone(),
                tags: tags.clone(),
            });
            g.emit(format!("{indent}crate::cir_trace::scope(tag, {sid:?});"));
            g.emit(format!("{indent}{{"));
            for (i, f) in funcs.iter().enumerate() {
                let cf = g.fn_ref(*f);
                let t = tags
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("t{sid}_{}", i + 1));
                g.emit(format!(
                    "{indent}    let __h{i} = {{ let __sh = std::sync::Arc::clone(&shared); std::thread::spawn(move || {cf}(__sh, {t:?})) }};"
                ));
            }
            for i in 0..funcs.len() {
                g.emit(format!("{indent}    __h{i}.join().unwrap();"));
            }
            g.emit(format!("{indent}}}"));
        }
        SemOp::Spawn {
            func: child,
            handle,
        } => {
            let tags = child_tags(func, sid);
            g.tags.push(TagRecord {
                sid: sid.to_string(),
                function: func.name.clone(),
                tags: tags.clone(),
            });
            let cf = g.fn_ref(*child);
            let t = tags.first().cloned().unwrap_or_else(|| format!("t{sid}"));
            let h = format!("_h_{}", sanitize(handle));
            let child_fqn = g.function_fqn(*child);
            g.emit(format!(
                "{indent}crate::cir_trace::record(tag, \"spawn\", {child_fqn:?}, {sid:?});"
            ));
            g.emit(format!(
                "{indent}let {h} = {{ let __sh = std::sync::Arc::clone(&shared); std::thread::spawn(move || {cf}(__sh, {t:?})) }};"
            ));
        }
        SemOp::Join { handle } => {
            let h = format!("_h_{}", sanitize(handle));
            g.emit(format!(
                "{indent}crate::cir_trace::record(tag, \"join\", \"\", {sid:?});"
            ));
            g.emit(format!("{indent}{h}.join().unwrap();"));
        }
        SemOp::Call {
            func: callee,
            args,
            dst,
        } => {
            let cf = g.fn_ref(*callee);
            for a in args {
                let _ = g.render_expr(&mod_name, &func.name, sid, a);
            }
            if dst.is_some() {
                g.emit(format!("{indent}let _ = {cf}(shared.clone(), tag);"));
            } else {
                g.emit(format!("{indent}{cf}(shared.clone(), tag);"));
            }
        }
        SemOp::AtomicLoad { resource, dst } => {
            let rd = g.render_shared_read(*resource);
            match dst {
                SlotRef::Local(i) => {
                    g.emit(format!("{indent}_l{i} = {rd};"));
                }
                SlotRef::Shared(r) => {
                    let w = g.render_shared_write(*r, "__v");
                    g.emit(format!("{indent}{{ let __v = {rd}; {w} }}"));
                }
                SlotRef::Discard => {
                    g.emit(format!("{indent}let _ = {rd};"));
                }
            }
        }
        SemOp::AtomicStore { resource, value } => {
            let e = g.render_expr(&mod_name, &func.name, sid, value);
            let w = g.render_shared_write(*resource, "__v");
            g.emit(format!("{indent}{{ let __v = {e}; {w} }}"));
        }
        SemOp::AtomicCas {
            resource,
            expected,
            desired,
            dst,
        } => {
            let f = g.field(*resource);
            let ex = g.render_expr(&mod_name, &func.name, sid, expected);
            let de = g.render_expr(&mod_name, &func.name, sid, desired);
            let assign = match dst {
                SlotRef::Local(i) => format!("_l{i} = __old;"),
                SlotRef::Shared(r) => g.render_shared_write(*r, "__old"),
                SlotRef::Discard => "let _ = __old;".to_string(),
            };
            if g.is_std_atomic(*resource) {
                g.emit(format!(
                    "{indent}{{ let __old = match shared.{f}.compare_exchange({ex}, {de}, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst) {{ Ok(v) | Err(v) => v }}; {assign} }}"
                ));
            } else {
                g.emit(format!(
                    "{indent}{{ let mut __g = shared.{f}.lock().unwrap(); let __old = __g.clone(); if __old == {ex} {{ *__g = {de}; }} drop(__g); {assign} }}"
                ));
            }
        }
        SemOp::ChannelSend { channel, value } => {
            let f = g.field(*channel);
            let rq = g.resource_fqn(*channel);
            let e = g.render_expr(&mod_name, &func.name, sid, value);
            g.emit(format!(
                "{indent}shared.{f}.send_sid({e}, tag, {rq:?}, {sid:?});"
            ));
        }
        SemOp::ChannelRecv { channel, dst } => {
            let f = g.field(*channel);
            let rq = g.resource_fqn(*channel);
            match dst {
                SlotRef::Local(i) => {
                    g.emit(format!(
                        "{indent}_l{i} = shared.{f}.recv_sid(tag, {rq:?}, {sid:?});"
                    ));
                }
                SlotRef::Shared(r) => {
                    let w = g.render_shared_write(*r, "__v");
                    g.emit(format!(
                        "{indent}{{ let __v = shared.{f}.recv_sid(tag, {rq:?}, {sid:?}); {w} }}"
                    ));
                }
                SlotRef::Discard => {
                    g.emit(format!(
                        "{indent}let _ = shared.{f}.recv_sid(tag, {rq:?}, {sid:?});"
                    ));
                }
            }
        }
        SemOp::Goto { .. } | SemOp::Branch { .. } | SemOp::Switch { .. } => {
            return Err("unexpected unstructured control op in structured emitter".into());
        }
        SemOp::Unsupported { construct } => {
            return Err(format!("unsupported construct in codegen: {construct}"));
        }
    }
    Ok(())
}

fn generate_pc_function(g: &mut Gen, func: &crate::sem::program::SemFunction) -> Result<(), String> {
    let name = format!("cf_{}", sanitize(&func.name));
    g.emit(format!("    pub(crate) fn {name}(shared: std::sync::Arc<crate::Shared>, tag: &str) {{"));
    // slot declarations
    for (i, slot) in func.slots.iter().enumerate() {
        let ty = g.resolve_rust_ty(func.module, Some(&slot.raw_ty), &slot.ty, false)?;
        let init = match &slot.init {
            Some(v) => g.render_value_typed(v, func.module, Some(&slot.raw_ty), &slot.ty)?,
            None => default_expr(&ty),
        };
        g.emit(format!(
            "    #[allow(unused_mut)] let mut _l{i}: {ty} = {init};"
        ));
    }
    // guard declarations for every mutex / rwlock
    let mut guards = Vec::new();
    let res_list: Vec<_> = g.program.resources().to_vec();
    for r in &res_list {
        match r.kind {
            ResKind::Mutex => {
                let f = g.field(r.id);
                let payload = g.resource_payload_ty(r.id)?;
                g.emit(format!(
                    "    #[allow(unused_mut)] let mut _g_{f}: Option<std::sync::MutexGuard<'_, {payload}>> = None;"
                ));
                guards.push(f);
            }
            ResKind::RwLock => {
                let f = g.field(r.id);
                let payload = g.resource_payload_ty(r.id)?;
                g.emit(format!(
                    "    #[allow(unused_mut)] let mut _rg_{f}: Option<std::sync::RwLockReadGuard<'_, {payload}>> = None;"
                ));
                g.emit(format!(
                    "    #[allow(unused_mut)] let mut _wg_{f}: Option<std::sync::RwLockWriteGuard<'_, {payload}>> = None;"
                ));
                guards.push(f);
            }
            _ => {}
        }
    }
    g.emit("    let mut pc: usize = 0;");
    g.emit("    loop {");
    g.emit("        match pc {");

    let body_len = func.body.len();
    for (idx, stmt) in func.body.iter().enumerate() {
        let sid = stmt.sid.clone();
        let loc = g.line();
        g.sids.insert(
            format!("{}::{}", g.program.module_name(func.module), sid),
            SidLocation {
                file: g.current_file.clone(),
                line: loc,
            },
        );
        g.emit(format!("            {idx} => {{"));
        g.emit(format!("                // @cir {sid}"));
        render_op(g, func, &stmt.op, idx, body_len, &sid, &guards)?;
        g.emit("            }");
    }
    g.emit("            _ => return,");
    g.emit("        }");
    g.emit("    }");
    g.emit("}");
    g.emit("");
    Ok(())
}

fn expected_rust_ty(g: &Gen, from_module: ModuleId, expected: &str) -> String {
    match expected.trim() {
        "" | "()" => "()".to_string(),
        "Int" => "i64".to_string(),
        "Bool" => "bool".to_string(),
        "Float" => "f64".to_string(),
        other => {
            if let Some(td) = g.program.resolve_type_def(from_module, other) {
                if let Some(rname) = g.named_types.get(&(td.module, td.name.clone())) {
                    return format!("crate::domain::{rname}");
                }
            }
            other.to_string()
        }
    }
}

fn render_op(
    g: &mut Gen,
    func: &crate::sem::program::SemFunction,
    op: &SemOp,
    idx: usize,
    body_len: usize,
    sid: &str,
    guards: &[String],
) -> Result<(), String> {
    let next = idx + 1;
    let mod_name = g.program.module_name(func.module).to_string();
    match op {
        SemOp::Nop => {
            g.emit(format!("                pc = {next};"));
        }
        SemOp::AssignLocal { target, expr } => {
            let e = g.render_expr(&mod_name, &func.name, sid, expr);
            match target {
                SlotRef::Local(i) => {
                    g.emit(format!("                _l{i} = {e};"));
                }
                SlotRef::Discard => {}
                SlotRef::Shared(r) => {
                    let w = g.render_shared_write(*r, &e);
                    g.emit(format!("                {w}"));
                }
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::ReadShared { resource, dst } => {
            let rd = g.render_shared_read(*resource);
            match dst {
                Some(SlotRef::Local(i)) => {
                    g.emit(format!("                _l{i} = {rd};"));
                }
                Some(SlotRef::Shared(r)) => {
                    let w = g.render_shared_write(*r, "__v");
                    g.emit(format!("                {{ let __v = {rd}; {w} }}"));
                }
                _ => {
                    g.emit(format!("                let _ = {rd};"));
                }
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::WriteShared { resource, expr } => {
            let e = g.render_expr(&mod_name, &func.name, sid, expr);
            let w = g.render_shared_write(*resource, "__v");
            g.emit(format!("                {{ let __v = {e}; {w} }}"));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::MutexLock { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "                _g_{f} = Some(crate::cir_trace::lock(&shared.{f}, tag, {rq:?}, {sid:?}));"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::MutexUnlock { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "                crate::cir_trace::unlock(_g_{f}.take(), tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::RwLockRead { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "                _rg_{f} = Some(crate::cir_trace::rwlock_read_lock(&shared.{f}, tag, {rq:?}, {sid:?}));"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::RwLockWrite { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "                _wg_{f} = Some(crate::cir_trace::rwlock_write_lock(&shared.{f}, tag, {rq:?}, {sid:?}));"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::RwLockUnlock { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "                if let Some(__wg) = _wg_{f}.take() {{ crate::cir_trace::rwlock_write_unlock(Some(__wg), tag, {rq:?}, {sid:?}); }} else if let Some(__rg) = _rg_{f}.take() {{ crate::cir_trace::rwlock_read_unlock(Some(__rg), tag, {rq:?}, {sid:?}); }}"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::CondvarWait { condvar, lock } => {
            let c = g.field(*condvar);
            let l = g.field(*lock);
            let rq = g.resource_fqn(*condvar);
            g.emit(format!(
                "                _g_{l} = Some(crate::cir_trace::condvar_wait(&shared.{c}, _g_{l}.take().expect(\"wait without lock\"), tag, {rq:?}, {sid:?}));"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::CondvarNotify { condvar } => {
            let c = g.field(*condvar);
            let rq = g.resource_fqn(*condvar);
            g.emit(format!(
                "                crate::cir_trace::notify_one(&shared.{c}, tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::CondvarNotifyAll { condvar } => {
            let c = g.field(*condvar);
            let rq = g.resource_fqn(*condvar);
            g.emit(format!(
                "                crate::cir_trace::notify_all(&shared.{c}, tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::SemaphoreAcquire { resource, count } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "                shared.{f}.acquire_sid({count}, tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::SemaphoreRelease { resource, count } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!(
                "                shared.{f}.release_sid({count}, tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::Goto { target } => {
            g.emit(format!("                pc = {target};"));
        }
        SemOp::Branch {
            cond,
            then,
            else_target,
        } => {
            let c = g.render_expr(&mod_name, &func.name, sid, cond);
            g.emit(format!(
                "                pc = if {c} {{ {then} }} else {{ {else_target} }};"
            ));
        }
        SemOp::Switch {
            var,
            cases,
            default,
        } => {
            let v = g.render_expr(&mod_name, &func.name, sid, var);
            g.emit(format!("                pc = match {v} {{"));
            for (label, target) in cases {
                g.emit(format!("                    {label:?} => {target},"));
            }
            g.emit(format!("                    _ => {default},"));
            g.emit("                };");
        }
        SemOp::Return { value } => {
            if let Some(e) = value {
                let _ = g.render_expr(&mod_name, &func.name, sid, e);
            }
            g.emit("                return;");
        }
        SemOp::SeqHole {
            id,
            reads,
            writes,
            updates_modeled,
            expected_type,
            req,
            ..
        } => {
            let expected_ty = expected_rust_ty(g, func.module, expected_type);
            let hole_line = g.line();
            g.holes.push(Hole {
                id: id.clone(),
                module: mod_name.clone(),
                function: func.name.clone(),
                sid: sid.to_string(),
                expected: expected_ty.clone(),
                file: g.current_file.clone(),
                line: hole_line,
                reads: reads.clone(),
                writes: writes.clone(),
                req: req.clone(),
            });
            g.emit(format!(
                "                let _hole_val: {expected_ty} = /* HOLE({id}) expected: {expected_ty} */ Default::default();"
            ));
            if !updates_modeled.is_empty() {
                g.emit("                {");
                for (ui, u) in updates_modeled.iter().enumerate() {
                    let rhs = g.render_expr(&mod_name, &func.name, sid, &u.expr);
                    g.emit(format!("                    let __u{ui} = {rhs};"));
                }
                for (ui, u) in updates_modeled.iter().enumerate() {
                    match u.target {
                        SlotRef::Local(i) => {
                            g.emit(format!("                    _l{i} = __u{ui};"));
                        }
                        SlotRef::Shared(r) => {
                            let w = g.render_shared_write(r, &format!("__u{ui}"));
                            g.emit(format!("                    {w}"));
                        }
                        SlotRef::Discard => {}
                    }
                }
                g.emit("                }");
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::Scope { funcs } => {
            let tags = child_tags(func, sid);
            g.tags.push(TagRecord {
                sid: sid.to_string(),
                function: func.name.clone(),
                tags: tags.clone(),
            });
            g.emit(format!("                crate::cir_trace::scope(tag, {sid:?});"));
            g.emit("                {");
            g.emit("                    let mut __hs = Vec::new();");
            for (i, f) in funcs.iter().enumerate() {
                let cf = g.fn_ref(*f);
                let t = tags
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("t{sid}_{}", i + 1));
                g.emit("                    {");
                g.emit("                        let __sh = shared.clone();");
                g.emit(format!("                        __hs.push(std::thread::spawn(move || {cf}(__sh, {t:?})));"));
                g.emit("                    }");
            }
            g.emit("                    for __h in __hs { __h.join().unwrap(); }");
            g.emit("                }");
            g.emit(format!("                pc = {next};"));
        }
        SemOp::Spawn {
            func: child,
            handle,
        } => {
            let tags = child_tags(func, sid);
            g.tags.push(TagRecord {
                sid: sid.to_string(),
                function: func.name.clone(),
                tags: tags.clone(),
            });
            let cf = g.fn_ref(*child);
            let t = tags.first().cloned().unwrap_or_else(|| format!("t{sid}"));
            let h = format!("_h_{}", sanitize(handle));
            let child_fqn = g.function_fqn(*child);
            g.emit("                {");
            g.emit("                    let __sh = shared.clone();");
            g.emit(format!(
                "                    let {h} = crate::cir_trace::spawn(tag, {child_fqn:?}, {sid:?}, move || {cf}(__sh, {t:?}));"
            ));
            g.emit("                }");
            g.emit(format!("                pc = {next};"));
        }
        SemOp::Join { handle } => {
            let h = format!("_h_{}", sanitize(handle));
            g.emit(format!(
                "                crate::cir_trace::join({h}, tag, \"\", {sid:?});"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::Call {
            func: callee,
            args,
            dst,
        } => {
            let cf = g.fn_ref(*callee);
            for a in args {
                let _ = g.render_expr(&mod_name, &func.name, sid, a);
            }
            if dst.is_some() {
                g.emit(format!(
                    "                let _ = {cf}(shared.clone(), tag);"
                ));
            } else {
                g.emit(format!("                {cf}(shared.clone(), tag);"));
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::AtomicLoad { resource, dst } => {
            let rd = g.render_shared_read(*resource);
            match dst {
                SlotRef::Local(i) => {
                    g.emit(format!("                _l{i} = {rd};"));
                }
                SlotRef::Shared(r) => {
                    let w = g.render_shared_write(*r, "__v");
                    g.emit(format!("                {{ let __v = {rd}; {w} }}"));
                }
                SlotRef::Discard => {
                    g.emit(format!("                let _ = {rd};"));
                }
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::AtomicStore { resource, value } => {
            let e = g.render_expr(&mod_name, &func.name, sid, value);
            let w = g.render_shared_write(*resource, "__v");
            g.emit(format!("                {{ let __v = {e}; {w} }}"));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::AtomicCas {
            resource,
            expected,
            desired,
            dst,
        } => {
            let f = g.field(*resource);
            let ex = g.render_expr(&mod_name, &func.name, sid, expected);
            let de = g.render_expr(&mod_name, &func.name, sid, desired);
            if g.is_std_atomic(*resource) {
                let assign = match dst {
                    SlotRef::Local(i) => format!("_l{i} = __old;"),
                    SlotRef::Shared(r) => g.render_shared_write(*r, "__old"),
                    SlotRef::Discard => "let _ = __old;".to_string(),
                };
                g.emit(format!(
                    "                {{ let __old = match shared.{f}.compare_exchange({ex}, {de}, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst) {{ Ok(v) | Err(v) => v }}; {assign} }}"
                ));
            } else {
                let assign = match dst {
                    SlotRef::Local(i) => format!("_l{i} = __old;"),
                    SlotRef::Shared(r) => g.render_shared_write(*r, "__old"),
                    SlotRef::Discard => "let _ = __old;".to_string(),
                };
                g.emit(format!(
                    "                {{ let mut __g = shared.{f}.lock().unwrap(); let __old = __g.clone(); if __old == {ex} {{ *__g = {de}; }} drop(__g); {assign} }}"
                ));
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::ChannelSend { channel, value } => {
            let f = g.field(*channel);
            let rq = g.resource_fqn(*channel);
            let e = g.render_expr(&mod_name, &func.name, sid, value);
            g.emit(format!(
                "                shared.{f}.send_sid({e}, tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::ChannelRecv { channel, dst } => {
            let f = g.field(*channel);
            let rq = g.resource_fqn(*channel);
            match dst {
                SlotRef::Local(i) => {
                    g.emit(format!(
                        "                _l{i} = shared.{f}.recv_sid(tag, {rq:?}, {sid:?});"
                    ));
                }
                SlotRef::Shared(r) => {
                    let w = g.render_shared_write(*r, "__v");
                    g.emit(format!(
                        "                {{ let __v = shared.{f}.recv_sid(tag, {rq:?}, {sid:?}); {w} }}"
                    ));
                }
                SlotRef::Discard => {
                    g.emit(format!(
                        "                let _ = shared.{f}.recv_sid(tag, {rq:?}, {sid:?});"
                    ));
                }
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::Unsupported { construct } => {
            return Err(format!("unsupported construct in codegen: {construct}"));
        }
    }
    let _ = (body_len, guards);
    Ok(())
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

#[allow(dead_code)]
fn _unused(_: &mut String) {
    let _ = write!(String::new(), "");
}
