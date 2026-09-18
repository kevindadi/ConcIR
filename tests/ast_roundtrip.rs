//! E1: complex-type serialization round-trips against the input schema.

use concir::ast::{ArrayDef, BaseType, ComplexBaseType, Program};
use std::collections::BTreeMap;

const MODEL: &str = r#"{
  "program": "complex_types",
  "version": "3.5.0",
  "entry": "main::main",
  "modules": [{
    "name": "main",
    "resources": [
      {"name": "b", "kind": "var", "type": "Var", "base": "Bool", "init": true},
      {"name": "i", "kind": "var", "type": "Var", "base": "Int", "init": 0},
      {"name": "bi", "kind": "var", "type": "Var", "base": {"Int": [0, 5]}, "init": 2},
      {"name": "f", "kind": "var", "type": "Var", "base": "Float", "init": 0.0},
      {"name": "s", "kind": "var", "type": "Var", "base": "String", "init": ""},
      {"name": "e", "kind": "var", "type": "Var", "base": {"Enum": ["A", "B"]}, "init": "A"},
      {"name": "st", "kind": "var", "type": "Var", "base": {"Struct": {"x": "Int", "y": "Bool"}}, "init": {"x": 0, "y": false}},
      {"name": "ar", "kind": "var", "type": "Var", "base": {"Array": {"elem": "Int", "len": 2}}, "init": [0, 0]},
      {"name": "nested", "kind": "var", "type": "Var", "base": {"Struct": {"xs": {"Array": {"elem": {"Int": [0,1]}, "len": 2}}}}, "init": {"xs": [0, 1]}}
    ],
    "protection": [],
    "functions": [
      {"name": "main", "kind": "normal",
       "params": [{"name": "p", "type": {"Int": [0, 3]}, "modeled": true}],
       "returns": {"name": "r", "type": {"Enum": ["A", "B"]}, "modeled": true},
       "locals": [{"name": "l", "type": {"Array": {"elem": {"Int": [0,1]}, "len": 1}}},
                  {"name": "m", "type": {"Struct": {"k": {"Enum": ["X"]}}}}],
       "body": [{"sid": "s1", "kind": "return", "value": "A"}]}
    ]
  }]
}"#;

#[test]
fn program_with_all_complex_types_round_trips() {
    let p1: Program = serde_json::from_str(MODEL).expect("parse model");
    let json = serde_json::to_string(&p1).expect("serialize program");
    // The bounded Int must be a single-key object, matching the input schema.
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let base = &v["modules"][0]["resources"][2]["base"];
    assert_eq!(
        base,
        &serde_json::json!({"Int": [0, 5]}),
        "bounded Int schema"
    );
    assert_eq!(
        &v["modules"][0]["resources"][6]["base"],
        &serde_json::json!({"Struct": {"x": "Int", "y": "Bool"}})
    );
    assert_eq!(
        &v["modules"][0]["resources"][7]["base"],
        &serde_json::json!({"Array": {"elem": "Int", "len": 2}})
    );
    // A nested Struct<Array<bounded Int>> survives.
    assert_eq!(
        &v["modules"][0]["resources"][8]["base"],
        &serde_json::json!({"Struct": {"xs": {"Array": {"elem": {"Int": [0,1]}, "len": 2}}}})
    );

    let p2: Program = serde_json::from_str(&json).expect("re-parse serialized program");
    assert_eq!(
        serde_json::to_value(&p1).unwrap(),
        serde_json::to_value(&p2).unwrap(),
        "Program round-trip must be value-preserving"
    );
}

#[test]
fn complex_base_type_round_trips_directly() {
    let types = vec![
        ComplexBaseType::BoundedInt { lo: -3, hi: 7 },
        ComplexBaseType::Enum(vec!["A".into(), "B".into()]),
        ComplexBaseType::Struct(BTreeMap::from([
            ("x".to_string(), BaseType::Primitive("Int".into())),
            (
                "n".to_string(),
                BaseType::Complex(ComplexBaseType::Array(Box::new(ArrayDef {
                    elem: BaseType::Complex(ComplexBaseType::BoundedInt { lo: 0, hi: 1 }),
                    len: 3,
                }))),
            ),
        ])),
        ComplexBaseType::Array(Box::new(ArrayDef {
            elem: BaseType::Complex(ComplexBaseType::Enum(vec!["E".into()])),
            len: 2,
        })),
    ];
    for ty in types {
        let s = serde_json::to_string(&ty).unwrap();
        let back: ComplexBaseType = serde_json::from_str(&s).unwrap();
        assert_eq!(ty, back, "type {s} did not round-trip");
    }
}
