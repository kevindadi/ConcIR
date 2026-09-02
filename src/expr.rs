//! Parsed ConcIR expressions (JSON still stores them as strings).
//!
//! Grammar: `doc/syntax/dataflow.md`. Names resolve through [`NameEnv`].

use crate::ast::{BaseType, ComplexBaseType};
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

fn collect_names(expr: &Expr, env: &NameEnv, out: &mut Vec<String>) {
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

/// Parse `input` using `env` to classify identifiers (slot vs enum variant).
pub fn parse(input: &str, env: &NameEnv) -> Result<Expr, ParseError> {
    let mut p = Parser::new(input, env);
    let expr = p.parse_cmp()?;
    p.skip();
    if p.peek().is_some() {
        return Err(p.error(format!("trailing input after expression: '{}'", p.rest())));
    }
    Ok(expr)
}

/// Type of `expr` in `env`. `None` means Unknown (unmodeled activation slot).
pub fn type_of(expr: &Expr, env: &NameEnv) -> Result<Option<BaseType>, TypeError> {
    match expr {
        Expr::Lit(Lit::Bool(_)) => Ok(Some(BaseType::Primitive("Bool".into()))),
        Expr::Lit(Lit::Int(_)) => Ok(Some(BaseType::Primitive("Int".into()))),
        Expr::Lit(Lit::Float(_)) => Ok(Some(BaseType::Primitive("Float".into()))),
        Expr::Lit(Lit::String(_)) => Ok(Some(BaseType::Primitive("String".into()))),
        Expr::Lit(Lit::Enum(_)) => Ok(None), // unify at comparison / assignment
        Expr::Name(n) => type_of_name(n, env),
        Expr::Field { base, field } => {
            let Some(ty) = type_of(base, env)? else {
                return Ok(None);
            };
            match ty {
                BaseType::Complex(ComplexBaseType::Struct(fields)) => match fields.get(field) {
                    Some(ft) => Ok(Some(ft.clone())),
                    None => Err(TypeError {
                        code: "E933",
                        message: format!("struct has no field '{field}'"),
                    }),
                },
                other => Err(TypeError {
                    code: "E933",
                    message: format!("cannot project '.{field}' from type {other}"),
                }),
            }
        }
        Expr::UnaryNeg(inner) => {
            let ty = type_of(inner, env)?;
            match ty {
                None => Ok(None),
                Some(ref t) if is_int(t) => Ok(ty),
                Some(t) => Err(TypeError {
                    code: "E932",
                    message: format!("unary '-' requires Int, found {t}"),
                }),
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            let lt = type_of(lhs, env)?;
            let rt = type_of(rhs, env)?;
            match (lt, rt) {
                (None, _) | (_, None) => Ok(None),
                (Some(l), Some(r)) if is_int(&l) && is_int(&r) => {
                    Ok(Some(BaseType::Primitive("Int".into())))
                }
                (Some(l), Some(r)) if is_float(&l) && is_float(&r) => {
                    Ok(Some(BaseType::Primitive("Float".into())))
                }
                (Some(l), Some(r)) => Err(TypeError {
                    code: "E932",
                    message: format!(
                        "binary operator requires matching numeric types, found {l} and {r}"
                    ),
                }),
            }
        }
        Expr::Cmp { op, lhs, rhs } => {
            let lt = type_of(lhs, env)?;
            let rt = type_of(rhs, env)?;
            check_comparable(op, lt.as_ref(), rt.as_ref(), lhs, rhs)?;
            Ok(Some(BaseType::Primitive("Bool".into())))
        }
        Expr::Struct { fields } => {
            let mut map = std::collections::BTreeMap::new();
            for (name, val) in fields {
                let Some(ty) = type_of(val, env)? else {
                    continue;
                };
                map.insert(name.clone(), ty);
            }
            Ok(Some(BaseType::Complex(ComplexBaseType::Struct(map))))
        }
    }
}

fn type_of_name(n: &str, env: &NameEnv) -> Result<Option<BaseType>, TypeError> {
    let Some(slot) = env.get(n) else {
        return Err(TypeError {
            code: "E931",
            message: format!("undefined name '{n}' in expression"),
        });
    };
    match slot.kind {
        SlotKind::Discard => Err(TypeError {
            code: "E931",
            message: "\"_\" is not an r-value".into(),
        }),
        SlotKind::SyncResource => Err(TypeError {
            code: "E934",
            message: format!("'{n}' is a sync resource and cannot appear as a value"),
        }),
        SlotKind::Local | SlotKind::Param | SlotKind::Return => {
            if !slot.modeled {
                return Ok(None);
            }
            Ok(slot.ty.clone())
        }
        SlotKind::Var | SlotKind::Atomic => Ok(slot.ty.clone()),
    }
}

fn check_comparable(
    op: &CmpOp,
    lt: Option<&BaseType>,
    rt: Option<&BaseType>,
    lhs: &Expr,
    rhs: &Expr,
) -> Result<(), TypeError> {
    match (lt, rt) {
        (None, None) => Ok(()),
        (Some(l), None) => match rhs {
            Expr::Lit(Lit::Enum(_)) if enum_lit_ok(rhs, l) => Ok(()),
            Expr::Lit(Lit::Enum(v)) => Err(TypeError {
                code: "E932",
                message: format!("'{v}' is not a variant of {l}"),
            }),
            _ => Ok(()),
        },
        (None, Some(r)) => match lhs {
            Expr::Lit(Lit::Enum(_)) if enum_lit_ok(lhs, r) => Ok(()),
            Expr::Lit(Lit::Enum(v)) => Err(TypeError {
                code: "E932",
                message: format!("'{v}' is not a variant of {r}"),
            }),
            _ => Ok(()),
        },
        (Some(l), Some(r)) => {
            if enum_lit_ok(lhs, r) || enum_lit_ok(rhs, l) {
                return Ok(());
            }
            let ordered = matches!(op, CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge);
            if ordered {
                if is_int(l) && is_int(r) || is_float(l) && is_float(r) {
                    return Ok(());
                }
                return Err(TypeError {
                    code: "E932",
                    message: format!(
                        "ordered comparison requires numeric types, found {l} and {r}"
                    ),
                });
            }
            if types_eq(l, r) || is_int(l) && is_int(r) {
                return Ok(());
            }
            Err(TypeError {
                code: "E932",
                message: format!("comparison operands have types {l} and {r}"),
            })
        }
    }
}

fn enum_lit_ok(expr: &Expr, other: &BaseType) -> bool {
    match (expr, other) {
        (Expr::Lit(Lit::Enum(v)), BaseType::Complex(ComplexBaseType::Enum(vars))) => {
            vars.iter().any(|x| x == v)
        }
        _ => false,
    }
}

pub fn assignable(
    got: Option<&BaseType>,
    expected: &BaseType,
    expr: &Expr,
) -> Result<(), TypeError> {
    if got.is_none() {
        if let Expr::Lit(Lit::Enum(v)) = expr {
            if let BaseType::Complex(ComplexBaseType::Enum(vars)) = expected {
                if vars.iter().any(|x| x == v) {
                    return Ok(());
                }
                return Err(TypeError {
                    code: "E932",
                    message: format!("'{v}' is not a variant of {expected}"),
                });
            }
        }
        return Ok(());
    }
    let got = got.unwrap();
    if types_eq(got, expected) || is_int(got) && is_int(expected) {
        if let BaseType::Complex(ComplexBaseType::BoundedInt { lo, hi }) = expected {
            if let Expr::Lit(Lit::Int(v)) = expr {
                if v < lo || v > hi {
                    return Err(TypeError {
                        code: "E203",
                        message: format!("value {v} is outside the declared Int range {lo}..={hi}"),
                    });
                }
            }
        }
        return Ok(());
    }
    if let (
        BaseType::Complex(ComplexBaseType::Struct(g)),
        BaseType::Complex(ComplexBaseType::Struct(e)),
    ) = (got, expected)
    {
        for (k, et) in e {
            match g.get(k) {
                None => {
                    return Err(TypeError {
                        code: "E933",
                        message: format!("struct literal missing field '{k}'"),
                    });
                }
                Some(gt) if !types_eq(gt, et) && !(is_int(gt) && is_int(et)) => {
                    return Err(TypeError {
                        code: "E932",
                        message: format!("field '{k}' has type {gt}, expected {et}"),
                    });
                }
                _ => {}
            }
        }
        for k in g.keys() {
            if !e.contains_key(k) {
                return Err(TypeError {
                    code: "E933",
                    message: format!("struct literal has unknown field '{k}'"),
                });
            }
        }
        return Ok(());
    }
    Err(TypeError {
        code: "E932",
        message: format!("type mismatch: expected {expected}, found {got}"),
    })
}

fn is_int(ty: &BaseType) -> bool {
    match ty {
        BaseType::Primitive(p) if p == "Int" => true,
        BaseType::Complex(ComplexBaseType::BoundedInt { .. }) => true,
        _ => false,
    }
}

fn is_float(ty: &BaseType) -> bool {
    matches!(ty, BaseType::Primitive(p) if p == "Float")
}

fn types_eq(a: &BaseType, b: &BaseType) -> bool {
    a == b
}

// ── Parser ──────────────────────────────────────────────────────────────

struct Parser<'a> {
    src: &'a str,
    i: usize,
    env: &'a NameEnv,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, env: &'a NameEnv) -> Self {
        Self { src, i: 0, env }
    }

    fn rest(&self) -> &str {
        &self.src[self.i..]
    }

    fn error(&self, message: String) -> ParseError {
        ParseError { message }
    }

    fn skip(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.i += c.len_utf8();
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<char> {
        self.src[self.i..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += c.len_utf8();
        Some(c)
    }

    fn starts_with(&self, s: &str) -> bool {
        self.src[self.i..].starts_with(s)
    }

    fn eat(&mut self, s: &str) -> bool {
        self.skip();
        if self.starts_with(s) {
            self.i += s.len();
            true
        } else {
            false
        }
    }

    fn parse_cmp(&mut self) -> Result<Expr, ParseError> {
        let lhs = self.parse_add()?;
        self.skip();
        let op = if self.eat("==") {
            CmpOp::Eq
        } else if self.eat("!=") {
            CmpOp::Ne
        } else if self.eat("<=") {
            CmpOp::Le
        } else if self.eat(">=") {
            CmpOp::Ge
        } else if self.eat("<") {
            CmpOp::Lt
        } else if self.eat(">") {
            CmpOp::Gt
        } else {
            return Ok(lhs);
        };
        let rhs = self.parse_add()?;
        Ok(Expr::Cmp {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        })
    }

    fn parse_add(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_mul()?;
        loop {
            self.skip();
            let op = if self.eat("+") {
                BinOp::Add
            } else if self.peek() == Some('-') {
                // do not steal unary of next? infix minus
                self.bump();
                BinOp::Sub
            } else {
                break;
            };
            let rhs = self.parse_mul()?;
            lhs = Expr::BinOp {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_mul(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_unary()?;
        loop {
            self.skip();
            let op = if self.eat("*") {
                BinOp::Mul
            } else if self.eat("/") {
                BinOp::Div
            } else if self.eat("%") {
                BinOp::Mod
            } else {
                break;
            };
            let rhs = self.parse_unary()?;
            lhs = Expr::BinOp {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        self.skip();
        if self.eat("-") {
            let inner = self.parse_unary()?;
            return Ok(Expr::UnaryNeg(Box::new(inner)));
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_primary()?;
        loop {
            self.skip();
            if self.eat(".") {
                let field = self.parse_ident()?;
                expr = Expr::Field {
                    base: Box::new(expr),
                    field,
                };
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        self.skip();
        if self.eat("(") {
            let inner = self.parse_cmp()?;
            if !self.eat(")") {
                return Err(self.error("expected ')'".into()));
            }
            return Ok(inner);
        }
        if self.eat("{") {
            return self.parse_struct();
        }
        if let Some(c) = self.peek() {
            if c == '"' {
                return self.parse_string();
            }
            if c.is_ascii_digit() {
                return self.parse_number();
            }
            if c.is_ascii_alphabetic() || c == '_' {
                return self.parse_name_or_enum();
            }
        }
        Err(self.error(format!("expected expression, found '{}'", self.rest())))
    }

    fn parse_struct(&mut self) -> Result<Expr, ParseError> {
        let mut fields = Vec::new();
        self.skip();
        if self.eat("}") {
            return Ok(Expr::Struct { fields });
        }
        loop {
            let name = self.parse_ident()?;
            if !self.eat(":") {
                return Err(self.error(
                    "struct literals use named fields '{field: expr, ...}', not positional".into(),
                ));
            }
            let val = self.parse_cmp()?;
            fields.push((name, val));
            self.skip();
            if self.eat(",") {
                self.skip();
                if self.peek() == Some('}') {
                    self.eat("}");
                    break;
                }
                continue;
            }
            if self.eat("}") {
                break;
            }
            return Err(self.error("expected ',' or '}' in struct literal".into()));
        }
        Ok(Expr::Struct { fields })
    }

    fn parse_string(&mut self) -> Result<Expr, ParseError> {
        self.bump(); // "
        let start = self.i;
        while let Some(c) = self.peek() {
            if c == '"' {
                let s = self.src[start..self.i].to_string();
                self.bump();
                return Ok(Expr::Lit(Lit::String(s)));
            }
            self.bump();
        }
        Err(self.error("unterminated string literal".into()))
    }

    fn parse_number(&mut self) -> Result<Expr, ParseError> {
        let start = self.i;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.bump();
        }
        if self.peek() == Some('.') {
            let next = self.src[self.i + 1..].chars().next();
            if matches!(next, Some(c) if c.is_ascii_digit()) {
                self.bump();
                while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                    self.bump();
                }
                let s = &self.src[start..self.i];
                let f: f64 = s
                    .parse()
                    .map_err(|_| self.error(format!("invalid float '{s}'")))?;
                return Ok(Expr::Lit(Lit::Float(f)));
            }
        }
        let s = &self.src[start..self.i];
        let n: i64 = s
            .parse()
            .map_err(|_| self.error(format!("invalid integer '{s}'")))?;
        Ok(Expr::Lit(Lit::Int(n)))
    }

    fn parse_ident(&mut self) -> Result<String, ParseError> {
        self.skip();
        let start = self.i;
        match self.peek() {
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                self.bump();
            }
            _ => return Err(self.error("expected identifier".into())),
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == '_') {
            self.bump();
        }
        Ok(self.src[start..self.i].to_string())
    }

    fn parse_name_or_enum(&mut self) -> Result<Expr, ParseError> {
        let mut name = self.parse_ident()?;
        if self.eat("::") {
            let ent = self.parse_ident()?;
            name = format!("{name}::{ent}");
        }
        if self.env.get(&name).is_some() {
            return Ok(Expr::Name(name));
        }
        if name == "true" {
            return Ok(Expr::Lit(Lit::Bool(true)));
        }
        if name == "false" {
            return Ok(Expr::Lit(Lit::Bool(false)));
        }
        if self.env.enum_variants().contains(&name) {
            return Ok(Expr::Lit(Lit::Enum(name)));
        }
        Err(self.error(format!("undefined name '{name}'")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
