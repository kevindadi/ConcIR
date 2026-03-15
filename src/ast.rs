use crate::span::Span;

// ──────────────────── Top-level ────────────────────

#[derive(Debug, Clone)]
pub struct Program {
    pub name: Spanned<String>,
    pub resources: Vec<ResourceDecl>,
    pub protections: Vec<ProtectionDecl>,
    pub functions: Vec<Function>,
    pub fn_summaries: Vec<FnSummary>,
    pub entry: Spanned<String>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Spanned<T> {
    pub value: T,
    pub span: Span,
}

impl<T> Spanned<T> {
    pub fn new(value: T, span: Span) -> Self {
        Self { value, span }
    }
}

// ──────────────────── Resources ────────────────────

#[derive(Debug, Clone)]
pub struct ResourceDecl {
    pub name: Spanned<String>,
    pub kind: ResourceKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ResourceKind {
    Sync(SyncType),
    Var(VarType),
}

#[derive(Debug, Clone)]
pub enum SyncType {
    Mutex(Mode),
    RwLock(Mode),
    Condvar(Mode),
    Semaphore(Mode, i64),
    Channel(Mode, BaseType),
}

#[derive(Debug, Clone)]
pub enum VarType {
    Var(BaseType),
    Atomic(BaseType),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Sync,
    Async,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BaseType {
    Bool,
    Int,
    Float,
    String,
    Enum(Vec<String>),
    Struct(Vec<FieldDecl>),
    Array(Box<BaseType>, i64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldDecl {
    pub name: String,
    pub ty: BaseType,
}

// ──────────────────── Protection ────────────────────

#[derive(Debug, Clone)]
pub struct ProtectionDecl {
    pub var_name: Spanned<String>,
    pub lock_name: Spanned<String>,
    pub span: Span,
}

// ──────────────────── Function ────────────────────

#[derive(Debug, Clone)]
pub struct Function {
    pub kind: FnKind,
    pub name: Spanned<String>,
    pub statements: Vec<Statement>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FnKind {
    Normal,
    Async,
    Closure,
}

#[derive(Debug, Clone)]
pub struct Statement {
    pub sid: Spanned<String>,
    pub op: Op,
    pub transfer: Transfer,
    pub span: Span,
}

// ──────────────────── Operations ────────────────────

#[derive(Debug, Clone)]
pub enum Op {
    ResOp(Spanned<String>, Action),
    Spawn(Spanned<String>),
    SpawnAsync(Spanned<String>),
    Join(Spanned<String>),
    Await(Spanned<String>),
    Call(Spanned<String>),
    Return,
}

#[derive(Debug, Clone)]
pub enum Action {
    Lock,
    Read,
    Write(Expr),
    Drop,
    Wait(Spanned<String>),
    Notify,
    NotifyAll,
    Acquire,
    Release,
    Send(Expr),
    Recv,
    Load,
    Store(Expr),
    Cas(Expr, Expr),
}

// ──────────────────── Transfer ────────────────────

#[derive(Debug, Clone)]
pub enum Transfer {
    Next(Spanned<String>),
    Branch(CondExpr, Spanned<String>, Spanned<String>),
    Switch(Spanned<String>, Vec<Case>),
    Return,
}

#[derive(Debug, Clone)]
pub struct CondExpr {
    pub lhs: Expr,
    pub op: CmpOp,
    pub rhs: Expr,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
}

#[derive(Debug, Clone)]
pub struct Case {
    pub label: Literal,
    pub target: Spanned<String>,
}

// ──────────────────── Expressions ────────────────────

#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Literal),
    Ident(Spanned<String>),
    BinOp(Box<Expr>, ArithOp, Box<Expr>),
    Paren(Box<Expr>),
    UnaryMinus(Box<Expr>),
}

impl Expr {
    pub fn infer_base_type(&self) -> Option<BaseType> {
        match self {
            Expr::Literal(lit) => lit.infer_base_type(),
            Expr::Ident(_) => None,
            Expr::BinOp(lhs, _, _) => lhs.infer_base_type(),
            Expr::Paren(inner) | Expr::UnaryMinus(inner) => inner.infer_base_type(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Literal {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    Ident(String),
    Compound(Vec<Literal>),
}

impl Literal {
    pub fn infer_base_type(&self) -> Option<BaseType> {
        match self {
            Literal::Int(_) => Some(BaseType::Int),
            Literal::Float(_) => Some(BaseType::Float),
            Literal::Bool(_) => Some(BaseType::Bool),
            Literal::String(_) => Some(BaseType::String),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

// ──────────────────── FnSummary ────────────────────

#[derive(Debug, Clone)]
pub struct FnSummary {
    pub name: Spanned<String>,
    pub reads: Vec<Spanned<String>>,
    pub writes: Vec<Spanned<String>>,
    pub callees: Vec<Spanned<String>>,
    pub has_concurrency: bool,
    pub span: Span,
}
