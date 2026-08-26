use std::fmt::Write;

use crate::ast::{Block, Call, Function, Program, Terminator};

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
        kind = escape(&func.kind),
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

    for stmt in &func.body {
        write_edges(out, prefix, stmt, opts);
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

fn node_style(stmt: &Block) -> NodeStyle {
    let transfer_override = match &stmt.terminator {
        Some(Terminator::Branch { .. }) => Some(NodeStyle {
            shape: "diamond",
            fillcolor: "#ffffcc",
            ..Default::default()
        }),
        Some(Terminator::Switch { .. }) => Some(NodeStyle {
            shape: "diamond",
            fillcolor: "#ffe8cc",
            ..Default::default()
        }),
        _ => None,
    };

    if let Some(s) = transfer_override {
        return s;
    }

    match &stmt.call {
        Some(Call::MutexLock { .. } | Call::RwLockWrite { .. } | Call::SemaphoreAcquire { .. }) => {
            NodeStyle {
                color: "red",
                penwidth: 2,
                ..Default::default()
            }
        }
        Some(
            Call::MutexUnlock { .. } | Call::RwLockUnlock { .. } | Call::SemaphoreRelease { .. },
        ) => NodeStyle {
            color: "green",
            penwidth: 2,
            ..Default::default()
        },
        Some(
            Call::CondvarWait { .. } | Call::CondvarNotify { .. } | Call::CondvarNotifyAll { .. },
        ) => NodeStyle {
            color: "purple",
            penwidth: 2,
            ..Default::default()
        },
        Some(Call::Spawn { .. } | Call::SpawnBatch { .. } | Call::AsyncCall { .. }) => NodeStyle {
            shape: "doubleoctagon",
            ..Default::default()
        },
        Some(Call::Join { .. } | Call::JoinAll { .. } | Call::Await { .. }) => NodeStyle {
            shape: "doubleoctagon",
            style: "filled,dashed".to_string(),
            ..Default::default()
        },
        Some(Call::Func { .. }) => NodeStyle {
            shape: "rect",
            style: "filled,rounded".to_string(),
            ..Default::default()
        },
        _ if stmt
            .statements
            .iter()
            .any(|s| matches!(s, crate::ast::Stmt::WriteShared { .. })) =>
        {
            NodeStyle {
                color: "orange",
                penwidth: 2,
                ..Default::default()
            }
        }
        _ if stmt.is_return() => NodeStyle {
            shape: "ellipse",
            fillcolor: "#a0a0a0",
            ..Default::default()
        },
        _ => NodeStyle::default(),
    }
}

fn write_node(out: &mut String, prefix: &str, stmt: &Block, is_entry: bool, opts: &DotOptions) {
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

fn format_label_compact(stmt: &Block) -> String {
    let op_str = if let Some(call) = &stmt.call {
        match call {
            Call::MutexLock { resource, .. } => format!("mutex_lock({resource})"),
            Call::MutexUnlock { resource, .. } => format!("mutex_unlock({resource})"),
            Call::Spawn { func, .. } => format!("spawn({func})"),
            Call::SpawnBatch { func, .. } => format!("spawn_batch({func})"),
            Call::Join { handle, .. } => format!("join({handle})"),
            Call::JoinAll { handle, .. } => format!("join_all({handle})"),
            Call::Func { func, .. } => format!("call({func})"),
            Call::Await { handle, .. } => format!("await({handle})"),
            Call::AsyncCall { func, .. } => format!("async_call({func})"),
            other => format!("{other:?}"),
        }
    } else if stmt.is_return() {
        "return".to_string()
    } else if let Some(Terminator::Goto { .. }) = &stmt.terminator {
        "goto".to_string()
    } else if let Some(Terminator::Branch { .. }) = &stmt.terminator {
        "branch".to_string()
    } else if let Some(Terminator::Switch { .. }) = &stmt.terminator {
        "switch".to_string()
    } else {
        "block".to_string()
    };
    escape(&format!("{}: {}", stmt.sid, op_str))
}

fn compact_args(args: &[String]) -> String {
    if args.len() == 1 {
        return escape(&args[0]);
    }
    // For CAS-like: "false", "true" → "F→T"
    if args.len() == 2 {
        let a = compact_val(&args[0]);
        let b = compact_val(&args[1]);
        return format!("{a}\\u2192{b}"); // →
    }
    args.iter()
        .map(|a| escape(a))
        .collect::<Vec<_>>()
        .join(", ")
}

fn compact_val(v: &str) -> String {
    match v {
        "true" => "T".to_string(),
        "false" => "F".to_string(),
        other => escape(other),
    }
}

fn format_label_verbose(stmt: &Block) -> String {
    format_label_compact(stmt)
}

// ── Edge generation ─────────────────────────────────────────────────────────

fn write_edges(out: &mut String, prefix: &str, stmt: &Block, opts: &DotOptions) {
    let src = format!("{prefix}_{}", stmt.sid);

    if stmt.is_return() {
        let dst = format!("{prefix}_ret");
        writeln!(out, "    {src} -> {dst};").unwrap();
        return;
    }

    match &stmt.terminator {
        Some(Terminator::Goto { target }) => {
            let dst = format!("{prefix}_{target}");
            if opts.highlight_back_edges && is_back_edge(&stmt.sid, target) {
                writeln!(out, "    {src} -> {dst} [color=blue, penwidth=2];").unwrap();
            } else {
                writeln!(out, "    {src} -> {dst};").unwrap();
            }
        }
        Some(Terminator::Branch {
            then, else_target, ..
        }) => {
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
        Some(Terminator::Switch { cases, default, .. }) => {
            for (label, target) in cases {
                let dst = format!("{prefix}_{target}");
                writeln!(out, "    {src} -> {dst} [label=\"{label}\"];").unwrap();
            }
            let dst = format!("{prefix}_{default}");
            writeln!(out, "    {src} -> {dst} [label=\"default\", style=dashed];").unwrap();
        }
        Some(Terminator::Return { .. }) | None => {
            if let Some(call) = &stmt.call {
                for t in call.successor_sids() {
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
            match &stmt.call {
                Some(Call::Spawn { func: target, .. } | Call::SpawnBatch { func: target, .. }) => {
                    let name = target.rsplit("::").next().unwrap_or(target);
                    if let Some(first) = first_sids.get(name) {
                        writeln!(
                            out,
                            "  {src} -> {name}_{first} [style=dashed, color=blue, label=\"spawn\"];",
                        )
                        .unwrap();
                    }
                }
                Some(Call::AsyncCall { func: target, .. }) => {
                    let name = target.rsplit("::").next().unwrap_or(target);
                    if let Some(first) = first_sids.get(name) {
                        writeln!(
                            out,
                            "  {src} -> {name}_{first} [style=dashed, color=blue, label=\"async_call\"];",
                        )
                        .unwrap();
                    }
                }
                Some(Call::Join { handle, .. } | Call::JoinAll { handle, .. }) => {
                    if let Some(name) = handle.strip_prefix("h_") {
                        writeln!(
                            out,
                            "  {name}_ret -> {src} [style=dashed, color=purple, label=\"join\"];",
                        )
                        .unwrap();
                    }
                }
                Some(Call::Func { func: target, .. }) => {
                    let name = target.rsplit("::").next().unwrap_or(target);
                    if let Some(first) = first_sids.get(name) {
                        writeln!(
                            out,
                            "  {src} -> {name}_{first} [style=dotted, color=gray50, label=\"call\"];",
                        )
                        .unwrap();
                    }
                }
                _ => {}
            }
        }
    }
    writeln!(out).unwrap();
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

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('<', "\\<")
        .replace('>', "\\>")
        .replace('{', "\\{")
        .replace('}', "\\}")
}
