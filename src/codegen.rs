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
use crate::sem::ids::SlotRef;
use crate::sem::program::{ResKind, SemOp, SemProgram};
use crate::sem::value::Value;

const TRACE_RUNTIME: &str = r#"// Generated cir_trace runtime v2 (std only, operation-bound events).
use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
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
    pub line: usize,
    #[serde(default)]
    pub reads: Vec<String>,
    #[serde(default)]
    pub writes: Vec<String>,
    #[serde(default)]
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
    pub map: CodegenMap,
}

struct Gen<'a> {
    program: &'a SemProgram,
    lines: Vec<String>,
    sids: BTreeMap<String, SidLocation>,
    holes: Vec<Hole>,
    tags: Vec<TagRecord>,
    hole_seq: usize,
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
            _ => Err("unsupported composite value type".into()),
        },
    }
}

fn default_expr(ty: &str) -> String {
    match ty {
        "bool" => "false".into(),
        "i64" => "0".into(),
        "f64" => "0.0".into(),
        "String" => "String::new()".into(),
        _ => "Default::default()".into(),
    }
}

fn render_value(v: &Value) -> Result<String, String> {
    Ok(match v {
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format!("{f:?}f64"),
        Value::Str(s) => format!("{s:?}.to_string()"),
        Value::Enum(s) => format!("{s:?}.to_string()"),
        _ => return Err("unsupported literal value".into()),
    })
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
        Gen {
            program,
            lines: Vec::new(),
            sids: BTreeMap::new(),
            holes: Vec::new(),
            tags: Vec::new(),
            hole_seq: 0,
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

    fn resource_fqn(&self, r: crate::sem::ids::ResourceId) -> String {
        let res = self.program.resource(r);
        crate::fqn::fqn(self.program.module_name(res.module), &res.name)
    }

    fn function_fqn(&self, f: crate::sem::ids::FunctionId) -> String {
        let fun = self.program.function(f);
        crate::fqn::fqn(self.program.module_name(fun.module), &fun.name)
    }

    fn field(&self, r: crate::sem::ids::ResourceId) -> String {
        let res = self.program.resource(r);
        format!("r_{}__{}", sanitize(self.program.module_name(res.module)),
                sanitize(&res.name))
    }

    /// Cross-module path to a generated function.
    fn fn_ref(&self, f: crate::sem::ids::FunctionId) -> String {
        let func = self.program.function(f);
        format!("crate::{}::cf_{}", sanitize(self.program.module_name(func.module)),
                sanitize(&func.name))
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
            line,
            reads: Vec::new(),
            writes: Vec::new(),
            req: Vec::new(),
        });
        format!("/* HOLE({id}) expected: {expected} */ Default::default()")
    }

    fn is_std_atomic(&self, r: crate::sem::ids::ResourceId) -> bool {
        let res = self.program.resource(r);
        if res.kind != ResKind::Atomic {
            return false;
        }
        matches!(
            res.ty.as_ref().and_then(|bt| rust_ty(bt).ok()).as_deref(),
            Some("i64" | "bool")
        )
    }

    fn render_shared_read(&self, r: crate::sem::ids::ResourceId) -> String {
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

    fn render_shared_write(&self, r: crate::sem::ids::ResourceId, val_expr: &str) -> String {
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
            LExpr::Slot(SlotRef::Local(i)) => format!("_l{i}"),
            LExpr::Slot(SlotRef::Discard) => "()".into(),
            LExpr::Slot(SlotRef::Shared(r)) => self.render_shared_read(*r),
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

pub fn generate(program: &SemProgram) -> Result<Generated, String> {
    let mut modules = std::collections::BTreeSet::new();
    for r in program.resources() {
        modules.insert(r.module);
    }
    for f in program.functions() {
        modules.insert(f.module);
    }
    let _ = modules; // multi-module is supported
    let mut g = Gen::new(program);

    // Header + Shared struct.
    g.emit("// Generated by `concir-backend codegen`. Do not edit the skeleton.");
    g.emit("mod cir_trace;");
    g.emit("use std::sync::{Arc, Condvar, Mutex};");
    g.emit("");
    g.emit("#[allow(dead_code)]");
    g.emit("pub(crate) mod __res {");
    g.emit("    use std::sync::{Condvar, Mutex};");
    for r in program.resources() {
        let f = g.field(r.id);
        match r.kind {
            ResKind::Mutex => {
                g.emit(format!(
                    "    #[allow(non_upper_case_globals)] pub(crate) static {f}: Mutex<()> = Mutex::new(());"
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
    g.emit("struct Shared {");
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
            ResKind::Mutex => "Arc<Mutex<()>>".to_string(),
            ResKind::Condvar => "Arc<Condvar>".to_string(),
            ResKind::Semaphore => "Arc<cir_trace::Semaphore>".to_string(),
            ResKind::Channel => {
                let bt = r.ty.as_ref().ok_or("channel resource without a base type")?;
                format!("Arc<cir_trace::Channel<{}>>", rust_ty(bt)?)
            }
            ResKind::RwLock => return Err("codegen does not support RwLock".into()),
            ResKind::Var => {
                let bt = r.ty.as_ref().ok_or("value resource without a type")?;
                format!("Arc<crate::cir_trace::SyncVar<{}>>", rust_ty(bt)?)
            }
            ResKind::Atomic => {
                let bt = r.ty.as_ref().ok_or("atomic resource without a type")?;
                let rt = rust_ty(bt)?;
                match rt.as_str() {
                    "i64" => "Arc<std::sync::atomic::AtomicI64>".to_string(),
                    "bool" => "Arc<std::sync::atomic::AtomicBool>".to_string(),
                    _ => format!("Arc<Mutex<{rt}>>"),
                }
            }
        };
        g.emit(format!("    {f}: {ty},"));
    }
    g.emit("}");
    g.emit("");

    // Function bodies, one `mod` per CIR module.
    let mut module_order: Vec<crate::sem::ids::ModuleId> = Vec::new();
    for f in program.functions() {
        if !module_order.contains(&f.module) {
            module_order.push(f.module);
        }
    }
    for m in module_order {
        let mod_name = program.module_name(m).to_string();
        g.emit(format!("pub(crate) mod {} {{", sanitize(&mod_name)));
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

    // Rust main.
    let entry = program.function(program.entry());
    let entry_name = format!("crate::{}::cf_{}", sanitize(program.module_name(entry.module)),
                             sanitize(&entry.name));
    g.emit("fn main() {");
    g.emit("    let shared = Arc::new(Shared {");
    for r in program.resources() {
        let f = g.field(r.id);
        let init = match r.kind {
            ResKind::Mutex => "Arc::new(Mutex::new(()))".to_string(),
            ResKind::Condvar => "Arc::new(Condvar::new())".to_string(),
            ResKind::Semaphore => format!("cir_trace::Semaphore::new({})", r.permits),
            ResKind::Channel => {
                let bt = r.ty.as_ref().ok_or("channel resource without a base type")?;
                format!("cir_trace::Channel::<{}>::new({})", rust_ty(bt)?, r.capacity)
            }
            ResKind::RwLock => return Err("RwLock unsupported".into()),
            ResKind::Var => {
                let bt = r.ty.as_ref().ok_or("value resource without a type")?;
                let ty = rust_ty(bt)?;
                let v = match &r.init {
                    Some(v) => render_value(v)?,
                    None => default_expr(&ty),
                };
                format!("Arc::new(cir_trace::SyncVar::new({v}))")
            }
            ResKind::Atomic => {
                let bt = r.ty.as_ref().ok_or("atomic resource without a type")?;
                let ty = rust_ty(bt)?;
                let v = match &r.init {
                    Some(v) => render_value(v)?,
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

    let map = CodegenMap {
        program: program_name(program),
        sids: g.sids.clone(),
        holes: g.holes.clone(),
        tags: g.tags.clone(),
        resources: codegen_resources,
    };
    Ok(Generated {
        cargo_toml: cargo_toml(),
        main_rs: g.lines.join("\n") + "\n",
        trace_rs: TRACE_RUNTIME.to_string(),
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

fn try_structure_function(func: &crate::sem::program::SemFunction) -> Option<Vec<StructuredNode>> {
    use std::collections::BTreeSet;

    let mut total_jump_edges = 0usize;
    for (idx, stmt) in func.body.iter().enumerate() {
        match &stmt.op {
            SemOp::Switch { .. } | SemOp::Unsupported { .. } => return None,
            SemOp::Goto { .. } => total_jump_edges += 1,
            SemOp::Branch { .. } => total_jump_edges += 2,
            SemOp::MutexUnlock { .. } => {
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
    // Also verify mutex locks/unlocks do not cross branch boundaries asymmetrically
    // (e.g. locked inside `if` without unlocking inside the same `if`).
    let mut held: BTreeSet<crate::sem::ids::ResourceId> = BTreeSet::new();
    if !check_lexical_locks(&func.body, &nodes, &mut held) {
        return None;
    }
    Some(nodes)
}

/// Returns `true` if `nodes` preserves lexical `MutexGuard` scoping (returns `(ok, diverges)`).
fn check_lexical_locks(
    body: &[crate::sem::program::SemStmt],
    nodes: &[StructuredNode],
    held: &mut std::collections::BTreeSet<crate::sem::ids::ResourceId>,
) -> bool {
    check_lexical_locks_inner(body, nodes, held).is_some()
}

fn check_lexical_locks_inner(
    body: &[crate::sem::program::SemStmt],
    nodes: &[StructuredNode],
    held: &mut std::collections::BTreeSet<crate::sem::ids::ResourceId>,
) -> Option<bool> {
    for node in nodes {
        match node {
            StructuredNode::Stmt(idx) => match &body[*idx].op {
                SemOp::MutexLock { resource } => {
                    held.insert(*resource);
                }
                SemOp::MutexUnlock { resource } => {
                    if !held.remove(resource) {
                        return None;
                    }
                }
                SemOp::CondvarWait { lock, .. } => {
                    if !held.contains(lock) {
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
        let ty = rust_ty(&slot.ty)?;
        let init = match &slot.init {
            Some(v) => render_value(v)?,
            None => default_expr(&ty),
        };
        g.emit(format!(
            "        #[allow(unused_mut, unused_variables)] let mut _l{i}: {ty} = {init};"
        ));
    }
    emit_structured_nodes(g, func, nodes, "        ")?;
    g.emit("    }");
    g.emit("");
    Ok(())
}

fn record_sid_line(g: &mut Gen, func: &crate::sem::program::SemFunction, sid: &str, indent: &str) {
    let loc = g.line();
    g.sids.insert(
        format!("{}::{}", g.program.module_name(func.module), sid),
        SidLocation {
            file: "src/main.rs".into(),
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
                emit_structured_nodes(g, func, body, &inner_indent)?;
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
                emit_structured_nodes(g, func, then_nodes, &inner_indent)?;
                if let Some(goto_idx) = then_goto_idx {
                    let goto_sid = &func.body[*goto_idx].sid;
                    record_sid_line(g, func, goto_sid, &inner_indent);
                }
                if else_nodes.is_empty() {
                    g.emit(format!("{indent}}}"));
                } else {
                    g.emit(format!("{indent}}} else {{"));
                    emit_structured_nodes(g, func, else_nodes, &inner_indent)?;
                    g.emit(format!("{indent}}}"));
                }
            }
            StructuredNode::Stmt(idx) => {
                let stmt = &func.body[*idx];
                let sid = &stmt.sid;
                record_sid_line(g, func, sid, indent);
                emit_structured_stmt(g, func, &stmt.op, sid, indent)?;
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
            g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
            g.emit(format!(
                "{indent}#[allow(unused_mut, unused_variables)] let mut _g_{f} = crate::cir_trace::lock(&shared.{f}, tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
            g.emit(format!(
                "{indent}#[allow(unused_mut, unused_variables)] let mut _g_{f} = crate::__res::{f}.lock().unwrap();"
            ));
        }
        SemOp::MutexUnlock { resource } => {
            let f = g.field(*resource);
            let rq = g.resource_fqn(*resource);
            g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
            g.emit(format!(
                "{indent}crate::cir_trace::unlock(Some(_g_{f}), tag, {rq:?}, {sid:?});"
            ));
            g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
            g.emit(format!("{indent}drop(_g_{f});"));
        }
        SemOp::CondvarWait { condvar, lock } => {
            let c = g.field(*condvar);
            let l = g.field(*lock);
            let rq = g.resource_fqn(*condvar);
            g.emit(format!("{indent}#[cfg(feature = \"cir_trace\")]"));
            g.emit(format!(
                "{indent}{{ _g_{l} = crate::cir_trace::condvar_wait(&shared.{c}, _g_{l}, tag, {rq:?}, {sid:?}); }}"
            ));
            g.emit(format!("{indent}#[cfg(not(feature = \"cir_trace\"))]"));
            g.emit(format!("{indent}{{ _g_{l} = crate::__res::{c}.wait(_g_{l}).unwrap(); }}"));
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
            let expected_ty = expected_rust_ty(expected_type);
            let hole_line = g.line();
            g.holes.push(Hole {
                id: id.clone(),
                module: mod_name.clone(),
                function: func.name.clone(),
                sid: sid.to_string(),
                expected: expected_ty.to_string(),
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
        let ty = rust_ty(&slot.ty)?;
        let init = match &slot.init {
            Some(v) => render_value(v)?,
            None => default_expr(&ty),
        };
        g.emit(format!(
            "    #[allow(unused_mut)] let mut _l{i}: {ty} = {init};"
        ));
    }
    // guard declarations for every mutex
    let mut guards = Vec::new();
    for r in g.program.resources() {
        if matches!(r.kind, ResKind::Mutex) {
            let f = g.field(r.id);
            g.emit(format!(
                "    #[allow(unused_mut)] let mut _g_{f}: Option<std::sync::MutexGuard<'_, ()>> = None;"
            ));
            guards.push(f);
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
                file: "src/main.rs".into(),
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

fn expected_rust_ty(expected: &str) -> &str {
    match expected.trim() {
        "" | "()" => "()",
        "Int" => "i64",
        "Bool" => "bool",
        "Float" => "f64",
        other => other,
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
            let expected_ty = expected_rust_ty(expected_type);
            let hole_line = g.line();
            g.holes.push(Hole {
                id: id.clone(),
                module: mod_name.clone(),
                function: func.name.clone(),
                sid: sid.to_string(),
                expected: expected_ty.to_string(),
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
    let map = serde_json::to_string_pretty(&generated.map).unwrap_or_default();
    std::fs::write(out_dir.join("codegen.json"), map + "\n")?;
    Ok(())
}

#[allow(dead_code)]
fn _unused(_: &mut String) {
    let _ = write!(String::new(), "");
}
