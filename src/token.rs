use crate::span::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Literals
    IntLit(i64),
    FloatLit(f64),
    BoolLit(bool),
    StringLit(String),

    // Identifier
    Ident(String),

    // Keywords
    Program,
    Resources,
    Protection,
    Fn,
    FnSummary,
    Entry,
    Sync,      // keyword "sync" (resource decl)
    Async,     // keyword "async" (fn_kind / mode)
    Var,
    Normal,
    Closure,
    Reads,
    Writes,
    Callees,
    HasConcurrency,

    // Sync types
    Mutex,
    RwLock,
    Condvar,
    Semaphore,
    Channel,

    // Var types
    Atomic,

    // Base types
    BoolType,
    IntType,
    FloatType,
    StringType,
    Enum,
    Struct,
    Array,

    // Actions (used inside res_op)
    Lock,
    Read,
    Write,
    Drop,
    Wait,
    Notify,
    NotifyAll,
    Acquire,
    Release,
    Send,
    Recv,
    Load,
    Store,
    Cas,

    // Operations
    ResOp,
    Spawn,
    SpawnAsync,
    Join,
    Await,
    Call,
    Return,

    // Transfer
    Next,
    Branch,
    Switch,

    // Mode identifiers (parsed as keyword)
    SyncMode,

    // Assignment / Comparison operators
    Eq,
    EqEq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,

    // Arithmetic operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,

    // Punctuation
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Colon,
    Semicolon,
    Comma,
    Arrow,     // ->
    FatArrow,  // =>

    // Special
    Eof,
}

impl TokenKind {
    pub fn is_eof(&self) -> bool {
        matches!(self, TokenKind::Eof)
    }
}
