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

use serde::Serialize;

use crate::ast::{BaseType, ComplexBaseType};
use crate::expr::{BinOp, CmpOp, Lit};
use crate::sem::eval::LExpr;
use crate::sem::ids::SlotRef;
use crate::sem::program::{ResKind, SemOp, SemProgram};
use crate::sem::value::Value;

const TRACE_RUNTIME: &str = r#"// Generated cir_trace runtime (std only).
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

static EVENTS: OnceLock<Mutex<Vec<(String, String)>>> = OnceLock::new();

pub fn ev(tag: &str, sid: &str) {
    let m = EVENTS.get_or_init(|| Mutex::new(Vec::new()));
    m.lock().unwrap().push((tag.to_string(), sid.to_string()));
}

pub fn finish() {
    if let Ok(path) = std::env::var("CIR_TRACE_OUT") {
        let m = EVENTS.get_or_init(|| Mutex::new(Vec::new()));
        let guard = m.lock().unwrap();
        let mut out = String::new();
        for (t, s) in guard.iter() {
            out.push_str(&format!("{{\"t\":\"{}\",\"sid\":\"{}\"}}\n", t, s));
        }
        let _ = std::fs::write(path, out);
    }
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
}
"#;

#[derive(Debug, Clone, Serialize)]
pub struct Hole {
    pub id: String,
    pub function: String,
    pub sid: String,
    pub expected: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TagRecord {
    pub sid: String,
    pub function: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodegenMap {
    pub program: String,
    pub sids: BTreeMap<String, SidLocation>,
    pub holes: Vec<Hole>,
    pub tags: Vec<TagRecord>,
}

#[derive(Debug, Clone, Serialize)]
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

    fn field(&self, r: crate::sem::ids::ResourceId) -> String {
        format!("r_{}", sanitize(&self.program.resource(r).name))
    }

    #[allow(unused)]
    fn tag_expr(&self, tag: &str) -> String {
        format!("{tag:?}")
    }

    fn hole(&mut self, function: &str, sid: &str, expected: &str) -> String {
        self.hole_seq += 1;
        let id = format!("h{}", self.hole_seq);
        self.holes.push(Hole {
            id: id.clone(),
            function: function.to_string(),
            sid: sid.to_string(),
            expected: expected.to_string(),
        });
        format!("/* HOLE({id}) expected: {expected} */ Default::default()")
    }

    fn render_expr(&mut self, function: &str, sid: &str, e: &LExpr) -> String {
        match e {
            LExpr::Lit(l) => match render_lit(l) {
                Ok(s) => s,
                Err(_) => self.hole(function, sid, "literal"),
            },
            LExpr::Slot(SlotRef::Local(i)) => format!("_l{i}"),
            LExpr::Slot(SlotRef::Discard) => "()".into(),
            LExpr::Slot(SlotRef::Shared(r)) => {
                let f = self.field(*r);
                format!("shared.{f}.lock().unwrap().clone()")
            }
            LExpr::Neg(inner) => format!("(-{})", self.render_expr(function, sid, inner)),
            LExpr::Bin { op, lhs, rhs } => {
                let a = self.render_expr(function, sid, lhs);
                let b = self.render_expr(function, sid, rhs);
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
                let a = self.render_expr(function, sid, lhs);
                let b = self.render_expr(function, sid, rhs);
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
            _ => self.hole(function, sid, "expression"),
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
    if modules.len() != 1 {
        return Err(format!(
            "codegen supports a single module, found {}",
            modules.len()
        ));
    }
    let mut g = Gen::new(program);

    // Header + Shared struct.
    g.emit("// Generated by `concir-backend codegen`. Do not edit the skeleton.");
    g.emit("mod cir_trace;");
    g.emit("use std::sync::{Arc, Condvar, Mutex};");
    g.emit("");
    g.emit("#[derive(Clone)]");
    g.emit("struct Shared {");
    for r in program.resources() {
        let f = g.field(r.id);
        let ty = match r.kind {
            ResKind::Mutex => "Arc<Mutex<()>>".to_string(),
            ResKind::Condvar => "Arc<Condvar>".to_string(),
            ResKind::Semaphore => "Arc<cir_trace::Semaphore>".to_string(),
            ResKind::Channel => {
                return Err(format!("codegen does not support channel '{}' yet", r.name));
            }
            ResKind::RwLock => return Err("codegen does not support RwLock".into()),
            ResKind::Var | ResKind::Atomic => {
                let bt = r.ty.as_ref().ok_or("value resource without a type")?;
                format!("Arc<Mutex<{}>>", rust_ty(bt)?)
            }
        };
        g.emit(format!("    {f}: {ty},"));
    }
    g.emit("}");
    g.emit("");

    // Function bodies.
    for func in program.functions() {
        if func.is_nobody {
            let name = format!("cf_{}", sanitize(&func.name));
            g.emit(format!(
                "fn {name}(_shared: Shared, _tag: &str) {{ /* HOLE(nobody) */ }}"
            ));
            g.emit("");
            continue;
        }
        generate_function(&mut g, func)?;
    }

    // Rust main.
    let entry = program.function(program.entry());
    let entry_name = format!("cf_{}", sanitize(&entry.name));
    g.emit("fn main() {");
    g.emit("    let shared = Shared {");
    for r in program.resources() {
        let f = g.field(r.id);
        let init = match r.kind {
            ResKind::Mutex => "Arc::new(Mutex::new(()))".to_string(),
            ResKind::Condvar => "Arc::new(Condvar::new())".to_string(),
            ResKind::Semaphore => format!("cir_trace::Semaphore::new({})", r.permits),
            ResKind::Channel => return Err("channel unsupported".into()),
            ResKind::RwLock => return Err("RwLock unsupported".into()),
            ResKind::Var | ResKind::Atomic => {
                let bt = r.ty.as_ref().ok_or("value resource without a type")?;
                let ty = rust_ty(bt)?;
                let v = match &r.init {
                    Some(v) => render_value(v)?,
                    None => default_expr(&ty),
                };
                format!("Arc::new(Mutex::new({v}))")
            }
        };
        g.emit(format!("        {f}: {init},"));
    }
    g.emit("    };");
    g.emit(format!("    {entry_name}(shared, \"t0\");"));
    g.emit("    cir_trace::finish();");
    g.emit("}");

    let map = CodegenMap {
        program: program_name(program),
        sids: g.sids.clone(),
        holes: g.holes.clone(),
        tags: g.tags.clone(),
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
    "[package]\nname = \"cir_generated\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n".to_string()
}

fn generate_function(g: &mut Gen, func: &crate::sem::program::SemFunction) -> Result<(), String> {
    let name = format!("cf_{}", sanitize(&func.name));
    g.emit(format!("fn {name}(shared: Shared, tag: &str) {{"));
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
    let ev = format!("                cir_trace::ev(tag, {sid:?});");
    if is_observable(op) && event_at_attempt(op) {
        g.emit(&ev);
    }
    match op {
        SemOp::Nop => {
            g.emit(format!("                pc = {next};"));
        }
        SemOp::AssignLocal { target, expr } => {
            let e = g.render_expr(&func.name, sid, expr);
            match target {
                SlotRef::Local(i) => {
                    g.emit(format!("                _l{i} = {e};"));
                }
                SlotRef::Discard => {}
                SlotRef::Shared(r) => {
                    let f = g.field(*r);
                    g.emit(format!(
                        "                *shared.{f}.lock().unwrap() = {e};"
                    ));
                }
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::ReadShared { resource, dst } => {
            let f = g.field(*resource);
            if let Some(SlotRef::Local(i)) = dst {
                g.emit(format!(
                    "                _l{i} = shared.{f}.lock().unwrap().clone();"
                ));
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::WriteShared { resource, expr } => {
            let f = g.field(*resource);
            let e = g.render_expr(&func.name, sid, expr);
            g.emit(format!(
                "                {{ let __v = {e}; *shared.{f}.lock().unwrap() = __v; }}"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::MutexLock { resource } => {
            let f = g.field(*resource);
            g.emit(format!(
                "                _g_{f} = Some(shared.{f}.lock().unwrap());"
            ));
            g.emit(&ev);
            g.emit(format!("                pc = {next};"));
        }
        SemOp::MutexUnlock { resource } => {
            let f = g.field(*resource);
            g.emit(format!("                drop(_g_{f}.take());"));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::CondvarWait { condvar, lock } => {
            let c = g.field(*condvar);
            let l = g.field(*lock);
            g.emit(format!(
                "                _g_{l} = Some(shared.{c}.wait(_g_{l}.take().expect(\"wait without lock\")).unwrap());"
            ));
            g.emit(&ev);
            g.emit(format!("                pc = {next};"));
        }
        SemOp::CondvarNotify { condvar } => {
            let c = g.field(*condvar);
            g.emit(format!("                shared.{c}.notify_one();"));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::CondvarNotifyAll { condvar } => {
            let c = g.field(*condvar);
            g.emit(format!("                shared.{c}.notify_all();"));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::SemaphoreAcquire { resource, count } => {
            let f = g.field(*resource);
            g.emit(format!("                shared.{f}.acquire({count});"));
            g.emit(&ev);
            g.emit(format!("                pc = {next};"));
        }
        SemOp::SemaphoreRelease { resource, count } => {
            let f = g.field(*resource);
            g.emit(format!("                shared.{f}.release({count});"));
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
            let c = g.render_expr(&func.name, sid, cond);
            g.emit(format!(
                "                pc = if {c} {{ {then} }} else {{ {else_target} }};"
            ));
        }
        SemOp::Switch {
            var,
            cases,
            default,
        } => {
            let v = g.render_expr(&func.name, sid, var);
            g.emit(format!("                pc = match {v} {{"));
            for (label, target) in cases {
                g.emit(format!("                    {label:?} => {target},"));
            }
            g.emit(format!("                    _ => {default},"));
            g.emit("                };");
        }
        SemOp::Return { value } => {
            if let Some(e) = value {
                let _ = g.render_expr(&func.name, sid, e);
            }
            g.emit("                return;");
        }
        SemOp::Scope { funcs } => {
            let tags = child_tags(func, sid);
            g.tags.push(TagRecord {
                sid: sid.to_string(),
                function: func.name.clone(),
                tags: tags.clone(),
            });
            g.emit("                {");
            g.emit("                    let mut __hs = Vec::new();");
            for (i, f) in funcs.iter().enumerate() {
                let cf = format!("cf_{}", sanitize(&g.program.function(*f).name));
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
            let cf = format!("cf_{}", sanitize(&g.program.function(*child).name));
            let t = tags.first().cloned().unwrap_or_else(|| format!("t{sid}"));
            let h = format!("_h_{}", sanitize(handle));
            g.emit("                {");
            g.emit("                    let __sh = shared.clone();");
            g.emit(format!(
                "                    let {h} = std::thread::spawn(move || {cf}(__sh, {t:?}));"
            ));
            g.emit("                }");
            g.emit(format!("                pc = {next};"));
        }
        SemOp::Join { handle } => {
            let h = format!("_h_{}", sanitize(handle));
            g.emit(format!("                {h}.join().unwrap();"));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::Call {
            func: callee,
            args,
            dst,
        } => {
            let cf = format!("cf_{}", sanitize(&g.program.function(*callee).name));
            for a in args {
                let _ = g.render_expr(&func.name, sid, a);
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
            let f = g.field(*resource);
            if let SlotRef::Local(i) = dst {
                g.emit(format!(
                    "                _l{i} = shared.{f}.lock().unwrap().clone();"
                ));
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::AtomicStore { resource, value } => {
            let f = g.field(*resource);
            let e = g.render_expr(&func.name, sid, value);
            g.emit(format!(
                "                {{ let __v = {e}; *shared.{f}.lock().unwrap() = __v; }}"
            ));
            g.emit(format!("                pc = {next};"));
        }
        SemOp::AtomicCas {
            resource,
            expected,
            desired,
            dst,
        } => {
            let f = g.field(*resource);
            let ex = g.render_expr(&func.name, sid, expected);
            let de = g.render_expr(&func.name, sid, desired);
            if let SlotRef::Local(i) = dst {
                g.emit(format!(
                    "                {{ let mut __g = shared.{f}.lock().unwrap(); let __ok = *__g == {ex}; if __ok {{ *__g = {de}; }} drop(__g); _l{i} = __ok; }}"
                ));
            } else {
                g.emit(format!(
                    "                {{ let mut __g = shared.{f}.lock().unwrap(); if *__g == {ex} {{ *__g = {de}; }} }}"
                ));
            }
            g.emit(format!("                pc = {next};"));
        }
        SemOp::ChannelSend { channel, value } => {
            let f = g.field(*channel);
            let e = g.render_expr(&func.name, sid, value);
            g.emit(format!("                shared.{f}.send({e});"));
            g.emit(&ev);
            g.emit(format!("                pc = {next};"));
        }
        SemOp::ChannelRecv { channel, dst } => {
            let f = g.field(*channel);
            if let SlotRef::Local(i) = dst {
                g.emit(format!("                _l{i} = shared.{f}.recv();"));
            } else {
                g.emit(format!("                let _ = shared.{f}.recv();"));
            }
            g.emit(&ev);
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
