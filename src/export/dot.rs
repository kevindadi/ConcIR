use std::fmt::Write;

use crate::ast::{Function, Op, Program, SelectGuard, Stmt};

// ── Public types ────────────────────────────────────────────────────────────

/// Graph layout direction.
pub enum DotDirection {
    /// Top to bottom (default).
    TopBottom,
    /// Left to right — better for wide functions.
    LeftRight,
}

/// Options controlling DOT export appearance.
pub struct DotOptions {
    /// Show the resource panel subgraph.
    pub show_resources: bool,
    /// Show cross-function edges (spawn/join/call).
    pub show_cross_function: bool,
    /// Highlight back-edges (loop detection) in blue.
    pub highlight_back_edges: bool,
    /// Use verbose multi-line labels instead of compact ones.
    pub verbose_labels: bool,
    /// Graph layout direction.
    pub direction: DotDirection,
}

impl Default for DotOptions {
    fn default() -> Self {
        Self {
            show_resources: true,
            show_cross_function: true,
            highlight_back_edges: true,
            verbose_labels: false,
            direction: DotDirection::TopBottom,
        }
    }
}

// ── Top-level entry points ──────────────────────────────────────────────────

pub(crate) fn program_to_dot(program: &Program, opts: &DotOptions) -> String {
    let mut out = String::new();

    let dir = match opts.direction {
        DotDirection::TopBottom => "TB",
        DotDirection::LeftRight => "LR",
    };

    writeln!(out, "digraph \"{}\" {{", escape(&program.program)).unwrap();
    writeln!(out, "  rankdir={dir};").unwrap();
    writeln!(out, "  node [fontname=\"Courier\", fontsize=10];").unwrap();
    writeln!(out, "  edge [fontname=\"Courier\", fontsize=9];").unwrap();
    writeln!(out).unwrap();

    let has_resources = program.modules.iter().any(|m| !m.resources.is_empty());
    if opts.show_resources && has_resources {
        write_resource_panel(&mut out, program);
    }

    let functions: Vec<&Function> = program
        .modules
        .iter()
        .flat_map(|m| m.functions.iter())
        .collect();
    for func in &functions {
        write_function_subgraph(&mut out, func, opts);
    }

    if opts.show_cross_function {
        write_cross_function_edges(&mut out, &functions);
    }

    writeln!(out, "}}").unwrap();
    out
}

pub(crate) fn function_to_dot(func: &Function) -> String {
    let opts = DotOptions {
        show_resources: false,
        show_cross_function: false,
        ..DotOptions::default()
    };
    let mut out = String::new();

    writeln!(out, "digraph \"{}\" {{", escape(&func.name)).unwrap();
    writeln!(out, "  rankdir=TB;").unwrap();
    writeln!(out, "  node [fontname=\"Courier\", fontsize=10];").unwrap();
    writeln!(out, "  edge [fontname=\"Courier\", fontsize=9];").unwrap();
    writeln!(out).unwrap();

    write_function_subgraph(&mut out, func, &opts);

    writeln!(out, "}}").unwrap();
    out
}

// ── Resource panel ──────────────────────────────────────────────────────────

fn write_resource_panel(out: &mut String, program: &Program) {
    writeln!(out, "  subgraph cluster_resources {{").unwrap();
    writeln!(out, "    label=\"Resources\";").unwrap();
    writeln!(out, "    style=dashed;").unwrap();
    writeln!(out, "    color=gray;").unwrap();
    writeln!(out).unwrap();

    for m in &program.modules {
        for res in &m.resources {
            let (shape, fill) = match res.res_type.as_str() {
                "Mutex" | "RwLock" => ("hexagon", "#ffe0e0"),
                "Condvar" => ("triangle", "#f0e0ff"),
                "Semaphore" => ("house", "#e0ffe0"),
                "Channel" => ("parallelogram", "#e0f0ff"),
                _ => ("rect", "#f0f0f0"), // Var, Atomic
            };

            writeln!(
            out,
            "    res_{name} [label=\"{name}\\n({typ})\", shape={shape}, style=filled, fillcolor=\"{fill}\"];",
            name = escape(&res.name),
            typ = escape(&res.res_type),
        )
        .unwrap();
        }
    }

    writeln!(out).unwrap();

    for m in &program.modules {
        for prot in &m.protection {
            writeln!(
                out,
                "    res_{var} -> res_{lock} [style=dotted, dir=both, color=gray50];",
                var = escape(&prot.var),
                lock = escape(&prot.lock),
            )
            .unwrap();
        }
    }

    writeln!(out, "  }}").unwrap();
    writeln!(out).unwrap();
}

// ── Function subgraph ───────────────────────────────────────────────────────

fn write_function_subgraph(out: &mut String, func: &Function, opts: &DotOptions) {
    let prefix = &func.name;

    writeln!(out, "  subgraph cluster_{prefix} {{",).unwrap();
    writeln!(
        out,
        "    label=\"{name} ({kind})\";",
        name = escape(&func.name),
        kind = escape(&cluster_kind_label(func)),
    )
    .unwrap();
    writeln!(out, "    style=rounded;").unwrap();
    writeln!(out, "    color=black;").unwrap();
    writeln!(out).unwrap();

    let first_sid = func.body.first().map(|s| s.sid.as_str());

    for stmt in &func.body {
        let is_entry = Some(stmt.sid.as_str()) == first_sid;
        write_node(out, prefix, stmt, is_entry, opts);
    }

    // Virtual [ret] node
    writeln!(
        out,
        "    {prefix}_ret [label=\"[ret]\", shape=ellipse, style=filled, fillcolor=\"#a0a0a0\"];",
    )
    .unwrap();

    writeln!(out).unwrap();

    for (i, _) in func.body.iter().enumerate() {
        write_edges(out, prefix, func, i, opts);
    }

    writeln!(out, "  }}").unwrap();
    writeln!(out).unwrap();
}

// ── Node generation ─────────────────────────────────────────────────────────

struct NodeStyle {
    shape: &'static str,
    style: String,
    fillcolor: &'static str,
    color: &'static str,
    penwidth: u8,
}

impl Default for NodeStyle {
    fn default() -> Self {
        Self {
            shape: "rect",
            style: "filled".to_string(),
            fillcolor: "#f0f0f0",
            color: "black",
            penwidth: 1,
        }
    }
}

fn node_style(stmt: &Stmt) -> NodeStyle {
    match &stmt.op {
        Op::Branch { .. } => NodeStyle {
            shape: "diamond",
            fillcolor: "#ffffcc",
            ..Default::default()
        },
        Op::Switch { .. } | Op::Select { .. } => NodeStyle {
            shape: "diamond",
            fillcolor: "#ffe8cc",
            ..Default::default()
        },
        Op::Return { .. } => NodeStyle {
            shape: "ellipse",
            fillcolor: "#a0a0a0",
            ..Default::default()
        },
        Op::MutexLock { .. } | Op::RwLockWrite { .. } | Op::SemaphoreAcquire { .. } => NodeStyle {
            color: "red",
            penwidth: 2,
            ..Default::default()
        },
        Op::MutexUnlock { .. } | Op::RwLockUnlock { .. } | Op::SemaphoreRelease { .. } => {
            NodeStyle {
                color: "green",
                penwidth: 2,
                ..Default::default()
            }
        }
        Op::CondvarWait { .. } | Op::CondvarNotify { .. } | Op::CondvarNotifyAll { .. } => {
            NodeStyle {
                color: "purple",
                penwidth: 2,
                ..Default::default()
            }
        }
        Op::Spawn { .. } | Op::Scope { .. } | Op::AsyncCall { .. } => NodeStyle {
            shape: "doubleoctagon",
            ..Default::default()
        },
        Op::Join { .. } | Op::Await { .. } => NodeStyle {
            shape: "doubleoctagon",
            style: "filled,dashed".to_string(),
            ..Default::default()
        },
        Op::Func { .. } => NodeStyle {
            shape: "rect",
            style: "filled,rounded".to_string(),
            ..Default::default()
        },
        Op::WriteShared { .. } => NodeStyle {
            color: "orange",
            penwidth: 2,
            ..Default::default()
        },
        _ => NodeStyle::default(),
    }
}

fn write_node(out: &mut String, prefix: &str, stmt: &Stmt, is_entry: bool, opts: &DotOptions) {
    let label = if opts.verbose_labels {
        format_label_verbose(stmt)
    } else {
        format_label_compact(stmt)
    };

    let ns = node_style(stmt);
    let pw = if is_entry {
        3.max(ns.penwidth)
    } else {
        ns.penwidth
    };

    writeln!(
        out,
        "    {prefix}_{sid} [label=\"{label}\", shape={shape}, style=\"{style}\", fillcolor=\"{fill}\", color={color}, penwidth={pw}];",
        sid = stmt.sid,
        shape = ns.shape,
        style = ns.style,
        fill = ns.fillcolor,
        color = ns.color,
    )
    .unwrap();
}

// ── Label formatting ────────────────────────────────────────────────────────

fn format_label_compact(stmt: &Stmt) -> String {
    escape(&format!("{}: {}", stmt.sid, format_op_compact(&stmt.op)))
}

fn format_op_compact(op: &Op) -> String {
    match op {
        Op::Nop => "nop".into(),
        Op::AssignLocal { target, expr } => format!("assign({target}, {expr})"),
        Op::ReadShared { resource, .. } => format!("read_shared({resource})"),
        Op::WriteShared { resource, expr } => format!("write_shared({resource}, {expr})"),
        Op::AbstractStep { desc, .. } => {
            if desc.is_empty() {
                "abstract_step".into()
            } else {
                format!("abstract_step({desc})")
            }
        }
        Op::AtomicLoad { resource, .. } => format!("atomic_load({resource})"),
        Op::AtomicStore { resource, .. } => format!("atomic_store({resource})"),
        Op::AtomicCas { resource, .. } => format!("atomic_cas({resource})"),
        Op::MutexLock { resource } => format!("mutex_lock({resource})"),
        Op::MutexUnlock { resource } => format!("mutex_unlock({resource})"),
        Op::RwLockRead { resource } => format!("rwlock_read({resource})"),
        Op::RwLockWrite { resource } => format!("rwlock_write({resource})"),
        Op::RwLockUnlock { resource } => format!("rwlock_unlock({resource})"),
        Op::ChannelSend { channel, .. } => format!("channel_send({channel})"),
        Op::ChannelRecv { channel, dst } => format!("channel_recv({channel} → {dst})"),
        Op::CondvarWait { condvar, lock } => format!("condvar_wait({condvar}, {lock})"),
        Op::CondvarNotify { condvar } => format!("condvar_notify({condvar})"),
        Op::CondvarNotifyAll { condvar } => format!("condvar_notify_all({condvar})"),
        Op::SemaphoreAcquire { resource, .. } => format!("semaphore_acquire({resource})"),
        Op::SemaphoreRelease { resource, .. } => format!("semaphore_release({resource})"),
        Op::Func { func, .. } => format!("call({func})"),
        Op::Spawn { func, .. } => format!("spawn({func})"),
        Op::Scope { funcs } => format!("scope({})", funcs.join(", ")),
        Op::Join { handle } => format!("join({handle})"),
        Op::AsyncCall { func, .. } => format!("async_call({func})"),
        Op::Await { handle } => format!("await({handle})"),
        Op::Goto { .. } => "goto".into(),
        Op::Branch { .. } => "branch".into(),
        Op::Switch { .. } => "switch".into(),
        Op::Return { .. } => "return".into(),
        Op::Select { .. } => "select".into(),
    }
}

fn format_label_verbose(stmt: &Stmt) -> String {
    format_label_compact(stmt)
}

// ── Edge generation ─────────────────────────────────────────────────────────

fn write_edges(out: &mut String, prefix: &str, func: &Function, i: usize, opts: &DotOptions) {
    let stmt = &func.body[i];
    let src = format!("{prefix}_{}", stmt.sid);

    if stmt.is_return() {
        let dst = format!("{prefix}_ret");
        writeln!(out, "    {src} -> {dst};").unwrap();
        return;
    }

    match &stmt.op {
        Op::Goto { target } => {
            let dst = format!("{prefix}_{target}");
            if opts.highlight_back_edges && is_back_edge(&stmt.sid, target) {
                writeln!(out, "    {src} -> {dst} [color=blue, penwidth=2];").unwrap();
            } else {
                writeln!(out, "    {src} -> {dst};").unwrap();
            }
        }
        Op::Branch {
            then, else_target, ..
        } => {
            let dst_t = format!("{prefix}_{then}");
            let dst_f = format!("{prefix}_{else_target}");
            let back_t = opts.highlight_back_edges && is_back_edge(&stmt.sid, then);
            let back_f = opts.highlight_back_edges && is_back_edge(&stmt.sid, else_target);
            if back_t {
                writeln!(
                    out,
                    "    {src} -> {dst_t} [label=\"T\", color=blue, penwidth=2];"
                )
                .unwrap();
            } else {
                writeln!(out, "    {src} -> {dst_t} [label=\"T\", color=green];").unwrap();
            }
            if back_f {
                writeln!(
                    out,
                    "    {src} -> {dst_f} [label=\"F\", color=blue, penwidth=2, style=dashed];"
                )
                .unwrap();
            } else {
                writeln!(
                    out,
                    "    {src} -> {dst_f} [label=\"F\", style=dashed, color=red];"
                )
                .unwrap();
            }
        }
        Op::Switch { cases, default, .. } => {
            for (label, target) in cases {
                let dst = format!("{prefix}_{target}");
                writeln!(out, "    {src} -> {dst} [label=\"{label}\"];").unwrap();
            }
            let dst = format!("{prefix}_{default}");
            writeln!(out, "    {src} -> {dst} [label=\"default\", style=dashed];").unwrap();
        }
        Op::Select { branches, default } => {
            for branch in branches {
                let dst = format!("{prefix}_{}", branch.target);
                let label = match &branch.guard {
                    SelectGuard::ChannelRecv { channel, dst: cap } => {
                        format!("recv {channel}→{cap}")
                    }
                    SelectGuard::CondvarWait { condvar, .. } => {
                        format!("wait {condvar}")
                    }
                    SelectGuard::SemaphoreAcquire { resource } => {
                        format!("acquire {resource}")
                    }
                };
                writeln!(out, "    {src} -> {dst} [label=\"{label}\"];").unwrap();
            }
            if let Some(d) = default {
                let dst = format!("{prefix}_{d}");
                writeln!(out, "    {src} -> {dst} [label=\"default\", style=dashed];").unwrap();
            }
        }
        _ => {
            for t in func.successors(i) {
                let dst = format!("{prefix}_{t}");
                if opts.highlight_back_edges && is_back_edge(&stmt.sid, t) {
                    writeln!(out, "    {src} -> {dst} [color=blue, penwidth=2];").unwrap();
                } else {
                    writeln!(out, "    {src} -> {dst};").unwrap();
                }
            }
        }
    }
}

// ── Cross-function edges ────────────────────────────────────────────────────

fn write_cross_function_edges(out: &mut String, functions: &[&Function]) {
    writeln!(out, "  // Cross-function edges").unwrap();

    // Build a map: fn_name → first sid
    let first_sids: std::collections::HashMap<&str, &str> = functions
        .iter()
        .filter_map(|f| f.body.first().map(|s| (f.name.as_str(), s.sid.as_str())))
        .collect();

    for func in functions {
        for stmt in &func.body {
            let src = format!("{}_{}", func.name, stmt.sid);
            match &stmt.op {
                Op::Spawn { func: target, .. } => {
                    write_callee_edge(out, &src, target, &first_sids, "spawn", "dashed", "blue");
                }
                Op::Scope { funcs } => {
                    for target in funcs {
                        write_callee_edge(
                            out,
                            &src,
                            target,
                            &first_sids,
                            "scope",
                            "dashed",
                            "blue",
                        );
                        let name = target.rsplit("::").next().unwrap_or(target);
                        writeln!(
                            out,
                            "  {name}_ret -> {src} [style=dashed, color=purple, label=\"join\"];",
                        )
                        .unwrap();
                    }
                }
                Op::AsyncCall { func: target, .. } => {
                    write_callee_edge(
                        out,
                        &src,
                        target,
                        &first_sids,
                        "async_call",
                        "dashed",
                        "blue",
                    );
                }
                Op::Join { handle } => {
                    if let Some(name) = handle.strip_prefix("h_") {
                        writeln!(
                            out,
                            "  {name}_ret -> {src} [style=dashed, color=purple, label=\"join\"];",
                        )
                        .unwrap();
                    }
                }
                Op::Func { func: target, .. } => {
                    write_callee_edge(out, &src, target, &first_sids, "call", "dotted", "gray50");
                }
                _ => {}
            }
        }
    }
    writeln!(out).unwrap();
}

fn write_callee_edge(
    out: &mut String,
    src: &str,
    target: &str,
    first_sids: &std::collections::HashMap<&str, &str>,
    label: &str,
    style: &str,
    color: &str,
) {
    let name = target.rsplit("::").next().unwrap_or(target);
    if let Some(first) = first_sids.get(name) {
        writeln!(
            out,
            "  {src} -> {name}_{first} [style={style}, color={color}, label=\"{label}\"];",
        )
        .unwrap();
    }
}

// ── Back-edge detection ─────────────────────────────────────────────────────

fn sid_number(sid: &str) -> Option<u32> {
    sid.strip_prefix('s').and_then(|n| n.parse().ok())
}

fn is_back_edge(current: &str, target: &str) -> bool {
    match (sid_number(current), sid_number(target)) {
        (Some(c), Some(t)) => t < c,
        _ => false,
    }
}

// ── Utility ─────────────────────────────────────────────────────────────────

fn cluster_kind_label(func: &Function) -> String {
    if func.is_async() {
        "async".into()
    } else if func.is_closure() {
        "closure".into()
    } else {
        "normal".into()
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('<', "\\<")
        .replace('>', "\\>")
        .replace('{', "\\{")
        .replace('}', "\\}")
}
