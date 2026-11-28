mod parser;
mod typecheck;

pub use parser::parse;
pub use typecheck::{assignable, type_of};


use crate::env::{NameEnv, SlotKind};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Enum(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit(Lit),
    Name(String),
    Field {
        base: Box<Expr>,
        field: String,
    },
    UnaryNeg(Box<Expr>),
    BinOp {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Cmp {
        op: CmpOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Struct {
        fields: Vec<(String, Expr)>,
    },
}

impl Expr {
    pub fn is_comparison(&self) -> bool {
        matches!(self, Expr::Cmp { .. })
    }

    /// Resource names (Var/Atomic) read by this expression.
    pub fn value_resource_names(&self, env: &NameEnv) -> Vec<String> {
        let mut out = Vec::new();
        collect_names(self, env, &mut out);
        out
    }
}

pub(crate) fn collect_names(expr: &Expr, env: &NameEnv, out: &mut Vec<String>) {
    match expr {
        Expr::Lit(_) => {}
        Expr::Name(n) => {
            if let Some(slot) = env.get(n) {
                if matches!(slot.kind, SlotKind::Var | SlotKind::Atomic) {
                    out.push(n.clone());
                }
            }
        }
        Expr::Field { base, .. } | Expr::UnaryNeg(base) => collect_names(base, env, out),
        Expr::BinOp { lhs, rhs, .. } | Expr::Cmp { lhs, rhs, .. } => {
            collect_names(lhs, env, out);
            collect_names(rhs, env, out);
        }
        Expr::Struct { fields } => {
            for (_, e) in fields {
                collect_names(e, env, out);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeError {
    pub code: &'static str,
    pub message: String,
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::BaseType;
    use crate::ast::Program;

    fn env() -> NameEnv {
        let program: Program = serde_json::from_str(
            r#"{
                "program": "p",
                "modules": [{
                    "name": "main",
                    "resources": [
                        {"name": "count", "kind": "var", "type": "Var", "base": "Int", "init": 0},
                        {"name": "pt", "kind": "var", "type": "Var",
                         "base": {"Struct": {"x": "Int", "ready": "Bool"}},
                         "init": {"x": 0, "ready": false}},
                        {"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}
                    ],
                    "functions": [{"name": "f", "kind": "normal", "body": []}]
                }],
                "entry": "main::f"
            }"#,
        )
        .unwrap();
        NameEnv::build(
            &program,
            &program.modules[0],
            &program.modules[0].functions[0],
        )
    }

    fn parse_ok(input: &str) -> Expr {
        parse(input, &env()).expect(input)
    }

    #[test]
    fn parses_arithmetic_and_comparison() {
        assert!(matches!(
            parse_ok("count + 1"),
            Expr::BinOp { op: BinOp::Add, .. }
        ));
        assert!(parse_ok("count > 0").is_comparison());
        assert!(matches!(parse_ok("-count"), Expr::UnaryNeg(_)));
        assert!(matches!(parse_ok("(count + 1) * 2"), Expr::BinOp { .. }));
    }

    #[test]
    fn parses_named_struct_and_field() {
        let e = parse_ok("{x: 1, ready: true}");
        assert!(matches!(e, Expr::Struct { .. }));
        assert!(matches!(parse_ok("pt.ready"), Expr::Field { .. }));
    }

    #[test]
    fn rejects_positional_struct_and_unknown_name() {
        assert!(parse("{1, 2}", &env()).is_err());
        assert!(parse("nope + 1", &env()).is_err());
        assert!(parse("mtx", &env()).is_ok()); // name exists; type_of is E934
    }

    #[test]
    fn types_int_arith_and_struct_field() {
        let e = env();
        let add = parse("count + 1", &e).unwrap();
        assert_eq!(
            type_of(&add, &e).unwrap(),
            Some(BaseType::Primitive("Int".into()))
        );
        let field = parse("pt.ready", &e).unwrap();
        assert_eq!(
            type_of(&field, &e).unwrap(),
            Some(BaseType::Primitive("Bool".into()))
        );
        let mtx = parse("mtx", &e).unwrap();
        assert_eq!(type_of(&mtx, &e).unwrap_err().code, "E934");
    }

    #[test]
    fn assignable_named_struct_and_bounded_int() {
        let e = env();
        let lit = parse("{x: 1, ready: true}", &e).unwrap();
        let got = type_of(&lit, &e).unwrap();
        let expected = e.ty("pt").unwrap();
        assert!(assignable(got.as_ref(), expected, &lit).is_ok());

        let positional = parse("{size: 100, ready: true}", &e).unwrap();
        let got = type_of(&positional, &e).unwrap();
        assert_eq!(
            assignable(got.as_ref(), expected, &positional)
                .unwrap_err()
                .code,
            "E933"
        );
    }
}
