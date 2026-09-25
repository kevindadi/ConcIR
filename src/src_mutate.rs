//! Source-level mutations for the conformance evaluation.
//!
//! Transforms are syntactic and scope-aware. They do not decide whether a
//! change violates a CIR or a requirement; the Python harness labels that
//! independently. A transform that cannot be applied returns `applicable: false`
//! instead of a broken string substitution.

use syn::spanned::Spanned;
use syn::{Expr, File, Ident, Local, Stmt};

#[derive(Debug, Clone)]
pub struct MutateResult {
    pub applicable: bool,
    pub source: String,
    pub reason: String,
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

fn offset(starts: &[usize], lc: proc_macro2::LineColumn) -> usize {
    let line = lc.line.saturating_sub(1).min(starts.len().saturating_sub(1));
    starts[line] + lc.column
}

fn span_range(src: &str, span: proc_macro2::Span) -> Option<(usize, usize)> {
    let starts = line_starts(src);
    let a = offset(&starts, span.start());
    let b = offset(&starts, span.end());
    if a < b && b <= src.len() {
        Some((a, b))
    } else {
        None
    }
}

struct IdentHits {
    from: String,
    spans: Vec<proc_macro2::Span>,
}

impl<'ast> syn::visit::Visit<'ast> for IdentHits {
    fn visit_expr_field(&mut self, node: &'ast syn::ExprField) {
        self.visit_expr(&node.base);
    }

    fn visit_field(&mut self, node: &'ast syn::Field) {
        self.visit_type(&node.ty);
    }

    fn visit_ident(&mut self, ident: &'ast Ident) {
        if ident == &self.from {
            self.spans.push(ident.span());
        }
    }
}

fn locals_of(file: &File) -> Vec<&Local> {
    let mut out = Vec::new();
    fn walk<'a>(stmt: &'a Stmt, out: &mut Vec<&'a Local>) {
        if let Stmt::Local(local) = stmt {
            out.push(local);
            if let Some(init) = &local.init {
                if let Expr::Block(block) = &*init.expr {
                    for child in &block.block.stmts {
                        walk(child, out);
                    }
                }
            }
        }
        if let Stmt::Expr(Expr::Block(block), _) = stmt {
            for child in &block.block.stmts {
                walk(child, out);
            }
        }
    }
    for item in &file.items {
        if let syn::Item::Fn(func) = item {
            for stmt in &func.block.stmts {
                walk(stmt, &mut out);
            }
        }
    }
    out
}

/// Rename the nth plain local and its identifier uses. Field names are left
/// alone. The new name is `<old>_kept`, applied once, so a second pass cannot
/// turn `tmp2` into `tmp2_renamed_renamed`.
pub fn rename_local(src: &str, index: usize) -> MutateResult {
    let file: File = match syn::parse_file(src) {
        Ok(file) => file,
        Err(err) => {
            return MutateResult {
                applicable: false,
                source: src.to_string(),
                reason: format!("parse: {err}"),
            };
        }
    };
    let locals = locals_of(&file);
    let Some(local) = locals.get(index) else {
        return MutateResult {
            applicable: false,
            source: src.to_string(),
            reason: "no local at index".into(),
        };
    };
    let syn::Pat::Ident(pat) = &local.pat else {
        return MutateResult {
            applicable: false,
            source: src.to_string(),
            reason: "local pattern is not a plain identifier".into(),
        };
    };
    let from = pat.ident.to_string();
    if from.starts_with('_') {
        return MutateResult {
            applicable: false,
            source: src.to_string(),
            reason: "skip unused binding".into(),
        };
    }
    let to = format!("{from}_kept");
    let mut hits = IdentHits { from: from.clone(), spans: Vec::new() };
    syn::visit::visit_file(&mut hits, &file);
    let mut ranges = Vec::new();
    for span in hits.spans {
        if let Some(range) = span_range(src, span) {
            if &src[range.0..range.1] == from {
                ranges.push(range);
            }
        }
    }
    ranges.sort_by(|a, b| b.0.cmp(&a.0));
    ranges.dedup();
    if ranges.is_empty() {
        return MutateResult { applicable: false, source: src.into(), reason: "ident spans missing".into() };
    }
    let mut out = src.to_string();
    for (a, b) in ranges {
        out.replace_range(a..b, &to);
    }
    if out.contains(&format!("{to}_kept")) {
        return MutateResult { applicable: false, source: src.into(), reason: "rename double-applied".into() };
    }
    MutateResult {
        applicable: true,
        source: out,
        reason: "renamed one local binding and its identifier uses".into(),
    }
}

fn is_lock_call(expr: &Expr) -> bool {
    let Expr::MethodCall(call) = expr else { return false };
    call.method == "lock" || (call.method == "unwrap" && is_lock_call(&call.receiver))
}

fn is_notify_stmt(stmt: &Stmt) -> bool {
    let Stmt::Expr(expr, _) = stmt else { return false };
    let Expr::MethodCall(call) = expr else { return false };
    call.method == "notify_one" || call.method == "notify_all"
}

/// Swap the nth pair of adjacent `lock` locals inside one block.
pub fn swap_adjacent_locks(src: &str, index: usize) -> MutateResult {
    let Ok(file) = syn::parse_file(src) else {
        return MutateResult { applicable: false, source: src.into(), reason: "parse".into() };
    };
    let mut pairs = Vec::new();
    fn walk(stmts: &[Stmt], src: &str, pairs: &mut Vec<(usize, usize, usize, usize)>) {
        for window in stmts.windows(2) {
            let (Stmt::Local(a), Stmt::Local(b)) = (&window[0], &window[1]) else { continue };
            let (Some(ia), Some(ib)) = (&a.init, &b.init) else { continue };
            if is_lock_call(&ia.expr) && is_lock_call(&ib.expr) {
                if let (Some((a0, a1)), Some((b0, b1))) = (
                    span_range(src, window[0].span()),
                    span_range(src, window[1].span()),
                ) {
                    if a1 <= b0 {
                        pairs.push((a0, a1, b0, b1));
                    }
                }
            }
        }
        for stmt in stmts {
            match stmt {
                Stmt::Local(local) => {
                    if let Some(init) = &local.init {
                        if let Expr::Block(block) = &*init.expr {
                            walk(&block.block.stmts, src, pairs);
                        }
                        if let Expr::Closure(closure) = &*init.expr {
                            if let Expr::Block(block) = &*closure.body {
                                walk(&block.block.stmts, src, pairs);
                            }
                        }
                    }
                }
                Stmt::Expr(Expr::Block(block), _) => walk(&block.block.stmts, src, pairs),
                Stmt::Expr(Expr::Closure(closure), _) => {
                    if let Expr::Block(block) = &*closure.body {
                        walk(&block.block.stmts, src, pairs);
                    }
                }
                _ => {}
            }
        }
    }
    for item in &file.items {
        if let syn::Item::Fn(func) = item {
            walk(&func.block.stmts, src, &mut pairs);
        }
    }
    let Some(&(a0, a1, b0, b1)) = pairs.get(index) else {
        return MutateResult { applicable: false, source: src.into(), reason: "no adjacent lock pair".into() };
    };
    let mut out = String::new();
    out.push_str(&src[..a0]);
    out.push_str(&src[b0..b1]);
    out.push_str(&src[a1..b0]);
    out.push_str(&src[a0..a1]);
    out.push_str(&src[b1..]);
    MutateResult { applicable: true, source: out, reason: "swapped adjacent lock statements".into() }
}

pub fn omit_notify_stmt(src: &str, index: usize) -> MutateResult {
    let Ok(file) = syn::parse_file(src) else {
        return MutateResult { applicable: false, source: src.into(), reason: "parse".into() };
    };
    let mut sites = Vec::new();
    fn walk(stmts: &[Stmt], src: &str, sites: &mut Vec<(usize, usize)>) {
        for stmt in stmts {
            if is_notify_stmt(stmt) {
                if let Some(range) = span_range(src, stmt.span()) {
                    sites.push(range);
                }
            }
            match stmt {
                Stmt::Expr(Expr::Block(block), _) => walk(&block.block.stmts, src, sites),
                Stmt::Expr(Expr::Closure(closure), _) => {
                    if let Expr::Block(block) = &*closure.body {
                        walk(&block.block.stmts, src, sites);
                    }
                }
                Stmt::Local(local) => {
                    if let Some(init) = &local.init {
                        if let Expr::Closure(closure) = &*init.expr {
                            if let Expr::Block(block) = &*closure.body {
                                walk(&block.block.stmts, src, sites);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    for item in &file.items {
        if let syn::Item::Fn(func) = item {
            walk(&func.block.stmts, src, &mut sites);
        }
    }
    let Some(&(a, b)) = sites.get(index) else {
        return MutateResult { applicable: false, source: src.into(), reason: "no notify statement".into() };
    };
    let mut end = b;
    if src[end..].starts_with(';') {
        end += 1;
    }
    if src[end..].starts_with('\n') {
        end += 1;
    }
    let mut out = String::new();
    out.push_str(&src[..a]);
    out.push_str(&src[end..]);
    MutateResult { applicable: true, source: out, reason: "removed one notify statement".into() }
}

pub fn append_comment(src: &str) -> MutateResult {
    let mut out = src.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("// kept-comment\n");
    MutateResult { applicable: true, source: out, reason: "comment only".into() }
}

pub fn apply(op: &str, src: &str, index: usize) -> MutateResult {
    match op {
        "rename-local" => rename_local(src, index),
        "swap-locks" => swap_adjacent_locks(src, index),
        "omit-notify" => omit_notify_stmt(src, index),
        "comment" => append_comment(src),
        _ => MutateResult { applicable: false, source: src.into(), reason: format!("unknown op {op}") },
    }
}
