use crate::ast::*;
use crate::span::Span;
use crate::token::{Token, TokenKind};

#[derive(Debug)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "parse error at offset {}: {}", self.span.offset, self.message)
    }
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    pub fn parse_program(&mut self) -> Result<Program, ParseError> {
        let start = self.cur_span();
        self.expect(TokenKind::Program)?;
        let name = self.expect_ident()?;
        self.expect(TokenKind::LBrace)?;

        let resources = self.parse_resources_block()?;
        let protections = self.parse_protection_block()?;

        let mut functions = Vec::new();
        let mut fn_summaries = Vec::new();

        loop {
            if self.check(TokenKind::Fn) {
                functions.push(self.parse_function()?);
            } else if self.check(TokenKind::FnSummary) {
                fn_summaries.push(self.parse_fn_summary()?);
            } else {
                break;
            }
        }

        self.expect(TokenKind::Entry)?;
        self.expect(TokenKind::Colon)?;
        let entry = self.expect_ident()?;
        self.expect(TokenKind::Semicolon)?;

        let end = self.cur_span();
        self.expect(TokenKind::RBrace)?;

        Ok(Program {
            name,
            resources,
            protections,
            functions,
            fn_summaries,
            entry,
            span: start.merge(end),
        })
    }

    // ──────────────────── Resources ────────────────────

    fn parse_resources_block(&mut self) -> Result<Vec<ResourceDecl>, ParseError> {
        self.expect(TokenKind::Resources)?;
        self.expect(TokenKind::LBrace)?;
        let mut decls = Vec::new();
        while !self.check(TokenKind::RBrace) && !self.check(TokenKind::Eof) {
            decls.push(self.parse_resource_decl()?);
        }
        self.expect(TokenKind::RBrace)?;
        Ok(decls)
    }

    fn parse_resource_decl(&mut self) -> Result<ResourceDecl, ParseError> {
        let start = self.cur_span();
        if self.check(TokenKind::Sync) {
            self.advance();
            let name = self.expect_ident()?;
            self.expect(TokenKind::Colon)?;
            let sync_type = self.parse_sync_type()?;
            self.expect(TokenKind::Semicolon)?;
            Ok(ResourceDecl {
                span: start.merge(self.prev_span()),
                name,
                kind: ResourceKind::Sync(sync_type),
            })
        } else if self.check(TokenKind::Var) {
            self.advance();
            let name = self.expect_ident()?;
            self.expect(TokenKind::Colon)?;
            let var_type = self.parse_var_type()?;
            self.expect(TokenKind::Eq)?;
            let _init = self.parse_literal()?;
            self.expect(TokenKind::Semicolon)?;
            Ok(ResourceDecl {
                span: start.merge(self.prev_span()),
                name,
                kind: ResourceKind::Var(var_type),
            })
        } else {
            Err(self.error("expected 'sync' or 'var' in resource declaration"))
        }
    }

    fn parse_sync_type(&mut self) -> Result<SyncType, ParseError> {
        if self.check(TokenKind::Mutex) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let mode = self.parse_mode()?;
            self.expect(TokenKind::RParen)?;
            Ok(SyncType::Mutex(mode))
        } else if self.check(TokenKind::RwLock) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let mode = self.parse_mode()?;
            self.expect(TokenKind::RParen)?;
            Ok(SyncType::RwLock(mode))
        } else if self.check(TokenKind::Condvar) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let mode = self.parse_mode()?;
            self.expect(TokenKind::RParen)?;
            Ok(SyncType::Condvar(mode))
        } else if self.check(TokenKind::Semaphore) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let mode = self.parse_mode()?;
            self.expect(TokenKind::Comma)?;
            let count = self.expect_int()?;
            self.expect(TokenKind::RParen)?;
            Ok(SyncType::Semaphore(mode, count))
        } else if self.check(TokenKind::Channel) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let mode = self.parse_mode()?;
            self.expect(TokenKind::Comma)?;
            let base = self.parse_base_type()?;
            self.expect(TokenKind::RParen)?;
            Ok(SyncType::Channel(mode, base))
        } else {
            Err(self.error("expected sync type (Mutex, RwLock, Condvar, Semaphore, Channel)"))
        }
    }

    fn parse_var_type(&mut self) -> Result<VarType, ParseError> {
        if self.check(TokenKind::Var) {
            self.advance();
            self.expect(TokenKind::Lt)?;
            let base = self.parse_base_type()?;
            self.expect(TokenKind::Gt)?;
            Ok(VarType::Var(base))
        } else if self.check(TokenKind::Atomic) {
            self.advance();
            self.expect(TokenKind::Lt)?;
            let base = self.parse_base_type()?;
            self.expect(TokenKind::Gt)?;
            Ok(VarType::Atomic(base))
        } else {
            Err(self.error("expected var type (Var or Atomic)"))
        }
    }

    fn parse_mode(&mut self) -> Result<Mode, ParseError> {
        if self.check(TokenKind::SyncMode) {
            self.advance();
            Ok(Mode::Sync)
        } else if self.check(TokenKind::Async) {
            self.advance();
            Ok(Mode::Async)
        } else {
            Err(self.error("expected mode (Sync or Async)"))
        }
    }

    fn parse_base_type(&mut self) -> Result<BaseType, ParseError> {
        if self.check(TokenKind::BoolType) {
            self.advance();
            Ok(BaseType::Bool)
        } else if self.check(TokenKind::IntType) {
            self.advance();
            Ok(BaseType::Int)
        } else if self.check(TokenKind::FloatType) {
            self.advance();
            Ok(BaseType::Float)
        } else if self.check(TokenKind::StringType) {
            self.advance();
            Ok(BaseType::String)
        } else if self.check(TokenKind::Enum) {
            self.advance();
            self.expect(TokenKind::LBrace)?;
            let mut variants = Vec::new();
            variants.push(self.expect_ident_str()?);
            while self.check(TokenKind::Comma) {
                self.advance();
                if self.check(TokenKind::RBrace) {
                    break;
                }
                variants.push(self.expect_ident_str()?);
            }
            self.expect(TokenKind::RBrace)?;
            Ok(BaseType::Enum(variants))
        } else if self.check(TokenKind::Struct) {
            self.advance();
            self.expect(TokenKind::LBrace)?;
            let mut fields = Vec::new();
            fields.push(self.parse_field_decl()?);
            while self.check(TokenKind::Comma) {
                self.advance();
                if self.check(TokenKind::RBrace) {
                    break;
                }
                fields.push(self.parse_field_decl()?);
            }
            self.expect(TokenKind::RBrace)?;
            Ok(BaseType::Struct(fields))
        } else if self.check(TokenKind::Array) {
            self.advance();
            self.expect(TokenKind::Lt)?;
            let elem = self.parse_base_type()?;
            self.expect(TokenKind::Comma)?;
            let size = self.expect_int()?;
            self.expect(TokenKind::Gt)?;
            Ok(BaseType::Array(Box::new(elem), size))
        } else {
            Err(self.error("expected base type (Bool, Int, Float, String, Enum, Struct, Array)"))
        }
    }

    fn parse_field_decl(&mut self) -> Result<FieldDecl, ParseError> {
        let name = self.expect_ident_str()?;
        self.expect(TokenKind::Colon)?;
        let ty = self.parse_base_type()?;
        Ok(FieldDecl { name, ty })
    }

    // ──────────────────── Protection ────────────────────

    fn parse_protection_block(&mut self) -> Result<Vec<ProtectionDecl>, ParseError> {
        self.expect(TokenKind::Protection)?;
        self.expect(TokenKind::LBrace)?;
        let mut decls = Vec::new();
        while !self.check(TokenKind::RBrace) && !self.check(TokenKind::Eof) {
            let start = self.cur_span();
            let var_name = self.expect_ident()?;
            self.expect(TokenKind::Arrow)?;
            let lock_name = self.expect_ident()?;
            self.expect(TokenKind::Semicolon)?;
            decls.push(ProtectionDecl {
                span: start.merge(self.prev_span()),
                var_name,
                lock_name,
            });
        }
        self.expect(TokenKind::RBrace)?;
        Ok(decls)
    }

    // ──────────────────── Function ────────────────────

    fn parse_function(&mut self) -> Result<Function, ParseError> {
        let start = self.cur_span();
        self.expect(TokenKind::Fn)?;
        let kind = self.parse_fn_kind()?;
        let name = self.expect_ident()?;
        self.expect(TokenKind::LBrace)?;

        let mut statements = Vec::new();
        while !self.check(TokenKind::RBrace) && !self.check(TokenKind::Eof) {
            statements.push(self.parse_statement()?);
        }
        self.expect(TokenKind::RBrace)?;

        Ok(Function {
            kind,
            name,
            statements,
            span: start.merge(self.prev_span()),
        })
    }

    fn parse_fn_kind(&mut self) -> Result<FnKind, ParseError> {
        if self.check(TokenKind::Normal) {
            self.advance();
            Ok(FnKind::Normal)
        } else if self.check(TokenKind::Async) {
            self.advance();
            Ok(FnKind::Async)
        } else if self.check(TokenKind::Closure) {
            self.advance();
            Ok(FnKind::Closure)
        } else {
            Err(self.error("expected fn kind (normal, async, closure)"))
        }
    }

    fn parse_statement(&mut self) -> Result<Statement, ParseError> {
        let start = self.cur_span();
        let sid = self.expect_ident()?;
        self.expect(TokenKind::Colon)?;
        let op = self.parse_op()?;
        self.expect(TokenKind::FatArrow)?;
        let transfer = self.parse_transfer()?;
        self.expect(TokenKind::Semicolon)?;
        Ok(Statement {
            sid,
            op,
            transfer,
            span: start.merge(self.prev_span()),
        })
    }

    // ──────────────────── Operations ────────────────────

    fn parse_op(&mut self) -> Result<Op, ParseError> {
        if self.check(TokenKind::ResOp) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let resource = self.expect_ident()?;
            self.expect(TokenKind::Comma)?;
            let action = self.parse_action()?;
            self.expect(TokenKind::RParen)?;
            Ok(Op::ResOp(resource, action))
        } else if self.check(TokenKind::Spawn) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let name = self.expect_ident()?;
            self.expect(TokenKind::RParen)?;
            Ok(Op::Spawn(name))
        } else if self.check(TokenKind::SpawnAsync) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let name = self.expect_ident()?;
            self.expect(TokenKind::RParen)?;
            Ok(Op::SpawnAsync(name))
        } else if self.check(TokenKind::Join) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let name = self.expect_ident()?;
            self.expect(TokenKind::RParen)?;
            Ok(Op::Join(name))
        } else if self.check(TokenKind::Await) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let name = self.expect_ident()?;
            self.expect(TokenKind::RParen)?;
            Ok(Op::Await(name))
        } else if self.check(TokenKind::Call) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let name = self.expect_ident()?;
            self.expect(TokenKind::RParen)?;
            Ok(Op::Call(name))
        } else if self.check(TokenKind::Return) {
            self.advance();
            Ok(Op::Return)
        } else {
            Err(self.error(
                "expected operation (res_op, spawn, spawn_async, join, await, call, return)",
            ))
        }
    }

    fn parse_action(&mut self) -> Result<Action, ParseError> {
        if self.check(TokenKind::Lock) {
            self.advance();
            Ok(Action::Lock)
        } else if self.check(TokenKind::Read) {
            self.advance();
            Ok(Action::Read)
        } else if self.check(TokenKind::Write) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let expr = self.parse_expr()?;
            self.expect(TokenKind::RParen)?;
            Ok(Action::Write(expr))
        } else if self.check(TokenKind::Drop) {
            self.advance();
            Ok(Action::Drop)
        } else if self.check(TokenKind::Wait) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let lock = self.expect_ident()?;
            self.expect(TokenKind::RParen)?;
            Ok(Action::Wait(lock))
        } else if self.check(TokenKind::Notify) {
            self.advance();
            Ok(Action::Notify)
        } else if self.check(TokenKind::NotifyAll) {
            self.advance();
            Ok(Action::NotifyAll)
        } else if self.check(TokenKind::Acquire) {
            self.advance();
            Ok(Action::Acquire)
        } else if self.check(TokenKind::Release) {
            self.advance();
            Ok(Action::Release)
        } else if self.check(TokenKind::Send) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let expr = self.parse_expr()?;
            self.expect(TokenKind::RParen)?;
            Ok(Action::Send(expr))
        } else if self.check(TokenKind::Recv) {
            self.advance();
            Ok(Action::Recv)
        } else if self.check(TokenKind::Load) {
            self.advance();
            Ok(Action::Load)
        } else if self.check(TokenKind::Store) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let expr = self.parse_expr()?;
            self.expect(TokenKind::RParen)?;
            Ok(Action::Store(expr))
        } else if self.check(TokenKind::Cas) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let expected = self.parse_expr()?;
            self.expect(TokenKind::Comma)?;
            let desired = self.parse_expr()?;
            self.expect(TokenKind::RParen)?;
            Ok(Action::Cas(expected, desired))
        } else {
            Err(self.error("expected action (lock, read, write, drop, wait, notify, ...)"))
        }
    }

    // ──────────────────── Transfer ────────────────────

    fn parse_transfer(&mut self) -> Result<Transfer, ParseError> {
        if self.check(TokenKind::Next) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let target = self.expect_ident()?;
            self.expect(TokenKind::RParen)?;
            Ok(Transfer::Next(target))
        } else if self.check(TokenKind::Return) {
            self.advance();
            Ok(Transfer::Return)
        } else if self.check(TokenKind::Branch) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let cond = self.parse_cond_expr()?;
            self.expect(TokenKind::Comma)?;
            let true_target = self.expect_ident()?;
            self.expect(TokenKind::Comma)?;
            let false_target = self.expect_ident()?;
            self.expect(TokenKind::RParen)?;
            Ok(Transfer::Branch(cond, true_target, false_target))
        } else if self.check(TokenKind::Switch) {
            self.advance();
            self.expect(TokenKind::LParen)?;
            let var = self.expect_ident()?;
            self.expect(TokenKind::Comma)?;
            self.expect(TokenKind::LBracket)?;
            let mut cases = Vec::new();
            cases.push(self.parse_case()?);
            while self.check(TokenKind::Comma) {
                self.advance();
                if self.check(TokenKind::RBracket) {
                    break;
                }
                cases.push(self.parse_case()?);
            }
            self.expect(TokenKind::RBracket)?;
            self.expect(TokenKind::RParen)?;
            Ok(Transfer::Switch(var, cases))
        } else {
            Err(self.error("expected transfer (next, return, branch, switch)"))
        }
    }

    fn parse_cond_expr(&mut self) -> Result<CondExpr, ParseError> {
        let start = self.cur_span();
        let lhs = self.parse_expr()?;
        let op = self.parse_cmp_op()?;
        let rhs = self.parse_expr()?;
        Ok(CondExpr {
            span: start.merge(self.prev_span()),
            lhs,
            op,
            rhs,
        })
    }

    fn parse_cmp_op(&mut self) -> Result<CmpOp, ParseError> {
        let kind = self.current().kind.clone();
        match kind {
            TokenKind::EqEq => { self.advance(); Ok(CmpOp::Eq) }
            TokenKind::Ne => { self.advance(); Ok(CmpOp::Ne) }
            TokenKind::Gt => { self.advance(); Ok(CmpOp::Gt) }
            TokenKind::Lt => { self.advance(); Ok(CmpOp::Lt) }
            TokenKind::Ge => { self.advance(); Ok(CmpOp::Ge) }
            TokenKind::Le => { self.advance(); Ok(CmpOp::Le) }
            _ => Err(self.error("expected comparison operator (==, !=, >, <, >=, <=)")),
        }
    }

    fn parse_case(&mut self) -> Result<Case, ParseError> {
        let label = self.parse_literal()?;
        self.expect(TokenKind::FatArrow)?;
        let target = self.expect_ident()?;
        Ok(Case { label, target })
    }

    // ──────────────────── Expressions (precedence climbing) ────────────────────

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_expr_bp(0)
    }

    /// Precedence climbing: higher `min_bp` binds tighter.
    /// +/- → bp 1, *// /% → bp 2
    fn parse_expr_bp(&mut self, min_bp: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_expr_atom()?;

        loop {
            let (op, bp) = match self.current().kind {
                TokenKind::Plus => (ArithOp::Add, 1),
                TokenKind::Minus => (ArithOp::Sub, 1),
                TokenKind::Star => (ArithOp::Mul, 2),
                TokenKind::Slash => (ArithOp::Div, 2),
                TokenKind::Percent => (ArithOp::Mod, 2),
                _ => break,
            };
            if bp < min_bp {
                break;
            }
            self.advance();
            let rhs = self.parse_expr_bp(bp + 1)?;
            lhs = Expr::BinOp(Box::new(lhs), op, Box::new(rhs));
        }

        Ok(lhs)
    }

    fn parse_expr_atom(&mut self) -> Result<Expr, ParseError> {
        // Unary minus
        if self.check(TokenKind::Minus) {
            self.advance();
            let inner = self.parse_expr_atom()?;
            return Ok(Expr::UnaryMinus(Box::new(inner)));
        }

        // Parenthesized expression
        if self.check(TokenKind::LParen) {
            self.advance();
            let inner = self.parse_expr()?;
            self.expect(TokenKind::RParen)?;
            return Ok(Expr::Paren(Box::new(inner)));
        }

        // Literals (int, float, bool, string, compound)
        if self.is_literal() {
            let lit = self.parse_literal()?;
            return Ok(Expr::Literal(lit));
        }

        // Identifier
        if matches!(self.current().kind, TokenKind::Ident(_)) {
            let ident = self.expect_ident()?;
            return Ok(Expr::Ident(ident));
        }

        Err(self.error("expected expression"))
    }

    fn is_literal(&self) -> bool {
        matches!(
            self.current().kind,
            TokenKind::IntLit(_)
                | TokenKind::FloatLit(_)
                | TokenKind::BoolLit(_)
                | TokenKind::StringLit(_)
                | TokenKind::LBrace
        )
    }

    fn parse_literal(&mut self) -> Result<Literal, ParseError> {
        let kind = self.current().kind.clone();
        match kind {
            TokenKind::IntLit(v) => { self.advance(); Ok(Literal::Int(v)) }
            TokenKind::FloatLit(v) => { self.advance(); Ok(Literal::Float(v)) }
            TokenKind::BoolLit(v) => { self.advance(); Ok(Literal::Bool(v)) }
            TokenKind::StringLit(v) => { self.advance(); Ok(Literal::String(v)) }
            TokenKind::LBrace => {
                self.advance();
                let mut elements = Vec::new();
                if !self.check(TokenKind::RBrace) {
                    elements.push(self.parse_literal()?);
                    while self.check(TokenKind::Comma) {
                        self.advance();
                        if self.check(TokenKind::RBrace) { break; }
                        elements.push(self.parse_literal()?);
                    }
                }
                self.expect(TokenKind::RBrace)?;
                Ok(Literal::Compound(elements))
            }
            TokenKind::Ident(name) => {
                self.advance();
                Ok(Literal::Ident(name))
            }
            _ => Err(self.error("expected literal value")),
        }
    }

    // ──────────────────── FnSummary ────────────────────

    fn parse_fn_summary(&mut self) -> Result<FnSummary, ParseError> {
        let start = self.cur_span();
        self.expect(TokenKind::FnSummary)?;
        let name = self.expect_ident()?;
        self.expect(TokenKind::LBrace)?;

        self.expect(TokenKind::Reads)?;
        self.expect(TokenKind::Colon)?;
        let reads = self.parse_ident_list()?;
        self.expect(TokenKind::Semicolon)?;

        self.expect(TokenKind::Writes)?;
        self.expect(TokenKind::Colon)?;
        let writes = self.parse_ident_list()?;
        self.expect(TokenKind::Semicolon)?;

        self.expect(TokenKind::Callees)?;
        self.expect(TokenKind::Colon)?;
        let callees = self.parse_ident_list()?;
        self.expect(TokenKind::Semicolon)?;

        self.expect(TokenKind::HasConcurrency)?;
        self.expect(TokenKind::Colon)?;
        let has_concurrency = self.expect_bool()?;
        self.expect(TokenKind::Semicolon)?;

        self.expect(TokenKind::RBrace)?;
        Ok(FnSummary {
            name,
            reads,
            writes,
            callees,
            has_concurrency,
            span: start.merge(self.prev_span()),
        })
    }

    fn parse_ident_list(&mut self) -> Result<Vec<Spanned<String>>, ParseError> {
        self.expect(TokenKind::LBracket)?;
        let mut items = Vec::new();
        if !self.check(TokenKind::RBracket) {
            items.push(self.expect_ident()?);
            while self.check(TokenKind::Comma) {
                self.advance();
                if self.check(TokenKind::RBracket) { break; }
                items.push(self.expect_ident()?);
            }
        }
        self.expect(TokenKind::RBracket)?;
        Ok(items)
    }

    // ──────────────────── Helpers ────────────────────

    fn current(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn cur_span(&self) -> Span {
        self.current().span
    }

    fn prev_span(&self) -> Span {
        if self.pos > 0 {
            self.tokens[self.pos - 1].span
        } else {
            Span::DUMMY
        }
    }

    fn check(&self, kind: TokenKind) -> bool {
        std::mem::discriminant(&self.current().kind) == std::mem::discriminant(&kind)
    }

    fn advance(&mut self) {
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
    }

    fn expect(&mut self, kind: TokenKind) -> Result<(), ParseError> {
        if self.check(kind.clone()) {
            self.advance();
            Ok(())
        } else {
            Err(self.error(&format!("expected {:?}, found {:?}", kind, self.current().kind)))
        }
    }

    fn expect_ident(&mut self) -> Result<Spanned<String>, ParseError> {
        let span = self.cur_span();
        match &self.current().kind {
            TokenKind::Ident(name) => {
                let name = name.clone();
                self.advance();
                Ok(Spanned::new(name, span))
            }
            _ => Err(self.error(&format!("expected identifier, found {:?}", self.current().kind))),
        }
    }

    fn expect_ident_str(&mut self) -> Result<String, ParseError> {
        Ok(self.expect_ident()?.value)
    }

    fn expect_int(&mut self) -> Result<i64, ParseError> {
        match &self.current().kind {
            TokenKind::IntLit(v) => {
                let v = *v;
                self.advance();
                Ok(v)
            }
            _ => Err(self.error("expected integer literal")),
        }
    }

    fn expect_bool(&mut self) -> Result<bool, ParseError> {
        match &self.current().kind {
            TokenKind::BoolLit(v) => {
                let v = *v;
                self.advance();
                Ok(v)
            }
            _ => Err(self.error("expected boolean literal")),
        }
    }

    fn error(&self, msg: &str) -> ParseError {
        ParseError {
            message: msg.to_string(),
            span: self.cur_span(),
        }
    }
}
