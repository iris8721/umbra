#[derive(Debug, Clone, PartialEq)]
pub enum InterpPart {
    Str(String),
    // Raw source text of a ${...} embedded expression, parsed lazily by the
    // parser rather than here, so the lexer doesn't need to know about Expr.
    Expr(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Int(i64),
    Float(f64),
    String(String),
    InterpString(Vec<InterpPart>),
    Ident(String),

    And,
    Break,
    Case,
    Continue,
    Else,
    Goto,
    Repeat,
    Until,
    False,
    Fn,
    For,
    If,
    In,
    Let,
    None,
    Not,
    Or,
    Return,
    Switch,
    True,
    Var,
    While,
    Yield,

    Plus,
    Minus,
    Star,
    Slash,
    SlashSlash,
    Percent,
    Caret,
    Hash,
    Question,
    Amp,
    Tilde,
    Pipe,
    LtLt,
    GtGt,
    BangEq,
    Eq,
    LtEq,
    GtEq,
    Lt,
    Gt,
    Assign,
    PlusEq,
    MinusEq,
    PlusPlus,
    MinusMinus,
    StarEq,
    SlashEq,
    SlashSlashEq,
    PercentEq,
    CaretEq,
    AmpEq,
    PipeEq,
    TildeEq,
    LtLtEq,
    GtGtEq,
    DotDotEq,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    ColonColon,
    Semicolon,
    Colon,
    Comma,
    Dot,
    DotDot,
    DotDotDot,

    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub line: u32,
}

pub struct Lexer<'src> {
    src: &'src [u8],
    pos: usize,
    line: u32,
}

impl<'src> Lexer<'src> {
    pub fn new(src: &'src str) -> Self {
        Self { src: src.as_bytes(), pos: 0, line: 1 }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<u8> {
        self.src.get(self.pos + 1).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let b = self.src.get(self.pos).copied();
        if b == Some(b'\n') { self.line += 1; }
        self.pos += 1;
        b
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) { self.advance(); true } else { false }
    }

    fn skip_whitespace_and_comments(&mut self) -> Result<(), LexError> {
        loop {
            while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
                self.advance();
            }
            if self.peek() == Some(b'@') {
                self.advance();
                while !matches!(self.peek(), Some(b'\n') | None) { self.advance(); }
                continue;
            }
            if self.peek() == Some(b'/') && self.peek2() == Some(b'*') {
                let line = self.line;
                self.advance(); self.advance();
                loop {
                    match self.advance() {
                        None => return Err(LexError::UnterminatedComment(line)),
                        Some(b'*') if self.peek() == Some(b'/') => { self.advance(); break; }
                        _ => {}
                    }
                }
                continue;
            }
            break;
        }
        Ok(())
    }

    /// Assumes `pos` is already past the opening `[`; returns the `=` level
    /// if this is a valid long-bracket opener, else -1.
    fn count_long_bracket(&self) -> i32 {
        let mut i = self.pos;
        let mut level = 0usize;
        while self.src.get(i) == Some(&b'=') { level += 1; i += 1; }
        if self.src.get(i) == Some(&b'[') { level as i32 } else { -1 }
    }

    fn read_long_string(&mut self, level: usize) -> Result<String, LexError> {
        let line = self.line;
        for _ in 0..level { self.advance(); }
        self.advance();
        if self.peek() == Some(b'\n') { self.advance(); }
        else if self.peek() == Some(b'\r') {
            self.advance();
            if self.peek() == Some(b'\n') { self.advance(); }
        }
        let mut out = Vec::new();
        loop {
            match self.advance() {
                None => return Err(LexError::UnterminatedString(line)),
                Some(b']') => {
                    let mut eqs = Vec::new();
                    while self.peek() == Some(b'=') { eqs.push(b'='); self.advance(); }
                    if eqs.len() == level && self.peek() == Some(b']') {
                        self.advance();
                        break;
                    }
                    out.push(b']');
                    out.extend_from_slice(&eqs);
                }
                Some(b) => out.push(b),
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    // Consumes up to (and including) the '}' that closes a ${...} embedded
    // expression, skipping over any nested string literal's own braces so a
    // `}` inside e.g. "${f(\"}\")}" doesn't end the interpolation early.
    fn read_interp_expr_src(&mut self) -> Result<String, LexError> {
        let start = self.pos;
        let mut depth: i32 = 1;
        loop {
            match self.advance() {
                None => return Err(LexError::UnterminatedString(self.line)),
                Some(b'{') => depth += 1,
                Some(b'}') => {
                    depth -= 1;
                    if depth == 0 { break; }
                }
                Some(q @ (b'"' | b'\'')) => loop {
                    match self.advance() {
                        None | Some(b'\n') => return Err(LexError::UnterminatedString(self.line)),
                        Some(b'\\') => { self.advance(); }
                        Some(b) if b == q => break,
                        Some(_) => {}
                    }
                },
                Some(_) => {}
            }
        }
        let end = self.pos - 1;
        Ok(String::from_utf8_lossy(&self.src[start..end]).into_owned())
    }

    fn read_string(&mut self, delim: u8) -> Result<TokenKind, LexError> {
        let mut out = Vec::new();
        let mut parts: Vec<InterpPart> = Vec::new();
        let mut has_interp = false;
        loop {
            match self.advance() {
                None | Some(b'\n') => return Err(LexError::UnterminatedString(self.line)),
                Some(b) if b == delim => break,
                Some(b'$') if self.peek() == Some(b'{') => {
                    self.advance();
                    has_interp = true;
                    parts.push(InterpPart::Str(String::from_utf8_lossy(&out).into_owned()));
                    out.clear();
                    parts.push(InterpPart::Expr(self.read_interp_expr_src()?));
                }
                Some(b'\\') => {
                    match self.advance() {
                        Some(b'a')  => out.push(7),
                        Some(b'b')  => out.push(8),
                        Some(b'f')  => out.push(12),
                        Some(b'n')  => out.push(b'\n'),
                        Some(b'r')  => out.push(b'\r'),
                        Some(b't')  => out.push(b'\t'),
                        Some(b'v')  => out.push(11),
                        Some(b'\\') => out.push(b'\\'),
                        Some(b'\'') => out.push(b'\''),
                        Some(b'"')  => out.push(b'"'),
                        Some(b'\n') => out.push(b'\n'),
                        Some(b'\r') => {
                            out.push(b'\n');
                            if self.peek() == Some(b'\n') { self.advance(); }
                        }
                        Some(b'z') => {
                            while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c)) {
                                self.advance();
                            }
                        }
                        Some(b'u') => {
                            if !self.eat(b'{') { return Err(LexError::BadEscape(self.line)); }
                            let mut cp: u32 = 0;
                            let mut digits = 0usize;
                            while let Some(h) = self.peek().and_then(hex_digit) {
                                self.advance();
                                digits += 1;
                                cp = match cp.checked_mul(16).and_then(|v| v.checked_add(h as u32)) {
                                    Some(v) => v,
                                    None => return Err(LexError::BadEscape(self.line)),
                                };
                            }
                            if digits == 0 || !self.eat(b'}') {
                                return Err(LexError::BadEscape(self.line));
                            }
                            match char::from_u32(cp) {
                                Some(c) => {
                                    let mut buf = [0u8; 4];
                                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                                }
                                None => return Err(LexError::BadEscape(self.line)),
                            }
                        }
                        Some(b'$') => out.push(b'$'),
                        Some(b'x') => {
                            let h1 = self.advance().and_then(hex_digit);
                            let h2 = self.advance().and_then(hex_digit);
                            match (h1, h2) {
                                (Some(a), Some(b)) => out.push(a << 4 | b),
                                _ => return Err(LexError::BadEscape(self.line)),
                            }
                        }
                        Some(d) if d.is_ascii_digit() => {
                            let mut n = (d - b'0') as u32;
                            if let Some(d2) = self.peek().filter(|b| b.is_ascii_digit()) {
                                self.advance(); n = n * 10 + (d2 - b'0') as u32;
                                if let Some(d3) = self.peek().filter(|b| b.is_ascii_digit()) {
                                    self.advance(); n = n * 10 + (d3 - b'0') as u32;
                                }
                            }
                            if n > 255 { return Err(LexError::BadEscape(self.line)); }
                            out.push(n as u8);
                        }
                        _ => return Err(LexError::BadEscape(self.line)),
                    }
                }
                Some(b) => out.push(b),
            }
        }
        if has_interp {
            parts.push(InterpPart::Str(String::from_utf8_lossy(&out).into_owned()));
            Ok(TokenKind::InterpString(parts))
        } else {
            Ok(TokenKind::String(String::from_utf8_lossy(&out).into_owned()))
        }
    }

    fn read_number(&mut self, first: u8) -> Result<TokenKind, LexError> {
        let start = self.pos - 1;
        let line = self.line;
        if first == b'0' && matches!(self.peek(), Some(b'x' | b'X')) {
            self.advance();
            while matches!(self.peek(), Some(b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F' | b'_')) {
                self.advance();
            }
            let mut is_float = false;
            // Fractional part: `0xA.8`; a '.' is consumed unless it begins a
            // '..' concat, matching the decimal rule below.
            if self.peek() == Some(b'.') && self.peek2() != Some(b'.') {
                is_float = true;
                self.advance();
                while matches!(self.peek(), Some(b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F' | b'_')) {
                    self.advance();
                }
            }
            // Binary exponent: `0x1p4` == 16.0. Only consumed when at least
            // one digit follows the optional sign, so `0x1p` stays an error.
            if matches!(self.peek(), Some(b'p' | b'P')) {
                let mut i = self.pos + 1;
                if matches!(self.src.get(i), Some(b'+' | b'-')) { i += 1; }
                if matches!(self.src.get(i), Some(b'0'..=b'9')) {
                    is_float = true;
                    self.advance();
                    if matches!(self.peek(), Some(b'+' | b'-')) { self.advance(); }
                    while matches!(self.peek(), Some(b'0'..=b'9')) { self.advance(); }
                }
            }
            let s: String = self.src[start..self.pos].iter()
                .filter(|&&b| b != b'_')
                .map(|&b| b as char).collect();
            if is_float {
                return parse_hex_float(&s).map(TokenKind::Float).map_err(|_| LexError::BadNumber(line));
            }
            // Hex literals wrap modulo 2^64 like Lua's, so 0xFFFFFFFFFFFFFFFF
            // is -1 rather than a malformed-number error.
            return u64::from_str_radix(&s[2..], 16)
                .map(|v| TokenKind::Int(v as i64))
                .map_err(|_| LexError::BadNumber(line));
        }
        let mut is_float = first == b'.';
        while matches!(self.peek(), Some(b'0'..=b'9' | b'_')) { self.advance(); }
        if self.peek() == Some(b'.') && self.peek2() != Some(b'.') {
            is_float = true;
            self.advance();
            while matches!(self.peek(), Some(b'0'..=b'9' | b'_')) { self.advance(); }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.advance();
            if matches!(self.peek(), Some(b'+' | b'-')) { self.advance(); }
            while matches!(self.peek(), Some(b'0'..=b'9')) { self.advance(); }
        }
        let s: String = self.src[start..self.pos].iter()
            .filter(|&&b| b != b'_')
            .map(|&b| b as char).collect();
        if is_float {
            s.parse().map(TokenKind::Float).map_err(|_| LexError::BadNumber(line))
        } else {
            // Decimal literals that overflow i64 become floats, like Lua.
            match s.parse::<i64>() {
                Ok(n) => Ok(TokenKind::Int(n)),
                Err(_) => s.parse::<f64>().map(TokenKind::Float).map_err(|_| LexError::BadNumber(line)),
            }
        }
    }

    pub fn next_token(&mut self) -> Result<Token, LexError> {
        self.skip_whitespace_and_comments()?;
        let line = self.line;
        let b = match self.advance() {
            None => return Ok(Token { kind: TokenKind::Eof, line }),
            Some(b) => b,
        };

        let kind = match b {
            b'+' => if self.eat(b'+') { TokenKind::PlusPlus }
                    else if self.eat(b'=') { TokenKind::PlusEq } else { TokenKind::Plus },
            b'*' => if self.eat(b'=') { TokenKind::StarEq } else { TokenKind::Star },
            b'%' => if self.eat(b'=') { TokenKind::PercentEq } else { TokenKind::Percent },
            b'^' => if self.eat(b'=') { TokenKind::CaretEq } else { TokenKind::Caret },
            b'#' => TokenKind::Hash,
            b'?' => TokenKind::Question,
            b'&' => if self.eat(b'=') { TokenKind::AmpEq } else { TokenKind::Amp },
            b'|' => if self.eat(b'=') { TokenKind::PipeEq } else { TokenKind::Pipe },
            b'(' => TokenKind::LParen,
            b')' => TokenKind::RParen,
            b'{' => TokenKind::LBrace,
            b'}' => TokenKind::RBrace,
            b']' => TokenKind::RBracket,
            b';' => TokenKind::Semicolon,
            b',' => TokenKind::Comma,
            b'-' => if self.eat(b'-') { TokenKind::MinusMinus }
                    else if self.eat(b'=') { TokenKind::MinusEq } else { TokenKind::Minus },
            b'/' => if self.eat(b'/') {
                        if self.eat(b'=') { TokenKind::SlashSlashEq } else { TokenKind::SlashSlash }
                    } else if self.eat(b'=') { TokenKind::SlashEq } else { TokenKind::Slash },
            b'~' => if self.eat(b'=') { TokenKind::TildeEq } else { TokenKind::Tilde },
            b'!' => if self.eat(b'=') { TokenKind::BangEq }
                    else { return Err(LexError::UnexpectedChar('!', line)) },
            b'<' => if self.eat(b'<') {
                        if self.eat(b'=') { TokenKind::LtLtEq } else { TokenKind::LtLt }
                    } else if self.eat(b'=') { TokenKind::LtEq }
                    else { TokenKind::Lt },
            b'>' => if self.eat(b'>') {
                        if self.eat(b'=') { TokenKind::GtGtEq } else { TokenKind::GtGt }
                    } else if self.eat(b'=') { TokenKind::GtEq }
                    else { TokenKind::Gt },
            b'=' => if self.eat(b'=') { TokenKind::Eq } else { TokenKind::Assign },
            b':' => if self.eat(b':') { TokenKind::ColonColon } else { TokenKind::Colon },
            b'.' => {
                if self.peek() == Some(b'.') {
                    self.advance();
                    if self.eat(b'.') { TokenKind::DotDotDot }
                    else if self.eat(b'=') { TokenKind::DotDotEq }
                    else { TokenKind::DotDot }
                } else if matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.read_number(b'.')?
                } else {
                    TokenKind::Dot
                }
            }
            b'[' => {
                let level = self.count_long_bracket();
                if level >= 0 {
                    TokenKind::String(self.read_long_string(level as usize)?)
                } else {
                    TokenKind::LBracket
                }
            }
            b'\'' | b'"' => self.read_string(b)?,
            b'0'..=b'9' => self.read_number(b)?,
            b if b.is_ascii_alphabetic() || b == b'_' => {
                let start = self.pos - 1;
                while matches!(self.peek(), Some(b) if b.is_ascii_alphanumeric() || b == b'_') {
                    self.advance();
                }
                let word = std::str::from_utf8(&self.src[start..self.pos]).unwrap();
                keyword_or_ident(word)
            }
            b => return Err(LexError::UnexpectedChar(b as char, line)),
        };

        Ok(Token { kind, line })
    }

    pub fn tokenize(src: &'src str) -> Result<Vec<Token>, LexError> {
        let mut lex = Self::new(src);
        let mut tokens = Vec::new();
        loop {
            let tok = lex.next_token()?;
            let is_eof = tok.kind == TokenKind::Eof;
            tokens.push(tok);
            if is_eof { break; }
        }
        Ok(tokens)
    }
}

fn keyword_or_ident(s: &str) -> TokenKind {
    match s {
        "and"      => TokenKind::And,
        "break"    => TokenKind::Break,
        "case"     => TokenKind::Case,
        "continue" => TokenKind::Continue,
        "else"   => TokenKind::Else,
        "false"  => TokenKind::False,
        "fn"     => TokenKind::Fn,
        "for"    => TokenKind::For,
        "goto"   => TokenKind::Goto,
        "if"     => TokenKind::If,
        "in"     => TokenKind::In,
        "let"    => TokenKind::Let,
        "none"   => TokenKind::None,
        "not"    => TokenKind::Not,
        "or"     => TokenKind::Or,
        "repeat" => TokenKind::Repeat,
        "return" => TokenKind::Return,
        "switch" => TokenKind::Switch,
        "true"   => TokenKind::True,
        "until"  => TokenKind::Until,
        "var"    => TokenKind::Var,
        "while"  => TokenKind::While,
        "yield"  => TokenKind::Yield,
        s        => TokenKind::Ident(s.to_owned()),
    }
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub enum LexError {
    UnexpectedChar(char, u32),
    UnterminatedString(u32),
    UnterminatedComment(u32),
    BadEscape(u32),
    BadNumber(u32),
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LexError::UnexpectedChar(c, l) => write!(f, "line {l}: unexpected character '{c}'"),
            LexError::UnterminatedString(l) => write!(f, "line {l}: unterminated string"),
            LexError::UnterminatedComment(l) => write!(f, "line {l}: unterminated comment"),
            LexError::BadEscape(l) => write!(f, "line {l}: invalid escape sequence"),
            LexError::BadNumber(l) => write!(f, "line {l}: malformed number literal"),
        }
    }
}

/// Parses a Lua-style hexadecimal float such as `0xA.8p1` (== 21.0). The
/// exponent is a power of two; without a `p`/`P` part the value is exact.
fn parse_hex_float(s: &str) -> Result<f64, ()> {
    let body = &s[2..];
    let (mantissa, exp) = match body.find(|c| c == 'p' || c == 'P') {
        Some(i) => (&body[..i], body[i + 1..].parse::<i32>().map_err(|_| ())?),
        None => (body, 0),
    };
    let (int_part, frac_part) = match mantissa.find('.') {
        Some(i) => (&mantissa[..i], &mantissa[i + 1..]),
        None => (mantissa, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return Err(());
    }
    let mut value = 0.0f64;
    for c in int_part.chars() {
        value = value * 16.0 + c.to_digit(16).ok_or(())? as f64;
    }
    let mut scale = 1.0f64 / 16.0;
    for c in frac_part.chars() {
        value += c.to_digit(16).ok_or(())? as f64 * scale;
        scale /= 16.0;
    }
    Ok(value * 2f64.powi(exp))
}
