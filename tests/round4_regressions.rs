//! Round-4 review regressions (C1–C3).

use std::collections::{BTreeMap, HashSet, VecDeque};

use concir::ast::Program;
use concir::explore::contract::{ContractSpec, Property};
use concir::explore::{verify_program, EngineKind, VerificationReport};
use concir::interp::Interpreter;
use concir::petri::exec::PetriEngine;
use concir::sem::outcome::{AnalysisBounds, Outcome};
use concir::sem::program;
use concir::sem::system::TransitionSystem;
use concir::sem::value::{within_type, Value};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("tests/repro_round4/{name}")).expect(name)
}

fn prog(name: &str) -> Program {
    serde_json::from_str(&fixture(name)).unwrap()
}

fn spec(name: &str) -> ContractSpec {
    serde_json::from_str(&fixture(name)).unwrap()
}

fn run(program_name: &str, contract_name: &str, engine: EngineKind) -> VerificationReport {
    verify_program(&prog(program_name), &spec(contract_name), engine)
}

fn run_str(src: &str, contract_src: &str, engine: EngineKind) -> VerificationReport {
    verify_program(
        &serde_json::from_str(src).unwrap(),
        &serde_json::from_str(contract_src).unwrap(),
        engine,
    )
}

fn both(program_name: &str, contract_name: &str) -> (Outcome, Outcome, bool, bool) {
    let i = run(program_name, contract_name, EngineKind::Interpreter);
    let p = run(program_name, contract_name, EngineKind::Petri);
    (i.outcome, p.outcome, i.complete, p.complete)
}

// ── C1: unambiguous state key ───────────────────────────────────────

#[test]
fn c1_canonical_collision_no_longer_merges_states() {
    // Both final values A and B are reachable after the scope completes.
    let (ia, pa, _ica, _pca) = both(
        "r3_canonical_collision_A.json",
        "r3_canonical_collision_A_contract.json",
    );
    assert_eq!(ia, Outcome::Pass, "A must be reachable");
    assert_eq!(pa, Outcome::Pass, "A must be reachable");

    let (ib, pb, _, _) = both(
        "r3_canonical_collision_B.json",
        "r3_canonical_collision_B_contract.json",
    );
    assert_eq!(ib, Outcome::Pass, "B must be reachable");
    assert_eq!(pb, Outcome::Pass, "B must be reachable");

    // AG(!(scope_done && x=A)) is violated because A is reachable.
    let (ir, pr, _, _) = both(
        "r3_canonical_false_pass.json",
        "r3_canonical_false_pass_contract.json",
    );
    assert_eq!(
        ir,
        Outcome::Fail,
        "reaching A must violate the safety property"
    );
    assert_eq!(pr, Outcome::Fail);
}

#[test]
fn c1_value_key_is_injective_for_the_colliding_structs() {
    let mut a = BTreeMap::new();
    a.insert("a".to_string(), Value::Str("X\",b:\"Y".to_string()));
    a.insert("b".to_string(), Value::Str("Z".to_string()));
    let mut b = BTreeMap::new();
    b.insert("a".to_string(), Value::Str("X".to_string()));
    b.insert("b".to_string(), Value::Str("Y\",b:\"Z".to_string()));
    let va = Value::Struct(a);
    let vb = Value::Struct(b);
    assert_ne!(va, vb, "the two structs are different values");
    assert_ne!(va.key(), vb.key(), "semantic keys must differ");
    assert_ne!(
        va.canonical(),
        vb.canonical(),
        "display text must be unambiguous too"
    );
    // Length prefixes and type tags handle nested/odd strings.
    let weird = Value::Array(vec![
        Value::Str("[".to_string()),
        Value::Str("]".to_string()),
        Value::Str("T3:ab".to_string()),
    ]);
    assert_ne!(weird.key(), Value::Array(vec![]).key());
}

// Independent oracle: BFS on the *raw* State `Eq`/`Hash`, not the production
// key. It confirms the raw graph is finite and both goals are reachable, and
// that no two raw states share a state key while disagreeing on the goal.
fn raw_oracle<S: TransitionSystem>(engine: &S, goal: &concir::sem::system::Predicate, name: &str) {
    let mut seen: HashSet<S::State> = HashSet::new();
    let mut keys: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    let mut q: VecDeque<S::State> = VecDeque::from([engine.initial().unwrap()]);
    let mut reachable = false;
    let mut conflicts = 0usize;
    while let Some(state) = q.pop_front() {
        if !seen.insert(state.clone()) {
            continue;
        }
        assert!(seen.len() < 10_000, "{name}: fixture must be finite");
        let sat = engine.satisfied(&state, goal);
        reachable |= sat;
        if let Some(old) = keys.insert(engine.state_key(&state), sat) {
            if old != sat {
                conflicts += 1;
            }
        }
        let next = engine.successors(&state).unwrap();
        assert!(next.boundary.is_empty(), "{name}: no boundary expected");
        for st in next.steps {
            q.push_back(st.state);
        }
    }
    assert!(reachable, "{name}: goal must be reachable in the raw graph");
    assert_eq!(
        conflicts, 0,
        "{name}: equal key with different predicate truth"
    );
}

#[test]
fn c1_raw_state_oracle_agrees_with_production_key() {
    let program = prog("r3_canonical_collision_A.json");
    let sp = program::lower(&program).unwrap();
    for name in ["A", "B"] {
        let cs = spec(&format!("r3_canonical_collision_{name}_contract.json"));
        let contract = cs.resolve(&sp).unwrap();
        let Property::Reachability { goal } = &contract.properties[0].property else {
            panic!("expected reachability");
        };
        raw_oracle(
            &Interpreter::new(&sp, AnalysisBounds::default()),
            goal,
            &format!("interp-{name}"),
        );
        raw_oracle(
            &PetriEngine::new(&sp, AnalysisBounds::default()),
            goal,
            &format!("petri-{name}"),
        );
    }
}

// ── C2: channel payload domain in both engines ──────────────────────

#[test]
fn c2_channel_payload_domain_is_enforced() {
    for cap in [0, 1] {
        let p = format!("r3_channel_domain_{cap}.json");
        let c = format!("r3_channel_domain_{cap}_contract.json");
        let (i, pe, ic, pc) = both(&p, &c);
        assert_eq!(
            i,
            Outcome::Fail,
            "capacity {cap}: invalid payload must not pass"
        );
        assert_eq!(
            pe,
            Outcome::Fail,
            "capacity {cap}: invalid payload must not pass"
        );
        assert!(ic && pc, "capacity {cap}: search must be complete");
    }
}

#[test]
fn c2_valid_channel_payload_still_flows() {
    // Same shape with a = 1 (inside [0,1]) must complete without deadlock.
    let src = r#"{
      "program": "chan_ok",
      "modules": [{"name": "main",
        "resources": [
          {"name": "a", "kind": "var", "type": "Var", "base": "Int", "init": 1},
          {"name": "c", "kind": "sync", "type": "Channel", "mode": "Sync", "base": {"Int": [0,1]}, "capacity": 1}
        ],
        "functions": [
          {"name": "main", "kind": "normal", "body": [{"sid":"s1","kind":"scope","funcs":["sender","receiver"]},{"sid":"s2","kind":"return"}]},
          {"name": "sender", "kind": "normal", "form": "closure", "body": [{"sid":"s1","kind":"channel_send","channel":"c","value":"a"},{"sid":"s2","kind":"return"}]},
          {"name": "receiver", "kind": "normal", "form": "closure", "body": [{"sid":"s1","kind":"channel_recv","channel":"c","dst":"_"},{"sid":"s2","kind":"return"}]}
        ]}],
      "entry": "main::main"
    }"#;
    let contract = r#"{"name":"c","properties":[{"kind":"deadlock_free","id":"d"}]}"#;
    assert_eq!(
        run_str(src, contract, EngineKind::Interpreter).outcome,
        Outcome::Pass
    );
    assert_eq!(
        run_str(src, contract, EngineKind::Petri).outcome,
        Outcome::Pass
    );
}

// ── C3: recursive composite domains ─────────────────────────────────

#[test]
fn c3_within_type_recurses_into_composites() {
    use concir::ast::{BaseType, ComplexBaseType};
    use std::collections::BTreeMap;
    let bounded = BaseType::Complex(ComplexBaseType::BoundedInt { lo: 0, hi: 1 });
    let int_ty = BaseType::Primitive("Int".into());
    let struct_ty = BaseType::Complex(ComplexBaseType::Struct(BTreeMap::from([(
        "n".to_string(),
        bounded.clone(),
    )])));
    let mk = |n: i64| {
        let mut m = BTreeMap::new();
        m.insert("n".to_string(), Value::Int(n));
        Value::Struct(m)
    };
    assert!(within_type(&mk(0), &struct_ty));
    assert!(within_type(&mk(1), &struct_ty));
    assert!(!within_type(&mk(2), &struct_ty));
    // A non-struct value cannot satisfy a struct type.
    assert!(!within_type(&Value::Int(0), &struct_ty));
    // Nested arrays.
    let arr_ty = BaseType::Complex(ComplexBaseType::Array(Box::new(concir::ast::ArrayDef {
        elem: bounded,
        len: 2,
    })));
    assert!(within_type(
        &Value::Array(vec![Value::Int(0), Value::Int(1)]),
        &arr_ty
    ));
    assert!(!within_type(
        &Value::Array(vec![Value::Int(0), Value::Int(2)]),
        &arr_ty
    ));
    assert!(!within_type(&Value::Array(vec![Value::Int(0)]), &arr_ty));
    // Plain Int remains Int.
    assert!(within_type(&Value::Int(5), &int_ty));
    assert!(!within_type(&Value::Bool(true), &int_ty));
}

#[test]
fn c3_nested_bounded_field_update_is_disabled() {
    let (i, pe, ic, pc) = both("r3_nested_domain.json", "r3_nested_domain_contract.json");
    assert_eq!(
        i,
        Outcome::Fail,
        "nested out-of-domain update must be unreachable"
    );
    assert_eq!(pe, Outcome::Fail);
    assert!(ic && pc);
}

#[test]
fn c3_array_and_mixed_nesting_domains() {
    // Array<Int[0,1]> of length 1 sourced from an out-of-domain array.
    let arr = r#"{
      "program": "arr_domain",
      "modules": [{"name": "main",
        "resources": [
          {"name": "src", "kind": "var", "type": "Var", "base": {"Array": {"elem": "Int", "len": 1}}, "init": [2]},
          {"name": "x", "kind": "var", "type": "Var", "base": {"Array": {"elem": {"Int": [0,1]}, "len": 1}}, "init": [0]}
        ],
        "functions": [{"name": "main", "kind": "normal", "body": [{"sid":"s1","kind":"read_shared","resource":"src","dst":"x"},{"sid":"s2","kind":"return"}]}]
      }],
      "entry": "main::main"
    }"#;
    let goal = r#"{"name":"c","properties":[{"kind":"reachability","id":"bad","goal":{"kind":"not","predicate":{"kind":"or","predicates":[{"kind":"var_eq","resource":"x","value":[0]},{"kind":"var_eq","resource":"x","value":[1]}]}}}]}"#;
    assert_eq!(
        run_str(arr, goal, EngineKind::Interpreter).outcome,
        Outcome::Fail
    );
    assert_eq!(run_str(arr, goal, EngineKind::Petri).outcome, Outcome::Fail);

    // Struct containing a bounded array; a legal value still flows.
    let ok = r#"{
      "program": "mixed_ok",
      "modules": [{"name": "main",
        "resources": [
          {"name": "src", "kind": "var", "type": "Var", "base": {"Struct": {"xs": {"Array": {"elem": {"Int": [0,1]}, "len": 1}}}}, "init": {"xs": [1]}},
          {"name": "y", "kind": "var", "type": "Var", "base": {"Struct": {"xs": {"Array": {"elem": {"Int": [0,1]}, "len": 1}}}}, "init": {"xs": [0]}}
        ],
        "functions": [{"name": "main", "kind": "normal", "body": [{"sid":"s1","kind":"read_shared","resource":"src","dst":"y"},{"sid":"s2","kind":"return"}]}]
      }],
      "entry": "main::main"
    }"#;
    let reach = r#"{"name":"c","properties":[{"kind":"reachability","id":"ok","goal":{"kind":"var_eq","resource":"y","value":{"xs":[1]}}}]}"#;
    assert_eq!(
        run_str(ok, reach, EngineKind::Interpreter).outcome,
        Outcome::Pass
    );
    assert_eq!(run_str(ok, reach, EngineKind::Petri).outcome, Outcome::Pass);
}

// ── CLI categories ──────────────────────────────────────────────────

#[test]
fn c1_cli_reports_fail_for_the_false_pass() {
    let bin = env!("CARGO_BIN_EXE_concir-backend");
    let status = std::process::Command::new(bin)
        .args([
            "explore",
            "tests/repro_round4/r3_canonical_false_pass.json",
            "tests/repro_round4/r3_canonical_false_pass_contract.json",
        ])
        .output()
        .unwrap()
        .status
        .code()
        .unwrap_or(-1);
    assert_eq!(status, 1, "FAIL must be a non-zero exit");
}
