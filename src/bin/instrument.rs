//! `concir-instrument`: annotate a std-only single-file Rust program with
//! `cir_trace::ev` calls at every concurrency operation and emit the label map.
//!
//! The LLM never edits Rust in extraction mode: this tool inserts an event as a
//! statement immediately before the statement containing each `.lock()`,
//! `.wait(`, `.wait_while(`, `.notify_one/all(`, `.send(`, `.recv()`, `.join()`,
//! `acquire`/`release`, and `thread::spawn` / `thread::scope` / `s.spawn`.
//! Labels `L<n>` are numbered by source position. Each thread gets a tag (`t0`
//! for main, `t1..` per spawn in source order) managed by a `thread_local!` in
//! the emitted `cir_trace` runtime.
//!
//! Outputs in `--out <dir>`:
//!   annotated.rs   instrumented source (with `mod cir_trace;` and `finish()`)
//!   labels.json    [{label, line, op, receiver, thread}]
//!   cir_trace.rs   the runtime the instrumented source builds against

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::process;

use proc_macro2::Span;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Expr, ExprCall, ExprClosure, ExprMethodCall, File, Stmt};

const OPS: &[(&str, &str)] = &[
    ("lock", "mutex_lock"),
    ("wait", "condvar_wait"),
    ("wait_while", "condvar_wait"),
    ("notify_one", "condvar_notify"),
    ("notify_all", "condvar_notify"),
    ("send", "channel_send"),
    ("recv", "channel_recv"),
    ("join", "join"),
    ("acquire", "sem_acquire"),
    ("release", "sem_release"),
];

fn op_for(method: &str) -> Option<&'static str> {
    OPS.iter().find(|(m, _)| *m == method).map(|(_, op)| *op)
}

fn line_starts(src: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

fn leading_header_len(src: &str) -> usize {
    // Inner doc comments (`//!`) and inner attributes (`#![...]`) must stay at
    // the very top of the file, so `mod cir_trace;` is inserted after them.
    let mut offset = 0;
    for line in src.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//!") || trimmed.starts_with("#![") || trimmed.trim().is_empty()
        {
            offset += line.len();
        } else {
            break;
        }
    }
    offset
}

fn lc_offset(starts: &[usize], lc: proc_macro2::LineColumn) -> usize {
    let line = lc.line.saturating_sub(1).min(starts.len().saturating_sub(1));
    starts[line] + lc.column
}

#[derive(Debug, Clone)]
struct Site {
    orig: usize,
    pos: usize,
    op: String,
    receiver: String,
}

#[derive(Debug, Clone)]
struct SpawnSite {
    event_site: usize,
    tag: String,
    body_insert: usize,
    body_end: usize,
    is_block: bool,
}

struct Collector {
    starts: Vec<usize>,
    src: String,
    sites: Vec<Site>,
    spawns: Vec<SpawnSite>,
    spawn_count: usize,
    stmt_stack: Vec<usize>,
    main_close: Option<usize>,
}

impl Collector {
    fn receiver_text(&self, span: Span) -> String {
        let a = lc_offset(&self.starts, span.start());
        let b = lc_offset(&self.starts, span.end());
        self.src
            .get(a..b)
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn record(&mut self, expr_span: Span, op: &str) -> usize {
        let fallback = lc_offset(&self.starts, expr_span.start());
        let pos = self.stmt_stack.last().copied().unwrap_or(fallback);
        let orig = self.sites.len();
        self.sites.push(Site {
            orig,
            pos,
            op: op.to_string(),
            receiver: self.receiver_text(expr_span),
        });
        orig
    }

    fn tag_closure(&mut self, closure: &ExprClosure, event_site: usize) {
        let (body_insert, body_end, is_block) = match &*closure.body {
            Expr::Block(block) => {
                let open =
                    lc_offset(&self.starts, block.block.brace_token.span.open().start());
                let close =
                    lc_offset(&self.starts, block.block.brace_token.span.close().start());
                (open + 1, close, true)
            }
            other => {
                let a = lc_offset(&self.starts, other.span().start());
                let b = lc_offset(&self.starts, other.span().end());
                (a, b, false)
            }
        };
        self.spawns.push(SpawnSite {
            event_site,
            tag: String::new(),
            body_insert,
            body_end,
            is_block,
        });
    }
}

impl<'ast> Visit<'ast> for Collector {
    fn visit_stmt(&mut self, node: &'ast Stmt) {
        let start = lc_offset(&self.starts, node.span().start());
        self.stmt_stack.push(start);
        syn::visit::visit_stmt(self, node);
        self.stmt_stack.pop();
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if node.sig.ident == "main" {
            self.main_close = Some(lc_offset(
                &self.starts,
                node.block.brace_token.span.close().start(),
            ));
        }
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        let method = node.method.to_string();
        if let Some(op) = op_for(&method) {
            self.record(node.receiver.span(), op);
        } else if method == "spawn" {
            let site = self.record(node.span(), "spawn");
            if let Some(Expr::Closure(c)) = node.args.first() {
                self.tag_closure(c, site);
            }
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        let last = match &*node.func {
            Expr::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
            _ => None,
        };
        match last.as_deref() {
            Some("spawn") => {
                let site = self.record(node.span(), "spawn");
                if let Some(Expr::Closure(c)) = node.args.first() {
                    self.tag_closure(c, site);
                }
            }
            Some("scope") => {
                self.record(node.span(), "scope");
            }
            _ => {}
        }
        syn::visit::visit_expr_call(self, node);
    }
}

const RUNTIME: &str = r#"// Generated cir_trace runtime (std only) with thread tags.
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

thread_local! {
    static TAG: std::cell::RefCell<String> = std::cell::RefCell::new("t0".to_string());
}

pub fn tag_str() -> String {
    TAG.with(|t| t.borrow().clone())
}

pub fn set_tag(tag: &str) {
    TAG.with(|t| *t.borrow_mut() = tag.to_string());
}

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

#[allow(dead_code)]
pub struct Semaphore {
    count: Mutex<i64>,
    cv: Condvar,
}

#[allow(dead_code)]
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

#[allow(dead_code)]
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
        b.push_back(v);
        self.recv_cv.notify_one();
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

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut input: Option<String> = None;
    let mut out_dir: Option<String> = None;
    let mut wrappers = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                out_dir = args.get(i + 1).cloned();
                i += 2;
            }
            "--wrappers" => {
                wrappers = true;
                i += 1;
            }
            "--help" | "-h" => {
                eprintln!(
                    "usage: concir-instrument <input.rs> --out <dir> [--wrappers]"
                );
                process::exit(0);
            }
            other => {
                input = Some(other.to_string());
                i += 1;
            }
        }
    }
    let (Some(input), Some(out_dir)) = (input, out_dir) else {
        eprintln!("usage: concir-instrument <input.rs> --out <dir>");
        process::exit(2);
    };
    let src = fs::read_to_string(&input).unwrap_or_else(|e| {
        eprintln!("error reading '{input}': {e}");
        process::exit(2);
    });
    if wrappers {
        let out_dir = out_dir.as_str();
        match concir::instrument::wrap(&src) {
            Ok(w) => {
                let out = std::path::Path::new(out_dir);
                fs::create_dir_all(out).unwrap_or_else(|e| {
                    eprintln!("cannot create '{out_dir}': {e}");
                    process::exit(2);
                });
                fs::write(out.join("annotated.rs"), &w.annotated).expect("write annotated.rs");
                fs::write(out.join("cir_trace.rs"), &w.runtime).expect("write cir_trace.rs");
                let resources = serde_json::json!({
                    "schema_version": "cir-resources-v1",
                    "source": input,
                    "resources": w.resources,
                    "limitations": w.limitations,
                    "harness_notes": w.harness_notes,
                });
                fs::write(
                    out.join("resources.json"),
                    serde_json::to_string_pretty(&resources).expect("serialize") + "\n",
                )
                .expect("write resources.json");
                println!(
                    "{}",
                    serde_json::json!({
                        "mode": "wrappers",
                        "annotated": out.join("annotated.rs").display().to_string(),
                        "resources": resources["resources"].as_array().map(|a| a.len()).unwrap_or(0),
                        "limitations": resources["limitations"],
                    })
                );
            }
            Err(e) => {
                eprintln!("instrument v2 failed: {e}");
                process::exit(2);
            }
        }
        return;
    }
    let file: File = syn::parse_file(&src).unwrap_or_else(|e| {
        eprintln!("parse error: {e}");
        process::exit(2);
    });
    let mut collector = Collector {
        starts: line_starts(&src),
        src: src.clone(),
        sites: Vec::new(),
        spawns: Vec::new(),
        spawn_count: 0,
        stmt_stack: Vec::new(),
        main_close: None,
    };
    collector.visit_file(&file);

    // Number labels by source position, keeping visit order within one statement.
    let mut sites = collector.sites.clone();
    sites.sort_by_key(|s| s.pos);

    let mut inserts: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut labels = Vec::new();
    let mut orig_label: std::collections::HashMap<usize, String> =
        std::collections::HashMap::new();
    for (n, site) in sites.iter().enumerate() {
        let label = format!("L{}", n + 1);
        orig_label.insert(site.orig, label.clone());
        let thread = collector
            .spawns
            .iter()
            .filter(|s| s.body_insert <= site.pos && site.pos < s.body_end)
            .min_by_key(|s| s.body_end.saturating_sub(s.body_insert))
            .map(|s| s.tag.clone())
            .unwrap_or_else(|| "t0".to_string());
        inserts.entry(site.pos).or_default().push(format!(
            "cir_trace::ev(&cir_trace::tag_str(), \"{label}\"); "
        ));
        labels.push((
            label,
            site.op.clone(),
            site.receiver.clone(),
            thread,
            site.pos,
        ));
    }

    for spawn in &mut collector.spawns {
        spawn.tag = orig_label
            .get(&spawn.event_site)
            .map(|l| format!("t{l}"))
            .unwrap_or_else(|| "t0".to_string());
        let statement = format!("cir_trace::set_tag(\"{}\"); ", spawn.tag);
        if spawn.is_block {
            inserts.entry(spawn.body_insert).or_default().push(statement);
        } else {
            inserts
                .entry(spawn.body_insert)
                .or_default()
                .push(format!("{{ {statement}"));
            inserts.entry(spawn.body_end).or_default().push("}".to_string());
        }
    }

    // Thread attribution needs the final spawn tags (derived from labels).
    for item in labels.iter_mut() {
        let pos = item.4;
        item.3 = collector
            .spawns
            .iter()
            .filter(|s| s.body_insert <= pos && pos < s.body_end)
            .min_by_key(|s| s.body_end.saturating_sub(s.body_insert))
            .map(|s| s.tag.clone())
            .unwrap_or_else(|| "t0".to_string());
    }

    if let Some(close) = collector.main_close {
        inserts
            .entry(close)
            .or_default()
            .push("cir_trace::finish(); ".to_string());
    }

    // Prepend `mod cir_trace;` if absent, shifting every insertion point.
    let mut annotated = src.clone();
    let base = if src.contains("mod cir_trace") {
        0
    } else {
        let at = leading_header_len(&src);
        let decl = "mod cir_trace;\n";
        annotated = format!("{}{}{}", &src[..at], decl, &src[at..]);
        decl.len()
    };
    let starts = collector.starts.clone();
    let mut points: Vec<(usize, Vec<String>)> = inserts.into_iter().collect();
    points.sort_by(|a, b| b.0.cmp(&a.0));
    for (point, texts) in points {
        let at = point + base;
        if at <= annotated.len() {
            annotated.insert_str(at, &texts.join(""));
        }
    }

    let out = std::path::Path::new(&out_dir);
    fs::create_dir_all(out).unwrap_or_else(|e| {
        eprintln!("cannot create '{out_dir}': {e}");
        process::exit(2);
    });
    fs::write(out.join("annotated.rs"), &annotated).expect("write annotated.rs");
    fs::write(out.join("cir_trace.rs"), RUNTIME).expect("write cir_trace.rs");

    let label_json: Vec<serde_json::Value> = labels
        .iter()
        .map(|(label, op, receiver, thread, at)| {
            let line = starts
                .iter()
                .rposition(|start| *start <= *at)
                .map(|i| i + 1)
                .unwrap_or(1);
            serde_json::json!({
                "label": label,
                "line": line,
                "op": op,
                "receiver": receiver,
                "thread": thread,
            })
        })
        .collect();
    let payload = serde_json::json!({
        "schema_version": "cir-labels-v1",
        "source": input,
        "labels": label_json,
    });
    fs::write(
        out.join("labels.json"),
        serde_json::to_string_pretty(&payload).expect("serialize") + "\n",
    )
    .expect("write labels.json");

    println!(
        "{}",
        serde_json::json!({
            "annotated": out.join("annotated.rs").display().to_string(),
            "labels": out.join("labels.json").display().to_string(),
            "events": labels.len(),
            "spawns": collector.spawns.len(),
        })
    );
}
