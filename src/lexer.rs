#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Int(i64),
    Float(f64),
    String(String),
    Ident(String),

    And,
    Break,
    Else,
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
    True,
    Var,
    While,
    Yield,

    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    Hash,
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

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
                self.advance();
            }
            if self.peek() == Some(b'/') && self.peek2() == Some(b'/') {
                self.advance(); self.advance();
                while !matches!(self.peek(), Some(b'\n') | None) { self.advance(); }
                continue;
            }
            if self.peek() == Some(b'/') && self.peek2() == Some(b'*') {
                self.advance(); self.advance();
                loop {
                    match self.advance() {
                        None => break,
                        Some(b'*') if self.peek() == Some(b'/') => { self.advance(); break; }
                        _ => {}
                    }
                }
                continue;
            }
            break;
        }
    }

    /// Assumes `pos` is already past the opening `[`; returns the `=` level
    /// if this is a valid long-bracket opener, else -1.
    fn count_long_bracket(&self) -> i32 {
        let mut i = self.pos;
        let mut level = 0usize;
        while self.src.get(i) == Some(&b'=') { level += 1; i += 1; }
        if self.src.get(i) == Some(&b'[') { level as i32 } else { -1 }
    }

    fn skip_long_string(&mut self, level: usize) {
        for _ in 0..level { self.advance(); }
        self.advance();
        loop {
            match self.advance() {
                None => break,
                Some(b']') => {
                    let mut n = 0;
                    while self.peek() == Some(b'=') { self.advance(); n += 1; }
                    if n == level && self.peek() == Some(b']') { self.advance(); break; }
                }
                _ => {}
            }
        }
    }

    fn read_long_string(&mut self, level: usize) -> String {
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
                None => break,
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
        String::from_utf8_lossy(&out).into_owned()
    }

    fn read_string(&mut self, delim: u8) -> Result<String, LexError> {
        let mut out = Vec::new();
        loop {
            match self.advance() {
                None | Some(b'\n') => return Err(LexError::UnterminatedString(self.line)),
                Some(b) if b == delim => break,
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
                        Some(b'\n') | Some(b'\r') => out.push(b'\n'),
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
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn read_number(&mut self, first: u8) -> Result<TokenKind, LexError> {
        let start = self.pos - 1;
        let line = self.line;
        if first == b'0' && matches!(self.peek(), Some(b'x' | b'X')) {
            self.advance();
            while matches!(self.peek(), Some(b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F' | b'_')) {
                self.advance();
            }
            let s: String = self.src[start..self.pos].iter()
                .filter(|&&b| b != b'_')
                .map(|&b| b as char).collect();
            return i64::from_str_radix(&s[2..], 16)
                .map(TokenKind::Int)
                .map_err(|_| LexError::BadNumber(line));
        }
        let mut is_float = false;
        while matches!(self.peek(), Some(b'0'..=b'9' | b'_')) { self.advance(); }
        if self.peek() == Some(b'.') && matches!(self.peek2(), Some(b'0'..=b'9')) {
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
            s.parse().map(TokenKind::Int).map_err(|_| LexError::BadNumber(line))
        }
    }

    pub fn next_token(&mut self) -> Result<Token, LexError> {
        self.skip_whitespace_and_comments();
        let line = self.line;
        let b = match self.advance() {
            None => return Ok(Token { kind: TokenKind::Eof, line }),
            Some(b) => b,
        };

        let kind = match b {
            b'+' => TokenKind::Plus,
            b'*' => TokenKind::Star,
            b'%' => TokenKind::Percent,
            b'^' => TokenKind::Caret,
            b'#' => TokenKind::Hash,
            b'&' => TokenKind::Amp,
            b'|' => TokenKind::Pipe,
            b'(' => TokenKind::LParen,
            b')' => TokenKind::RParen,
            b'{' => TokenKind::LBrace,
            b'}' => TokenKind::RBrace,
            b']' => TokenKind::RBracket,
            b';' => TokenKind::Semicolon,
            b',' => TokenKind::Comma,
            b'-' => TokenKind::Minus,
            b'/' => TokenKind::Slash,
            b'~' => TokenKind::Tilde,
            b'!' => if self.eat(b'=') { TokenKind::BangEq }
                    else { return Err(LexError::UnexpectedChar('!', line)) },
            b'<' => if self.eat(b'<') { TokenKind::LtLt }
                    else if self.eat(b'=') { TokenKind::LtEq }
                    else { TokenKind::Lt },
            b'>' => if self.eat(b'>') { TokenKind::GtGt }
                    else if self.eat(b'=') { TokenKind::GtEq }
                    else { TokenKind::Gt },
            b'=' => if self.eat(b'=') { TokenKind::Eq } else { TokenKind::Assign },
            b':' => if self.eat(b':') { TokenKind::ColonColon } else { TokenKind::Colon },
            b'.' => {
                if self.peek() == Some(b'.') {
                    self.advance();
                    if self.eat(b'.') { TokenKind::DotDotDot } else { TokenKind::DotDot }
                } else if matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.read_number(b'0')?
                } else {
                    TokenKind::Dot
                }
            }
            b'[' => {
                let level = self.count_long_bracket();
                if level >= 0 {
                    TokenKind::String(self.read_long_string(level as usize))
                } else {
                    TokenKind::LBracket
                }
            }
            b'\'' | b'"' => TokenKind::String(self.read_string(b)?),
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
        "and"    => TokenKind::And,
        "break"  => TokenKind::Break,
        "else"   => TokenKind::Else,
        "false"  => TokenKind::False,
        "fn"     => TokenKind::Fn,
        "for"    => TokenKind::For,
        "if"     => TokenKind::If,
        "in"     => TokenKind::In,
        "let"    => TokenKind::Let,
        "none"   => TokenKind::None,
        "not"    => TokenKind::Not,
        "or"     => TokenKind::Or,
        "return" => TokenKind::Return,
        "true"   => TokenKind::True,
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
    BadEscape(u32),
    BadNumber(u32),
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LexError::UnexpectedChar(c, l) => write!(f, "line {l}: unexpected character '{c}'"),
            LexError::UnterminatedString(l) => write!(f, "line {l}: unterminated string"),
            LexError::BadEscape(l) => write!(f, "line {l}: invalid escape sequence"),
            LexError::BadNumber(l) => write!(f, "line {l}: malformed number literal"),
        }
    }
}
