use crate::span::Span;
use crate::token::{Token, TokenKind};

#[derive(Debug)]
pub struct LexError {
    pub message: String,
    pub span: Span,
}

pub struct Lexer<'src> {
    source: &'src [u8],
    pos: usize,
}

impl<'src> Lexer<'src> {
    pub fn new(source: &'src str) -> Self {
        Self {
            source: source.as_bytes(),
            pos: 0,
        }
    }

    pub fn tokenize(&mut self) -> Result<Vec<Token>, LexError> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace_and_comments();
            if self.pos >= self.source.len() {
                tokens.push(Token {
                    kind: TokenKind::Eof,
                    span: Span::new(self.pos, 0),
                });
                break;
            }
            tokens.push(self.next_token()?);
        }
        Ok(tokens)
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            while self.pos < self.source.len() && self.source[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            // # line comments
            if self.pos < self.source.len() && self.source[self.pos] == b'#' {
                while self.pos < self.source.len() && self.source[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            // // line comments (legacy)
            if self.pos + 1 < self.source.len()
                && self.source[self.pos] == b'/'
                && self.source[self.pos + 1] == b'/'
            {
                while self.pos < self.source.len() && self.source[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            // /* */ block comments (legacy)
            if self.pos + 1 < self.source.len()
                && self.source[self.pos] == b'/'
                && self.source[self.pos + 1] == b'*'
            {
                self.pos += 2;
                while self.pos + 1 < self.source.len()
                    && !(self.source[self.pos] == b'*' && self.source[self.pos + 1] == b'/')
                {
                    self.pos += 1;
                }
                if self.pos + 1 < self.source.len() {
                    self.pos += 2;
                }
                continue;
            }
            break;
        }
    }

    fn peek_at(&self, offset: usize) -> u8 {
        let idx = self.pos + offset;
        if idx < self.source.len() {
            self.source[idx]
        } else {
            0
        }
    }

    fn next_token(&mut self) -> Result<Token, LexError> {
        let start = self.pos;
        let ch = self.source[self.pos];

        match ch {
            b'{' => { self.pos += 1; Ok(self.tok(TokenKind::LBrace, start)) }
            b'}' => { self.pos += 1; Ok(self.tok(TokenKind::RBrace, start)) }
            b'(' => { self.pos += 1; Ok(self.tok(TokenKind::LParen, start)) }
            b')' => { self.pos += 1; Ok(self.tok(TokenKind::RParen, start)) }
            b'[' => { self.pos += 1; Ok(self.tok(TokenKind::LBracket, start)) }
            b']' => { self.pos += 1; Ok(self.tok(TokenKind::RBracket, start)) }
            b':' => { self.pos += 1; Ok(self.tok(TokenKind::Colon, start)) }
            b';' => { self.pos += 1; Ok(self.tok(TokenKind::Semicolon, start)) }
            b',' => { self.pos += 1; Ok(self.tok(TokenKind::Comma, start)) }
            b'+' => { self.pos += 1; Ok(self.tok(TokenKind::Plus, start)) }
            b'*' => { self.pos += 1; Ok(self.tok(TokenKind::Star, start)) }
            b'%' => { self.pos += 1; Ok(self.tok(TokenKind::Percent, start)) }

            b'-' => {
                if self.peek_at(1) == b'>' {
                    self.pos += 2;
                    Ok(self.tok(TokenKind::Arrow, start))
                } else {
                    self.pos += 1;
                    Ok(self.tok(TokenKind::Minus, start))
                }
            }

            b'=' => {
                if self.peek_at(1) == b'>' {
                    self.pos += 2;
                    Ok(self.tok(TokenKind::FatArrow, start))
                } else if self.peek_at(1) == b'=' {
                    self.pos += 2;
                    Ok(self.tok(TokenKind::EqEq, start))
                } else {
                    self.pos += 1;
                    Ok(self.tok(TokenKind::Eq, start))
                }
            }

            b'!' => {
                if self.peek_at(1) == b'=' {
                    self.pos += 2;
                    Ok(self.tok(TokenKind::Ne, start))
                } else {
                    Err(LexError {
                        message: "unexpected '!', expected '!='".into(),
                        span: Span::new(start, 1),
                    })
                }
            }

            b'>' => {
                if self.peek_at(1) == b'=' {
                    self.pos += 2;
                    Ok(self.tok(TokenKind::Ge, start))
                } else {
                    self.pos += 1;
                    Ok(self.tok(TokenKind::Gt, start))
                }
            }

            b'<' => {
                if self.peek_at(1) == b'=' {
                    self.pos += 2;
                    Ok(self.tok(TokenKind::Le, start))
                } else {
                    self.pos += 1;
                    Ok(self.tok(TokenKind::Lt, start))
                }
            }

            b'/' => {
                self.pos += 1;
                Ok(self.tok(TokenKind::Slash, start))
            }

            b'"' => self.lex_string(start),

            b'0'..=b'9' => self.lex_number(start),

            b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.lex_ident_or_keyword(start),

            _ => Err(LexError {
                message: format!("unexpected character '{}'", ch as char),
                span: Span::new(start, 1),
            }),
        }
    }

    fn tok(&self, kind: TokenKind, start: usize) -> Token {
        Token {
            kind,
            span: Span::new(start, self.pos - start),
        }
    }

    fn lex_string(&mut self, start: usize) -> Result<Token, LexError> {
        self.pos += 1; // skip opening "
        let mut value = String::new();
        while self.pos < self.source.len() && self.source[self.pos] != b'"' {
            if self.source[self.pos] == b'\\' {
                self.pos += 1;
                if self.pos >= self.source.len() {
                    return Err(LexError {
                        message: "unterminated string escape".into(),
                        span: Span::new(start, self.pos - start),
                    });
                }
                match self.source[self.pos] {
                    b'n' => value.push('\n'),
                    b't' => value.push('\t'),
                    b'\\' => value.push('\\'),
                    b'"' => value.push('"'),
                    other => {
                        value.push('\\');
                        value.push(other as char);
                    }
                }
            } else {
                value.push(self.source[self.pos] as char);
            }
            self.pos += 1;
        }
        if self.pos >= self.source.len() {
            return Err(LexError {
                message: "unterminated string literal".into(),
                span: Span::new(start, self.pos - start),
            });
        }
        self.pos += 1; // skip closing "
        Ok(self.tok(TokenKind::StringLit(value), start))
    }

    fn lex_number(&mut self, start: usize) -> Result<Token, LexError> {
        while self.pos < self.source.len() && self.source[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if self.pos < self.source.len()
            && self.source[self.pos] == b'.'
            && self.pos + 1 < self.source.len()
            && self.source[self.pos + 1].is_ascii_digit()
        {
            self.pos += 1; // skip '.'
            while self.pos < self.source.len() && self.source[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
            let text = std::str::from_utf8(&self.source[start..self.pos]).unwrap();
            let val: f64 = text.parse().map_err(|_| LexError {
                message: format!("invalid float literal '{text}'"),
                span: Span::new(start, self.pos - start),
            })?;
            return Ok(self.tok(TokenKind::FloatLit(val), start));
        }
        let text = std::str::from_utf8(&self.source[start..self.pos]).unwrap();
        let val: i64 = text.parse().map_err(|_| LexError {
            message: format!("invalid integer literal '{text}'"),
            span: Span::new(start, self.pos - start),
        })?;
        Ok(self.tok(TokenKind::IntLit(val), start))
    }

    fn lex_ident_or_keyword(&mut self, start: usize) -> Result<Token, LexError> {
        while self.pos < self.source.len()
            && (self.source[self.pos].is_ascii_alphanumeric() || self.source[self.pos] == b'_')
        {
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.source[start..self.pos]).unwrap();
        let kind = match text {
            "program" => TokenKind::Program,
            "resources" => TokenKind::Resources,
            "protection" => TokenKind::Protection,
            "fn" => TokenKind::Fn,
            "fn_summary" => TokenKind::FnSummary,
            "entry" => TokenKind::Entry,
            "sync" => TokenKind::Sync,
            "var" => TokenKind::Var,
            "normal" => TokenKind::Normal,
            "closure" => TokenKind::Closure,
            "reads" => TokenKind::Reads,
            "writes" => TokenKind::Writes,
            "callees" => TokenKind::Callees,
            "has_concurrency" => TokenKind::HasConcurrency,

            "Mutex" => TokenKind::Mutex,
            "RwLock" => TokenKind::RwLock,
            "Condvar" => TokenKind::Condvar,
            "Semaphore" => TokenKind::Semaphore,
            "Channel" => TokenKind::Channel,
            "Atomic" => TokenKind::Atomic,
            "Var" => TokenKind::Var,

            "Bool" => TokenKind::BoolType,
            "Int" => TokenKind::IntType,
            "Float" => TokenKind::FloatType,
            "String" => TokenKind::StringType,
            "Enum" => TokenKind::Enum,
            "Struct" => TokenKind::Struct,
            "Array" => TokenKind::Array,

            "Sync" => TokenKind::SyncMode,
            "Async" | "async" => TokenKind::Async,

            "lock" => TokenKind::Lock,
            "read" => TokenKind::Read,
            "write" => TokenKind::Write,
            "drop" => TokenKind::Drop,
            "wait" => TokenKind::Wait,
            "notify" => TokenKind::Notify,
            "notify_all" => TokenKind::NotifyAll,
            "acquire" => TokenKind::Acquire,
            "release" => TokenKind::Release,
            "send" => TokenKind::Send,
            "recv" => TokenKind::Recv,
            "load" => TokenKind::Load,
            "store" => TokenKind::Store,
            "cas" => TokenKind::Cas,

            "res_op" => TokenKind::ResOp,
            "spawn" => TokenKind::Spawn,
            "spawn_async" => TokenKind::SpawnAsync,
            "join" => TokenKind::Join,
            "await" => TokenKind::Await,
            "call" => TokenKind::Call,
            "return" => TokenKind::Return,

            "next" => TokenKind::Next,
            "branch" => TokenKind::Branch,
            "switch" => TokenKind::Switch,

            "true" => TokenKind::BoolLit(true),
            "false" => TokenKind::BoolLit(false),

            // "async" as fn_kind handled by Async token above
            other => TokenKind::Ident(other.to_string()),
        };
        Ok(self.tok(kind, start))
    }
}
