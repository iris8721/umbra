use crate::ast::*;
use crate::lexer::{LexError, Lexer, Token, TokenKind};

#[derive(Debug, Clone)]
pub enum ParseError {
    Lex(LexError),
    Expected { what: &'static str, line: u32 },
    Unexpected { tok: String, line: u32 },
}

impl From<LexError> for ParseError {
    fn from(e: LexError) -> Self { ParseError::Lex(e) }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Lex(e) => write!(f, "{e}"),
            ParseError::Expected { what, line } => write!(f, "line {line}: expected {what}"),
            ParseError::Unexpected { tok, line } => write!(f, "line {line}: unexpected '{tok}'"),
        }
    }
}

type PResult<T> = Result<T, ParseError>;

enum StmtResult {
    Stmt(Stmt),
    Expr(Expr),
}

pub struct Parser<'src> {
    lexer: Lexer<'src>,
    current: Token,
    lookahead: Option<Token>,
    errors: Vec<ParseError>,
}

impl<'src> Parser<'src> {
    pub fn new(src: &'src str) -> PResult<Self> {
        let mut lexer = Lexer::new(src);
        let current = lexer.next_token()?;
        Ok(Self { lexer, current, lookahead: None, errors: Vec::new() })
    }

    fn peek(&self) -> &TokenKind { &self.current.kind }
    fn line(&self) -> u32 { self.current.line }

    fn peek2(&mut self) -> PResult<&TokenKind> {
        if self.lookahead.is_none() {
            self.lookahead = Some(self.lexer.next_token()?);
        }
        Ok(&self.lookahead.as_ref().unwrap().kind)
    }

    fn advance(&mut self) -> PResult<Token> {
        let prev = self.current.clone();
        self.current = match self.lookahead.take() {
            Some(t) => t,
            None => self.lexer.next_token()?,
        };
        Ok(prev)
    }

    fn check(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(self.peek()) == std::mem::discriminant(kind)
    }

    fn eat(&mut self, kind: &TokenKind) -> PResult<bool> {
        if self.check(kind) { self.advance()?; Ok(true) } else { Ok(false) }
    }

    fn expect(&mut self, kind: &TokenKind, what: &'static str) -> PResult<Token> {
        if self.check(kind) {
            self.advance()
        } else {
            Err(ParseError::Expected { what, line: self.line() })
        }
    }

    fn expect_ident(&mut self) -> PResult<String> {
        if matches!(self.current.kind, TokenKind::Ident(_)) {
            match self.advance()?.kind {
                TokenKind::Ident(s) => Ok(s),
                _ => unreachable!(),
            }
        } else {
            Err(ParseError::Expected { what: "identifier", line: self.line() })
        }
    }

    fn expect_string(&mut self) -> PResult<String> {
        if matches!(self.current.kind, TokenKind::String(_)) {
            match self.advance()?.kind {
                TokenKind::String(s) => Ok(s),
                _ => unreachable!(),
            }
        } else {
            Err(ParseError::Expected { what: "string", line: self.line() })
        }
    }

    fn sync_to_stmt(&mut self) {
        loop {
            match self.peek() {
                TokenKind::Eof
                | TokenKind::RBrace
                | TokenKind::Else
                | TokenKind::If
                | TokenKind::While
                | TokenKind::For
                | TokenKind::Let
                | TokenKind::Var
                | TokenKind::Fn
                | TokenKind::Return => break,
                _ => { let _ = self.advance(); }
            }
        }
    }

    fn record(&mut self, e: ParseError) {
        self.errors.push(e);
    }

    pub fn parse_chunk(src: &'src str) -> (Block, Vec<ParseError>) {
        match Self::new(src) {
            Err(e) => (Block { stmts: vec![], ret: None, line: 1 }, vec![e]),
            Ok(mut p) => {
                let block = p.parse_block();
                (block, p.errors)
            }
        }
    }

    fn parse_block(&mut self) -> Block {
        self.parse_block_body(false)
    }

    fn parse_braced_block(&mut self) -> PResult<Block> {
        self.expect(&TokenKind::LBrace, "'{'")?;
        let block = self.parse_block_body(true);
        self.expect(&TokenKind::RBrace, "'}'")?;
        Ok(block)
    }

    fn parse_block_body(&mut self, stop_at_brace: bool) -> Block {
        let line = self.line();
        let mut stmts = Vec::new();
        let mut ret = None;
        loop {
            while self.eat(&TokenKind::Semicolon).unwrap_or(false) {}
            let done = if stop_at_brace {
                matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof)
            } else {
                matches!(self.peek(), TokenKind::Eof)
            };
            if done { break; }

            if self.check(&TokenKind::Return) {
                match self.parse_return() {
                    Ok(r) => ret = Some(r),
                    Err(e) => self.record(e),
                }
                let _ = self.eat(&TokenKind::Semicolon);
                break;
            }

            match self.parse_stmt() {
                Ok(StmtResult::Stmt(s)) => stmts.push(s),
                Ok(StmtResult::Expr(e)) => {
                    if stop_at_brace && matches!(self.peek(), TokenKind::RBrace) {
                        ret = Some(vec![e]);
                    } else {
                        self.record(ParseError::Unexpected {
                            tok: "expression".to_owned(),
                            line: e.line(),
                        });
                    }
                    break;
                }
                Err(e) => { self.record(e); self.sync_to_stmt(); }
            }
        }
        Block { stmts, ret, line }
    }

    fn parse_return(&mut self) -> PResult<Vec<Expr>> {
        self.advance()?;
        match self.peek() {
            TokenKind::Eof | TokenKind::RBrace | TokenKind::Semicolon => Ok(vec![]),
            _ => self.parse_exprlist(),
        }
    }

    fn parse_stmt(&mut self) -> PResult<StmtResult> {
        let line = self.line();
        match self.peek() {
            TokenKind::Break => {
                self.advance()?;
                Ok(StmtResult::Stmt(Stmt::Break(line)))
            }
            TokenKind::While => {
                self.advance()?;
                let cond = self.parse_expr()?;
                let body = self.parse_braced_block()?;
                Ok(StmtResult::Stmt(Stmt::While { cond, body, line }))
            }
            TokenKind::If  => Ok(StmtResult::Stmt(self.parse_if()?)),
            TokenKind::For => Ok(StmtResult::Stmt(self.parse_for()?)),
            TokenKind::Fn  => Ok(StmtResult::Stmt(self.parse_fn_stmt(line)?)),
            TokenKind::Let => {
                self.advance()?;
                Ok(StmtResult::Stmt(self.parse_binding(false, line)?))
            }
            TokenKind::Var => {
                self.advance()?;
                Ok(StmtResult::Stmt(self.parse_binding(true, line)?))
            }
            _ => {
                let expr = self.parse_expr()?;
                if matches!(self.peek(), TokenKind::Assign | TokenKind::Comma) {
                    Ok(StmtResult::Stmt(self.parse_assign(expr, line)?))
                } else {
                    match expr {
                        Expr::Call(c)       => Ok(StmtResult::Stmt(Stmt::Call(c))),
                        Expr::MethodCall(m) => Ok(StmtResult::Stmt(Stmt::MethodCall(m))),
                        other               => Ok(StmtResult::Expr(other)),
                    }
                }
            }
        }
    }

    fn parse_if(&mut self) -> PResult<Stmt> {
        let line = self.line();
        self.expect(&TokenKind::If, "'if'")?;
        let cond = self.parse_expr()?;
        let then = self.parse_braced_block()?;
        let mut elseifs = Vec::new();
        let mut else_ = None;
        loop {
            if self.eat(&TokenKind::Else)? {
                if self.check(&TokenKind::If) {
                    self.advance()?;
                    let c = self.parse_expr()?;
                    let b = self.parse_braced_block()?;
                    elseifs.push((c, b));
                } else {
                    else_ = Some(self.parse_braced_block()?);
                    break;
                }
            } else {
                break;
            }
        }
        Ok(Stmt::If { cond, then, elseifs, else_, line })
    }

    fn parse_for(&mut self) -> PResult<Stmt> {
        let line = self.line();
        self.expect(&TokenKind::For, "'for'")?;
        let first = self.expect_ident()?;
        if self.eat(&TokenKind::Assign)? {
            let start = self.parse_expr()?;
            self.expect(&TokenKind::Comma, "','")?;
            let limit = self.parse_expr()?;
            let step  = if self.eat(&TokenKind::Comma)? { Some(self.parse_expr()?) } else { None };
            let body  = self.parse_braced_block()?;
            Ok(Stmt::ForNum { var: first, start, limit, step, body, line })
        } else {
            let mut vars = vec![first];
            while self.eat(&TokenKind::Comma)? {
                vars.push(self.expect_ident()?);
            }
            self.expect(&TokenKind::In, "'in'")?;
            let iters = self.parse_exprlist()?;
            let body  = self.parse_braced_block()?;
            Ok(Stmt::ForIn { vars, iters, body, line })
        }
    }

    fn parse_fn_stmt(&mut self, line: u32) -> PResult<Stmt> {
        self.advance()?;
        let name = self.parse_funcname()?;
        let body = self.parse_funcbody()?;
        Ok(Stmt::FuncDef { name, body, line })
    }

    fn parse_binding(&mut self, mutable: bool, line: u32) -> PResult<Stmt> {
        if self.check(&TokenKind::Fn) {
            self.advance()?;
            let name = self.expect_ident()?;
            let body = self.parse_funcbody()?;
            return Ok(Stmt::LocalFunc { name, body, line });
        }
        let mut names = Vec::new();
        names.push(self.expect_ident()?);
        while self.eat(&TokenKind::Comma)? {
            names.push(self.expect_ident()?);
        }
        let values = if self.eat(&TokenKind::Assign)? { self.parse_exprlist()? } else { vec![] };
        Ok(Stmt::Local { mutable, names, values, line })
    }

    fn parse_assign(&mut self, first: Expr, line: u32) -> PResult<Stmt> {
        let mut targets = vec![first];
        while self.eat(&TokenKind::Comma)? {
            targets.push(self.parse_suffixed_expr()?);
        }
        self.expect(&TokenKind::Assign, "'='")?;
        let values = self.parse_exprlist()?;
        Ok(Stmt::Assign { targets, values, line })
    }

    fn parse_funcname(&mut self) -> PResult<FuncName> {
        let mut parts = vec![self.expect_ident()?];
        while self.eat(&TokenKind::Dot)? {
            parts.push(self.expect_ident()?);
        }
        let method = if self.eat(&TokenKind::Colon)? {
            Some(self.expect_ident()?)
        } else {
            None
        };
        Ok(FuncName { parts, method })
    }

    fn parse_funcbody(&mut self) -> PResult<FuncBody> {
        let line = self.line();
        self.expect(&TokenKind::LParen, "'('")?;
        let mut params = Vec::new();
        let mut vararg = false;
        if !self.check(&TokenKind::RParen) {
            if self.check(&TokenKind::DotDotDot) {
                self.advance()?;
                vararg = true;
            } else {
                params.push(self.expect_ident()?);
                loop {
                    if !self.eat(&TokenKind::Comma)? { break; }
                    if self.check(&TokenKind::DotDotDot) {
                        self.advance()?;
                        vararg = true;
                        break;
                    }
                    params.push(self.expect_ident()?);
                }
            }
        }
        self.expect(&TokenKind::RParen, "')'")?;
        let body = self.parse_braced_block()?;
        Ok(FuncBody { params, vararg, body, line })
    }

    fn parse_closure(&mut self) -> PResult<Expr> {
        let line = self.line();
        self.advance()?;
        let mut params = Vec::new();
        let mut vararg = false;
        if !self.check(&TokenKind::Pipe) {
            if self.check(&TokenKind::DotDotDot) {
                self.advance()?;
                vararg = true;
            } else {
                params.push(self.expect_ident()?);
                loop {
                    if !self.eat(&TokenKind::Comma)? { break; }
                    if self.check(&TokenKind::DotDotDot) {
                        self.advance()?;
                        vararg = true;
                        break;
                    }
                    params.push(self.expect_ident()?);
                }
            }
        }
        self.expect(&TokenKind::Pipe, "'|'")?;
        let body = if self.check(&TokenKind::LBrace) {
            self.parse_braced_block()?
        } else {
            let expr = self.parse_expr()?;
            let l = expr.line();
            Block { stmts: vec![], ret: Some(vec![expr]), line: l }
        };
        Ok(Expr::Function(FuncBody { params, vararg, body, line }))
    }

    fn parse_exprlist(&mut self) -> PResult<Vec<Expr>> {
        let mut exprs = vec![self.parse_expr()?];
        while self.eat(&TokenKind::Comma)? {
            exprs.push(self.parse_expr()?);
        }
        Ok(exprs)
    }

    fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_pratt(0)
    }

    fn parse_pratt(&mut self, min_bp: u8) -> PResult<Expr> {
        let line = self.line();
        let mut lhs = if let Some((op, rbp)) = self.unary_op() {
            self.advance()?;
            let operand = self.parse_pratt(rbp)?;
            Expr::Unop { op, operand: Box::new(operand), line }
        } else {
            self.parse_simple_expr()?
        };

        loop {
            let line = self.line();
            let Some((op, lbp, rbp)) = self.binary_op() else { break };
            if lbp < min_bp { break; }
            self.advance()?;
            let rhs = self.parse_pratt(rbp)?;
            if op == Binop::Concat {
                let mut parts = match lhs {
                    Expr::Concat { parts, .. } => parts,
                    other => vec![other],
                };
                match rhs {
                    Expr::Concat { parts: rhs_parts, .. } => parts.extend(rhs_parts),
                    other => parts.push(other),
                }
                lhs = Expr::Concat { parts, line };
            } else {
                lhs = Expr::Binop { op, lhs: Box::new(lhs), rhs: Box::new(rhs), line };
            }
        }
        Ok(lhs)
    }

    fn unary_op(&self) -> Option<(Unop, u8)> {
        match self.peek() {
            TokenKind::Minus => Some((Unop::Neg,  18)),
            TokenKind::Not   => Some((Unop::Not,  18)),
            TokenKind::Hash  => Some((Unop::Len,  18)),
            TokenKind::Tilde => Some((Unop::BNot, 18)),
            _ => None,
        }
    }

    fn binary_op(&self) -> Option<(Binop, u8, u8)> {
        match self.peek() {
            TokenKind::Or      => Some((Binop::Or,     1,  2)),
            TokenKind::And     => Some((Binop::And,    3,  4)),
            TokenKind::Lt      => Some((Binop::Lt,     5,  6)),
            TokenKind::Gt      => Some((Binop::Gt,     5,  6)),
            TokenKind::LtEq    => Some((Binop::Le,     5,  6)),
            TokenKind::GtEq    => Some((Binop::Ge,     5,  6)),
            TokenKind::BangEq  => Some((Binop::Ne,     5,  6)),
            TokenKind::Eq      => Some((Binop::Eq,     5,  6)),
            TokenKind::Pipe    => Some((Binop::BOr,    7,  8)),
            TokenKind::Tilde   => Some((Binop::BXor,   7,  8)),
            TokenKind::Amp     => Some((Binop::BAnd,   9, 10)),
            TokenKind::LtLt    => Some((Binop::Shl,   11, 12)),
            TokenKind::GtGt    => Some((Binop::Shr,   11, 12)),
            TokenKind::DotDot  => Some((Binop::Concat, 14, 13)), // lbp > rbp: right-associative
            TokenKind::Plus    => Some((Binop::Add,   15, 16)),
            TokenKind::Minus   => Some((Binop::Sub,   15, 16)),
            TokenKind::Star    => Some((Binop::Mul,   17, 18)),
            TokenKind::Slash   => Some((Binop::Div,   17, 18)),
            TokenKind::Percent => Some((Binop::Mod,   17, 18)),
            TokenKind::Caret   => Some((Binop::Pow,   20, 19)), // lbp > rbp: right-associative
            _ => None,
        }
    }

    fn parse_simple_expr(&mut self) -> PResult<Expr> {
        let line = self.line();
        match self.peek() {
            TokenKind::Int(_) | TokenKind::Float(_) | TokenKind::String(_)
            | TokenKind::None | TokenKind::True | TokenKind::False
            | TokenKind::DotDotDot => {
                match self.advance()?.kind {
                    TokenKind::Int(n)    => Ok(Expr::Int(n, line)),
                    TokenKind::Float(f)  => Ok(Expr::Float(f, line)),
                    TokenKind::String(s) => Ok(Expr::String(s, line)),
                    TokenKind::None      => Ok(Expr::Nil(line)),
                    TokenKind::True      => Ok(Expr::True(line)),
                    TokenKind::False     => Ok(Expr::False(line)),
                    TokenKind::DotDotDot => Ok(Expr::Vararg(line)),
                    _ => unreachable!(),
                }
            }
            TokenKind::Fn => { self.advance()?; Ok(Expr::Function(self.parse_funcbody()?)) }
            TokenKind::Pipe => self.parse_closure(),
            TokenKind::LBrace => Ok(Expr::Table(self.parse_table_constructor()?)),
            _ => self.parse_suffixed_expr(),
        }
    }

    fn parse_suffixed_expr(&mut self) -> PResult<Expr> {
        let mut e = self.parse_primary()?;
        loop {
            let line = self.line();
            match self.peek() {
                TokenKind::Dot => {
                    self.advance()?;
                    let field = self.expect_ident()?;
                    e = Expr::Field { table: Box::new(e), field, line };
                }
                TokenKind::LBracket => {
                    self.advance()?;
                    let key = self.parse_expr()?;
                    self.expect(&TokenKind::RBracket, "']'")?;
                    e = Expr::Index { table: Box::new(e), key: Box::new(key), line };
                }
                TokenKind::Colon => {
                    self.advance()?;
                    let method = self.expect_ident()?;
                    let args = self.parse_args()?;
                    e = Expr::MethodCall(MethodCallExpr {
                        receiver: Box::new(e), method, args, line,
                    });
                }
                TokenKind::LParen | TokenKind::String(_) => {
                    let args = self.parse_args()?;
                    e = Expr::Call(CallExpr { callee: Box::new(e), args, line });
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        let line = self.line();
        match self.peek() {
            TokenKind::Ident(_) => {
                match self.advance()?.kind {
                    TokenKind::Ident(s) => Ok(Expr::Ident(Ident { name: s, line })),
                    _ => unreachable!(),
                }
            }
            TokenKind::Yield => {
                self.advance()?;
                Ok(Expr::Ident(Ident { name: "yield".to_owned(), line }))
            }
            TokenKind::LParen => {
                self.advance()?;
                let e = self.parse_expr()?;
                self.expect(&TokenKind::RParen, "')'")?;
                Ok(e)
            }
            _ => Err(ParseError::Unexpected {
                tok: format!("{:?}", self.peek()),
                line,
            }),
        }
    }

    fn parse_args(&mut self) -> PResult<Args> {
        match self.peek() {
            TokenKind::LParen => {
                self.advance()?;
                if self.check(&TokenKind::RParen) {
                    self.advance()?;
                    Ok(Args::Exprs(vec![]))
                } else {
                    let exprs = self.parse_exprlist()?;
                    self.expect(&TokenKind::RParen, "')'")?;
                    Ok(Args::Exprs(exprs))
                }
            }
            TokenKind::String(_) => Ok(Args::String(self.expect_string()?)),
            _ => Err(ParseError::Expected { what: "function arguments", line: self.line() }),
        }
    }

    fn parse_table_constructor(&mut self) -> PResult<TableConstructor> {
        let line = self.line();
        self.expect(&TokenKind::LBrace, "'{'")?;
        let mut fields = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.check(&TokenKind::Eof) {
            fields.push(self.parse_table_field()?);
            if !self.eat(&TokenKind::Comma)? && !self.eat(&TokenKind::Semicolon)? {
                break;
            }
        }
        self.expect(&TokenKind::RBrace, "'}'")?;
        Ok(TableConstructor { fields, line })
    }

    fn parse_table_field(&mut self) -> PResult<TableField> {
        let line = self.line();
        if self.check(&TokenKind::LBracket) {
            self.advance()?;
            let key = self.parse_expr()?;
            self.expect(&TokenKind::RBracket, "']'")?;
            self.expect(&TokenKind::Assign, "'='")?;
            let val = self.parse_expr()?;
            return Ok(TableField::Indexed { key, val });
        }
        if matches!(self.peek(), TokenKind::Ident(_)) && self.peek_ahead_is_assign() {
            let name = self.expect_ident()?;
            self.advance()?;
            let val = self.parse_expr()?;
            return Ok(TableField::Named { key: name, val, line });
        }
        Ok(TableField::Positional(self.parse_expr()?))
    }

    fn peek_ahead_is_assign(&mut self) -> bool {
        matches!(self.peek2(), Ok(TokenKind::Assign))
    }
}

pub fn parse(src: &str) -> (Block, Vec<ParseError>) {
    Parser::parse_chunk(src)
}
