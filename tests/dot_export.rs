use concir::ast::*;
use concir::export::{DotDirection, DotOptions};

fn load_example(name: &str) -> Program {
    let path = format!("examples/{name}.json");
    let json = std::fs::read_to_string(&path).expect(&format!("load {path}"));
    serde_json::from_str(&json).expect(&format!("parse {path}"))
}

fn make_linear_function() -> Function {
    Function {
        name: "linear".into(),
        kind: "normal".into(),
        effects: None,
        module: None,
        body: vec![
            Statement {
                sid: "s1".into(),
                op: Op::ResOp {
                    resource: "mtx".into(),
                    action: "lock".into(),
                    args: vec![],
                },
                transfer: Transfer::Next("s2".into()),
            },
            Statement {
                sid: "s2".into(),
                op: Op::ResOp {
                    resource: "x".into(),
                    action: "write".into(),
                    args: vec!["42".into()],
                },
                transfer: Transfer::Next("s3".into()),
            },
            Statement {
                sid: "s3".into(),
                op: Op::ResOp {
                    resource: "mtx".into(),
                    action: "drop".into(),
                    args: vec![],
                },
                transfer: Transfer::Next("s4".into()),
            },
            Statement {
                sid: "s4".into(),
                op: Op::Return,
                transfer: Transfer::Return,
            },
        ],
    }
}

fn make_branch_function() -> Function {
    Function {
        name: "branching".into(),
        kind: "normal".into(),
        effects: None,
        module: None,
        body: vec![
            Statement {
                sid: "s1".into(),
                op: Op::ResOp {
                    resource: "flag".into(),
                    action: "read".into(),
                    args: vec![],
                },
                transfer: Transfer::Branch {
                    cond: "flag == true".into(),
                    true_target: "s2".into(),
                    false_target: "s3".into(),
                },
            },
            Statement {
                sid: "s2".into(),
                op: Op::ResOp {
                    resource: "x".into(),
                    action: "write".into(),
                    args: vec!["1".into()],
                },
                transfer: Transfer::Next("s4".into()),
            },
            Statement {
                sid: "s3".into(),
                op: Op::ResOp {
                    resource: "x".into(),
                    action: "write".into(),
                    args: vec!["0".into()],
                },
                transfer: Transfer::Next("s4".into()),
            },
            Statement {
                sid: "s4".into(),
                op: Op::Return,
                transfer: Transfer::Return,
            },
        ],
    }
}

fn make_loop_function() -> Function {
    Function {
        name: "looping".into(),
        kind: "closure".into(),
        effects: None,
        module: None,
        body: vec![
            Statement {
                sid: "s1".into(),
                op: Op::ResOp {
                    resource: "counter".into(),
                    action: "read".into(),
                    args: vec![],
                },
                transfer: Transfer::Branch {
                    cond: "counter < 10".into(),
                    true_target: "s2".into(),
                    false_target: "s3".into(),
                },
            },
            Statement {
                sid: "s2".into(),
                op: Op::ResOp {
                    resource: "counter".into(),
                    action: "write".into(),
                    args: vec!["counter + 1".into()],
                },
                transfer: Transfer::Next("s1".into()),
            },
            Statement {
                sid: "s3".into(),
                op: Op::Return,
                transfer: Transfer::Return,
            },
        ],
    }
}

// ── Linear control flow ─────────────────────────────────────────────────────

#[test]
fn linear_function_dot() {
    let func = make_linear_function();
    let dot = func.to_dot();

    assert!(dot.starts_with("digraph \"linear\""));
    assert!(dot.contains("linear_s1"));
    assert!(dot.contains("linear_s2"));
    assert!(dot.contains("linear_s3"));
    assert!(dot.contains("linear_s4"));
    assert!(dot.contains("linear_ret"));
    // Edges: s1→s2, s2→s3, s3→s4, s4→ret
    assert!(dot.contains("linear_s1 -> linear_s2"));
    assert!(dot.contains("linear_s2 -> linear_s3"));
    assert!(dot.contains("linear_s3 -> linear_s4"));
    assert!(dot.contains("linear_s4 -> linear_ret"));
    // Node count: 4 statements + 1 ret = 5 nodes
    let node_count = dot.matches("[label=").count();
    assert_eq!(node_count, 5, "expected 5 nodes (4 stmts + ret)");
}

#[test]
fn linear_node_styles() {
    let func = make_linear_function();
    let dot = func.to_dot();

    // lock → red border
    assert!(dot.contains("linear_s1") && dot.contains("color=red"));
    // write → orange border
    assert!(dot.contains("linear_s2") && dot.contains("color=orange"));
    // drop → green border
    assert!(dot.contains("linear_s3") && dot.contains("color=green"));
    // return → ellipse
    assert!(dot.contains("linear_s4") && dot.contains("shape=ellipse"));
    // first statement (s1) has penwidth=3
    assert!(dot.contains("penwidth=3"));
}

// ── Branch control flow ─────────────────────────────────────────────────────

#[test]
fn branch_function_dot() {
    let func = make_branch_function();
    let dot = func.to_dot();

    // Branch node should be diamond
    assert!(dot.contains("branching_s1") && dot.contains("shape=diamond"));
    // T/F edge labels
    assert!(dot.contains("label=\"T\""));
    assert!(dot.contains("label=\"F\""));
    // T edge green, F edge red
    assert!(dot.contains("color=green"));
    assert!(dot.contains("color=red"));
    // F edge dashed
    assert!(dot.contains("style=dashed"));
}

// ── Switch control flow ─────────────────────────────────────────────────────

#[test]
fn switch_from_state_machine() {
    let prog = load_example("state_machine");
    let worker = prog.functions.iter().find(|f| f.name == "worker").unwrap();
    let dot = worker.to_dot();

    // s21 is the switch node (reads state, then switches)
    assert!(dot.contains("worker_s21") && dot.contains("shape=diamond"));
    // Case labels
    assert!(dot.contains("label=\"Init\""));
    assert!(dot.contains("label=\"Running\""));
    assert!(dot.contains("label=\"Paused\""));
    assert!(dot.contains("label=\"Stopped\""));
}

// ── Loop / back-edge ────────────────────────────────────────────────────────

#[test]
fn loop_back_edge_highlighted() {
    let func = make_loop_function();
    let dot = func.to_dot();

    // s2 → s1 is a back edge (2 > 1)
    assert!(
        dot.contains("looping_s2 -> looping_s1 [color=blue, penwidth=2]"),
        "back edge s2→s1 should be blue: {dot}"
    );
}

#[test]
fn cross_function_spawn_join() {
    let prog = load_example("producer_consumer");
    let dot = prog.to_dot();

    // spawn edges
    assert!(dot.contains("main_s1 -> producer_s1"));
    assert!(dot.contains("label=\"spawn\""));
    assert!(dot.contains("main_s2 -> consumer_s1"));

    // join edges (target_ret → join_node)
    assert!(dot.contains("producer_ret -> main_s3"));
    assert!(dot.contains("label=\"join\""));
    assert!(dot.contains("consumer_ret -> main_s4"));
}

#[test]
fn cross_function_call() {
    let prog = load_example("with_summary");
    let dot = prog.to_dot();

    // worker s11 calls validate, but validate has no function body
    // so no cross-function edge emitted (target not in functions list)
    // The call node should still be rendered with rounded style
    assert!(dot.contains("worker_s11"));
    assert!(dot.contains("style=\"filled,rounded\""));
}

// ── Resource panel ──────────────────────────────────────────────────────────

#[test]
fn resource_panel_present() {
    let prog = load_example("producer_consumer");
    let dot = prog.to_dot();

    assert!(dot.contains("subgraph cluster_resources"));
    assert!(dot.contains("res_mtx"));
    assert!(dot.contains("res_cv"));
    assert!(dot.contains("res_count"));

    // Protection edge
    assert!(dot.contains("res_count -> res_mtx [style=dotted, dir=both"));
}

#[test]
fn resource_panel_shapes() {
    let prog = load_example("producer_consumer");
    let dot = prog.to_dot();

    // Mutex → hexagon
    assert!(dot.contains("res_mtx") && dot.contains("shape=hexagon"));
    // Condvar → triangle
    assert!(dot.contains("res_cv") && dot.contains("shape=triangle"));
    // Var → rect
    assert!(dot.contains("res_count") && dot.contains("shape=rect"));
}

#[test]
fn no_resource_panel_when_disabled() {
    let prog = load_example("producer_consumer");
    let opts = DotOptions {
        show_resources: false,
        ..DotOptions::default()
    };
    let dot = prog.to_dot_with_options(&opts);

    assert!(!dot.contains("cluster_resources"));
}

// ── Options ─────────────────────────────────────────────────────────────────

#[test]
fn direction_left_right() {
    let prog = load_example("with_summary");
    let opts = DotOptions {
        direction: DotDirection::LeftRight,
        ..DotOptions::default()
    };
    let dot = prog.to_dot_with_options(&opts);

    assert!(dot.contains("rankdir=LR"));
}

#[test]
fn verbose_labels() {
    let prog = load_example("with_summary");
    let opts = DotOptions {
        verbose_labels: true,
        ..DotOptions::default()
    };
    let dot = prog.to_dot_with_options(&opts);

    // Verbose labels contain full op type like "res_op(...)"
    assert!(dot.contains("res_op("));
}

#[test]
fn no_cross_function_when_disabled() {
    let prog = load_example("producer_consumer");
    let opts = DotOptions {
        show_cross_function: false,
        ..DotOptions::default()
    };
    let dot = prog.to_dot_with_options(&opts);

    assert!(!dot.contains("label=\"spawn\""));
    assert!(!dot.contains("label=\"join\""));
}

// ── Concurrency ops ─────────────────────────────────────────────────────────

#[test]
fn spawn_join_node_shapes() {
    let prog = load_example("producer_consumer");
    let dot = prog.to_dot();

    // Spawn should be doubleoctagon
    assert!(dot.contains("main_s1") && dot.contains("shape=doubleoctagon"));
    // Join should be doubleoctagon with dashed
    assert!(dot.contains("main_s3") && dot.contains("shape=doubleoctagon"));
}

#[test]
fn condvar_node_styles() {
    let prog = load_example("producer_consumer");
    let dot = prog.to_dot();

    // wait → purple, penwidth=2
    assert!(dot.contains("consumer_s3") && dot.contains("color=purple"));
    // notify_all → purple, dashed
    assert!(dot.contains("producer_s3") && dot.contains("color=purple"));
}

// ── insta snapshots ─────────────────────────────────────────────────────────

#[test]
fn snapshot_producer_consumer_dot() {
    let prog = load_example("producer_consumer");
    let dot = prog.to_dot();
    insta::assert_snapshot!("producer_consumer_dot", dot);
}

#[test]
fn snapshot_state_machine_dot() {
    let prog = load_example("state_machine");
    let dot = prog.to_dot();
    insta::assert_snapshot!("state_machine_dot", dot);
}

#[test]
fn snapshot_with_summary_dot() {
    let prog = load_example("with_summary");
    let dot = prog.to_dot();
    insta::assert_snapshot!("with_summary_dot", dot);
}

#[test]
fn snapshot_function_only() {
    let prog = load_example("producer_consumer");
    let producer = prog
        .functions
        .iter()
        .find(|f| f.name == "producer")
        .unwrap();
    let dot = producer.to_dot();
    insta::assert_snapshot!("producer_function_only_dot", dot);
}
