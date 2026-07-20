#[derive(Debug, Clone)]
pub struct Ident {
    pub name: String,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Nil(u32),
    True(u32),
    False(u32),
    Int(i64, u32),
    Float(f64, u32),
    String(String, u32),
    Vararg(u32),

    Ident(Ident),
    Index { table: Box<Expr>, key: Box<Expr>, line: u32 },
    Field { table: Box<Expr>, field: String, line: u32 },

    Unop { op: Unop, operand: Box<Expr>, line: u32 },
    Binop { op: Binop, lhs: Box<Expr>, rhs: Box<Expr>, line: u32 },

    Concat { parts: Vec<Expr>, line: u32 },

    Call(CallExpr),
    MethodCall(MethodCallExpr),

    Function(FuncBody),
    Table(TableConstructor),

    Ternary { cond: Box<Expr>, then: Box<Expr>, else_: Box<Expr>, line: u32 },
    // delta is always +1/-1 (from ++ / --); prefix distinguishes ++i (yields
    // the new value) from i++ (yields the old value) — full C-style semantics.
    IncrDecr { target: Box<Expr>, delta: i64, prefix: bool, line: u32 },
}

impl Expr {
    pub fn line(&self) -> u32 {
        match self {
            Expr::Nil(l) | Expr::True(l) | Expr::False(l) | Expr::Vararg(l) => *l,
            Expr::Int(_, l) | Expr::Float(_, l) | Expr::String(_, l) => *l,
            Expr::Ident(i) => i.line,
            Expr::Index { line, .. } | Expr::Field { line, .. } => *line,
            Expr::Unop { line, .. } | Expr::Binop { line, .. } => *line,
            Expr::Concat { line, .. } => *line,
            Expr::Call(c) => c.line,
            Expr::MethodCall(m) => m.line,
            Expr::Function(f) => f.line,
            Expr::Table(t) => t.line,
            Expr::Ternary { line, .. } => *line,
            Expr::IncrDecr { line, .. } => *line,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CallExpr {
    pub callee: Box<Expr>,
    pub args: Args,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct MethodCallExpr {
    pub receiver: Box<Expr>,
    pub method: String,
    pub args: Args,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub enum Args {
    Exprs(Vec<Expr>),
    String(String),
    Table(TableConstructor),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unop {
    Neg,
    Not,
    Len,
    BNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binop {
    Add, Sub, Mul, Div, IDiv, Mod, Pow,
    BAnd, BOr, BXor, Shl, Shr,
    Concat,
    Eq, Ne, Lt, Le, Gt, Ge,
    And, Or,
}

#[derive(Debug, Clone)]
pub struct TableConstructor {
    pub fields: Vec<TableField>,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub enum TableField {
    Indexed { key: Expr, val: Expr },
    Named   { key: String, val: Expr, line: u32 },
    Positional(Expr),
}

#[derive(Debug, Clone)]
pub struct FuncBody {
    pub params: Vec<String>,
    pub vararg: bool,
    pub body: Block,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub ret: Option<Vec<Expr>>,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Assign   { targets: Vec<Expr>, values: Vec<Expr>, line: u32 },
    // closes[i] is true if names[i] was declared with a <close> attribute.
    Local    { mutable: bool, names: Vec<String>, closes: Vec<bool>, values: Vec<Expr>, line: u32 },
    Destructure { mutable: bool, fields: Vec<String>, value: Expr, line: u32 },
    Do       { body: Block, line: u32 },
    While    { cond: Expr, body: Block, line: u32 },
    RepeatUntil { body: Block, cond: Expr, line: u32 },
    If       { cond: Expr, then: Block, elseifs: Vec<(Expr, Block)>, else_: Option<Block>, line: u32 },
    ForNum   { var: String, start: Expr, limit: Expr, step: Option<Expr>, body: Block, line: u32 },
    ForIn    { vars: Vec<String>, iters: Vec<Expr>, body: Block, line: u32 },
    FuncDef  { name: FuncName, body: FuncBody, line: u32 },
    LocalFunc { name: String, body: FuncBody, line: u32 },
    Call     (CallExpr),
    MethodCall(MethodCallExpr),
    // For expressions used as statements purely for their side effect (currently
    // just ++/--); the value is discarded, unlike Call/MethodCall which can also
    // appear in value position and get their own dedicated Expr variants already.
    ExprStmt (Expr),
    Break    (u32),
    Continue (u32),
    Goto     (String, u32),
    Label    (String, u32),
}

#[derive(Debug, Clone)]
pub struct FuncName {
    pub parts: Vec<String>,
    pub method: Option<String>,
}
