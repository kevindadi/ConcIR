use crate::env::NameEnv;

use super::{BinOp, CmpOp, Expr, Lit, ParseError};

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

