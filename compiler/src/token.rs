//! Tokens and the lexer. Every token carries a `Span` so downstream errors
//! (parser, interpreter) can report structured, machine-checkable positions
//! instead of prose — the diagnostic shape goal.md row 9 asks for, started
//! here rather than bolted on later.

// `Hash` is for `refine.rs`, which keys a `HashSet<Span>` of proven-safe
// sites — every other consumer only needed equality/ordering before this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // literals & identifiers
    Int(i64),
    True,
    False,
    Ident(String),

    // keywords
    Fn,
    Let,
    Return,
    If,
    Else,
    While,
    Box,
    Spawn,
    Join,
    Thread,
    TypeName(String), // i8/i16/.../usize/bool/unit — validated by the parser

    // symbols
    LParen,
    RParen,
    LBrace,
    RBrace,
    Colon,
    Comma,
    Arrow, // ->
    Assign,
    Plus,
    Minus,
    Star,
    Slash,
    EqEq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    AndAnd,
    OrOr,
    Bang,
    Amp,

    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

const TYPE_NAMES: &[&str] = &[
    "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "usize", "bool", "unit",
];

#[derive(Debug, Clone)]
pub struct LexError {
    pub message: String,
    pub span: Span,
}

pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: usize,
    col: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer { src: src.as_bytes(), pos: 0, line: 1, col: 1 }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<u8> {
        self.src.get(self.pos + 1).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.pos += 1;
        if c == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn span(&self) -> Span {
        Span { line: self.line, col: self.col }
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') => {
                    self.bump();
                }
                Some(b'/') if self.peek2() == Some(b'/') => {
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                _ => break,
            }
        }
    }

    /// Produce the whole token stream in one pass. Single-token-lookahead
    /// parsing downstream never needs to re-enter the lexer mid-stream.
    pub fn tokenize(mut self) -> Result<Vec<Token>, LexError> {
        let mut out = Vec::new();
        loop {
            self.skip_ws_and_comments();
            let span = self.span();
            let c = match self.peek() {
                None => {
                    out.push(Token { tok: Tok::Eof, span });
                    break;
                }
                Some(c) => c,
            };

            if c.is_ascii_digit() {
                let start = self.pos;
                while self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                    self.bump();
                }
                let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap();
                let n: i64 = text.parse().map_err(|_| LexError {
                    message: format!("integer literal `{text}` out of range"),
                    span,
                })?;
                out.push(Token { tok: Tok::Int(n), span });
                continue;
            }

            if c.is_ascii_alphabetic() || c == b'_' {
                let start = self.pos;
                while self
                    .peek()
                    .map(|c| c.is_ascii_alphanumeric() || c == b'_')
                    .unwrap_or(false)
                {
                    self.bump();
                }
                let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap();
                let tok = match text {
                    "fn" => Tok::Fn,
                    "let" => Tok::Let,
                    "return" => Tok::Return,
                    "if" => Tok::If,
                    "else" => Tok::Else,
                    "while" => Tok::While,
                    "box" => Tok::Box,
                    "spawn" => Tok::Spawn,
                    "join" => Tok::Join,
                    "thread" => Tok::Thread,
                    "true" => Tok::True,
                    "false" => Tok::False,
                    t if TYPE_NAMES.contains(&t) => Tok::TypeName(t.to_string()),
                    t => Tok::Ident(t.to_string()),
                };
                out.push(Token { tok, span });
                continue;
            }

            // symbols — check two-char forms before falling back to one-char
            let two = self.peek2();
            let (tok, len) = match (c, two) {
                (b'-', Some(b'>')) => (Tok::Arrow, 2),
                (b'=', Some(b'=')) => (Tok::EqEq, 2),
                (b'!', Some(b'=')) => (Tok::NotEq, 2),
                (b'<', Some(b'=')) => (Tok::LtEq, 2),
                (b'>', Some(b'=')) => (Tok::GtEq, 2),
                (b'&', Some(b'&')) => (Tok::AndAnd, 2),
                (b'|', Some(b'|')) => (Tok::OrOr, 2),
                _ => {
                    let single = match c {
                        b'(' => Tok::LParen,
                        b')' => Tok::RParen,
                        b'{' => Tok::LBrace,
                        b'}' => Tok::RBrace,
                        b':' => Tok::Colon,
                        b',' => Tok::Comma,
                        b'=' => Tok::Assign,
                        b'+' => Tok::Plus,
                        b'-' => Tok::Minus,
                        b'*' => Tok::Star,
                        b'/' => Tok::Slash,
                        b'<' => Tok::Lt,
                        b'>' => Tok::Gt,
                        b'!' => Tok::Bang,
                        b'&' => Tok::Amp,
                        other => {
                            return Err(LexError {
                                message: format!(
                                    "unexpected character `{}`",
                                    other as char
                                ),
                                span,
                            })
                        }
                    };
                    (single, 1)
                }
            };
            for _ in 0..len {
                self.bump();
            }
            out.push(Token { tok, span });
        }
        Ok(out)
    }
}
