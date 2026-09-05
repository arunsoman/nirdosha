//! Tokens and the lexer. Every token carries a `Span` so downstream errors
//! (parser, interpreter) can report structured, machine-checkable positions
//! instead of prose — the diagnostic shape docs/goal.md row 9 asks for, started
//! here rather than bolted on later.

// `Hash` is for `refine.rs`, which keys a `HashSet<Span>` of proven-safe
// sites — every other consumer only needed equality/ordering before this.
// `Serialize`/`Deserialize` are for `lib.rs::Diagnostic` — every
// structured diagnostic (docs/goal.md row 9) carries a `Span`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Span {
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // literals & identifiers
    Int(i64),
    /// A decimal float literal (`3.14`) — digits, a `.`, digits, no
    /// scientific notation. Lexed as its own token, not `Int` followed by
    /// `Dot` followed by `Int`, so the parser never has to reassemble one
    /// (and never confuses it with, say, a future field-access `.`).
    Float(f64),
    Str(String),
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
    Chan,
    Send,
    Recv,
    Sandbox,
    Stop,
    Connect,
    Listen,
    Accept,
    /// `open(path, mode)` — see `Expr::Open`. A dedicated keyword, not
    /// folded into the generic builtin-call machinery, for the same
    /// reason `connect`/`listen` aren't: its result type (`Ty::File`)
    /// isn't inferable from a plain `ident(args)` call shape, so it needs
    /// its own grammar production the parser can special-case.
    Open,
    /// `effect(...)` — a `fn` declaration's optional effect annotation
    /// (`docs/PROTOLANG_PORT.md`'s "Locked design 1"). The five names inside
    /// the parens (`pure`/`rng`/`io`/`concurrent`/`network`) are
    /// deliberately **not** reserved keywords of their own — matched by
    /// identifier text inside `effect(...)`'s parens only
    /// (`parser.rs::parse_effect_annotation`), the same "keyword only
    /// within one specific syntactic slot" treatment `transact`'s own
    /// slot names (`network`/`verify`/`commit`/...) already get, so
    /// using `io` or `network` as an ordinary variable/function name
    /// elsewhere in a program stays legal.
    Effect,
    /// `requires(...)` — a `fn` declaration's optional privilege
    /// annotation (see `ast::Requirement`). `role`/`claim` inside the
    /// parens are deliberately **not** reserved keywords of their own —
    /// matched by identifier text inside `requires(...)`'s parens only
    /// (`parser.rs::parse_requires_annotation`), the same "keyword only
    /// within one specific syntactic slot" treatment `effect(...)`'s own
    /// names already get.
    Requires,
    /// `acquire name(proof)` — turns a `requires`-gated function into a
    /// first-class, callable value once `proof` (a `RoleView`/`ClaimView`)
    /// is checked against the requirement (`Expr::Acquire`). Shaped like
    /// `spawn`/`sandbox`: a keyword, then a plain call, destructured.
    Acquire,
    Audited,
    Transact,
    /// Row 11 (`docs/nirdosha_row11_amendment.md`) — `struct`/`enum`
    /// declarations and `match` expressions.
    Struct,
    Enum,
    Match,
    /// `screen <StructName> { ... }` — explicit, typechecked UI
    /// authoring for one struct, layered on top of (never replacing)
    /// `ui_gen.rs`'s pure convention-based inference for any struct with
    /// no `screen` block. A real reserved keyword (unlike `field`/
    /// `action`/`paginate`, matched by identifier text only *inside* a
    /// `screen`/`dashboard` body — the same "keyword only within one
    /// specific syntactic slot" treatment `effect(...)`/`transact`'s own
    /// inner names already get, and necessary here: `action` is already
    /// a real struct field/param name in `examples/trade-finance/
    /// trade_finance.nir`).
    Screen,
    /// `dashboard { tile "..." -> stat_fn  chart "..." -> chart_fn }` —
    /// supplements `ui_gen.rs`'s `stat_`/`chart_` naming-convention
    /// inference; see `Screen`'s doc comment for the same reserved-vs-
    /// contextual reasoning.
    Dashboard,
    /// `module "Display Name" { fn ... struct ... enum ... }` — pure
    /// nav-grouping sugar for `ui_gen.rs` (`docs/GRAMMAR.md`'s `module_decl`),
    /// not a real scoping/namespace construct: every declaration inside
    /// still registers into the exact same flat global namespace as a
    /// top-level one, just tagged with this module's display name. A
    /// real reserved keyword (like `Struct`/`Enum`/`Screen`/`Dashboard`
    /// above, unlike `field`/`action`/`paginate`'s contextual-only
    /// treatment) since no existing example uses "module" as an
    /// identifier.
    Module,
    /// `workflow Name { data { ... } state ... }` — top-level durable
    /// state-machine declaration (`docs/WORKFLOW.md`). A real reserved keyword
    /// like `Struct`/`Enum`/`Screen`/`Dashboard`/`Module` above; its body
    /// keywords (`data`/`on_entry`/`on_exit`/`on`/`terminal`/`link`) stay
    /// contextual-only (matched by identifier text inside
    /// `parser.rs::parse_workflow_decl` only), same "keyword only within
    /// one specific syntactic slot" treatment `transact`'s slot names and
    /// `screen`/`dashboard`'s inner names already get.
    Workflow,
    /// `state Name { ... }` — one state inside a `workflow` block. Real
    /// reserved keyword (unlike its own body's `on_entry`/`on_exit`/`on`,
    /// which are contextual) since `state` isn't otherwise used as an
    /// identifier anywhere in the existing examples, matching `Screen`'s
    /// own reserved-vs-contextual reasoning.
    State,
    /// `workspace Name { subject: Struct panel "..." { ... } }` — a
    /// composite multi-panel screen (`docs/ROADMAP.md` Track E1,
    /// `examples/ctms/UI_CONSTRUCTS.md` §1). A real reserved keyword,
    /// same reasoning as `Screen`/`Dashboard`/`Module`/`Workflow` above;
    /// its body's `panel` is contextual-only (matched by identifier text
    /// only inside `parser.rs::parse_workspace_decl`), the same
    /// "keyword only within one specific syntactic slot" treatment
    /// `screen`'s own `field`/`action`/`paginate` already get — disam-
    /// biguated from an ordinary `kv_entry` by its second token always
    /// being a `string`, never a `:`, so LL(1) holds with no
    /// second-token lookahead beyond that same one-token check.
    Workspace,
    /// `validate <fn_name> { pre: <expr>  post: <expr> ... }` — a Hoare
    /// contract on an existing fn (`docs/ROADMAP.md` Track F, F3;
    /// `docs/NEXT_GEN.md` §F3). A real reserved keyword, same reasoning as
    /// `Screen`/`Dashboard`/`Module`/`Workflow`/`Workspace` above (no
    /// existing example uses "validate" as an identifier); its body's
    /// `pre`/`post` are contextual-only `kv_entry` keys, the same
    /// "keyword only within one specific syntactic slot" treatment
    /// `screen`'s own `field`/`action`/`paginate` already get.
    Validate,
    /// `pub` — marks a declaration inside a real (identifier-named)
    /// `module Ident { ... }` block visible outside that module
    /// (`docs/ROADMAP.md` Track F, F2; `docs/NEXT_GEN.md` §F2). Meaningless
    /// (accepted, no effect) on a top-level or legacy string-named
    /// `module "Display Name" { ... }` declaration — those are always
    /// effectively public already, unchanged from before this existed.
    Pub,
    /// `use "relative/path.nir"` — a leading-only top-level item that
    /// imports another file's `pub` real-namespace declarations
    /// (`docs/ROADMAP.md` Track F, F2 piece 3). Only legal in the leading
    /// run at the very start of a program, before any other item.
    Use,
    /// `Vector`/`Matrix` in *type* position (`Vector(f64, 3)`) — deliberately
    /// capitalized, distinct from the lowercase `TypeName` scalars, matching
    /// the surface syntax the unified plan's architecture table already
    /// uses. Recognized by exact case-sensitive spelling in the identifier
    /// scanner (below), not folded into `TYPE_NAMES` since these two take
    /// `(...)` arguments — a genuinely different production, not another
    /// bare keyword.
    VectorKw,
    MatrixKw,
    /// `handle(KindName)` — `Ty::Handle`'s own syntax, the same
    /// `Name(args)` shape `Vector`/`Matrix` already use (a bare
    /// identifier argument, not a comma list, since a handle kind is
    /// nominal, not parameterized). Lowercase, unlike `Vector`/`Matrix`,
    /// since it names a *kind* of value (`box`/`thread`/`chan`/`sandbox`
    /// are all lowercase type-formers too), not a capitalized
    /// user-facing generic type.
    HandleKw,
    TypeName(String), // i8/i16/.../usize/f64/bool/unit/str — validated by the parser

    // symbols
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Comma,
    /// `expr.field` — Row 11 field access (`Expr::FieldAccess`). Not
    /// produced by digit-scanning (a float literal's own `.` is a
    /// separate, earlier lexer branch — see `Lexer::tokenize`'s digit
    /// case), only by this literal one-character symbol.
    Dot,
    Arrow,    // ->
    FatArrow, // => (a `match` arm's separator)
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
    DotStar,  // .*
    DotSlash, // ./
    /// `::` — qualified-name path separator (`Mod::Name`). Lexed as one
    /// token, not two `Colon`s, so `expr : Type` (an eventual annotation
    /// slot, none exists today) and `Mod::Name` can never be confused
    /// downstream — see `Lexer::tokenize`'s two-char dispatch, which
    /// checks this before falling back to a single `Colon`.
    ColonColon,

    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

const TYPE_NAMES: &[&str] = &[
    "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "usize", "f64", "dec128", "bool",
    "unit", "str", "tcp", "tcp_listener", "file", "json", "db", "mq",
];

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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

            if c == b'"' {
                self.bump(); // opening quote
                // Raw bytes, not `char`-by-`char` -- this lexer works over
                // `&[u8]` throughout (see `src`'s type), and pushing bytes
                // one at a time as `as char` would silently corrupt any
                // multi-byte UTF-8 content (each continuation byte would
                // become its own bogus Latin-1 codepoint). Collecting raw
                // bytes and validating the whole run as UTF-8 once, at the
                // end, is the only correct way to do this byte-wise.
                let mut bytes = Vec::new();
                loop {
                    match self.bump() {
                        None => {
                            return Err(LexError { message: "unterminated string literal".to_string(), span });
                        }
                        Some(b'"') => break,
                        // Minimal, deliberately small escape set -- just
                        // enough to write a quote, a backslash, a
                        // newline/tab/carriage-return (`\r\n` matters for
                        // real reasons -- e.g. HTTP request lines over
                        // `tcp` genuinely need it, not a hypothetical);
                        // not a full escape grammar (no \u{...}, no \x..,
                        // no \0). Nothing needing those exists yet.
                        Some(b'\\') => match self.bump() {
                            Some(b'"') => bytes.push(b'"'),
                            Some(b'\\') => bytes.push(b'\\'),
                            Some(b'n') => bytes.push(b'\n'),
                            Some(b't') => bytes.push(b'\t'),
                            Some(b'r') => bytes.push(b'\r'),
                            Some(other) => {
                                return Err(LexError {
                                    message: format!("unknown escape `\\{}`", other as char),
                                    span,
                                });
                            }
                            None => {
                                return Err(LexError {
                                    message: "unterminated string literal".to_string(),
                                    span,
                                });
                            }
                        },
                        Some(b) => bytes.push(b),
                    }
                }
                let s = String::from_utf8(bytes)
                    .map_err(|_| LexError { message: "string literal is not valid UTF-8".to_string(), span })?;
                out.push(Token { tok: Tok::Str(s), span });
                continue;
            }

            if c.is_ascii_digit() {
                let start = self.pos;
                while self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                    self.bump();
                }
                // A `.` immediately followed by a digit turns this into a
                // float literal instead — checked with `peek2`, not just
                // `peek`, so a bare trailing `.` (nothing this language
                // has a use for yet — no method-call/field-access syntax)
                // doesn't get swallowed into a malformed float. `1..` or
                // `1.` alone stays a plain `Int(1)` followed by whatever
                // the `.` actually is (currently: a lex error, same as
                // today — no production accepts a bare `.` yet).
                let is_float = self.peek() == Some(b'.') && self.peek2().map(|c| c.is_ascii_digit()).unwrap_or(false);
                if is_float {
                    self.bump(); // '.'
                    while self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                        self.bump();
                    }
                    let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap();
                    let f: f64 = text.parse().map_err(|_| LexError {
                        message: format!("float literal `{text}` is not valid"),
                        span,
                    })?;
                    out.push(Token { tok: Tok::Float(f), span });
                    continue;
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
                    "chan" => Tok::Chan,
                    "send" => Tok::Send,
                    "recv" => Tok::Recv,
                    "sandbox" => Tok::Sandbox,
                    "stop" => Tok::Stop,
                    "connect" => Tok::Connect,
                    "listen" => Tok::Listen,
                    "accept" => Tok::Accept,
                    "open" => Tok::Open,
                    "effect" => Tok::Effect,
                    "requires" => Tok::Requires,
                    "acquire" => Tok::Acquire,
                    "audited" => Tok::Audited,
                    "transact" => Tok::Transact,
                    "struct" => Tok::Struct,
                    "enum" => Tok::Enum,
                    "match" => Tok::Match,
                    "screen" => Tok::Screen,
                    "dashboard" => Tok::Dashboard,
                    "module" => Tok::Module,
                    "workflow" => Tok::Workflow,
                    "state" => Tok::State,
                    "workspace" => Tok::Workspace,
                    "validate" => Tok::Validate,
                    "pub" => Tok::Pub,
                    "use" => Tok::Use,
                    "Vector" => Tok::VectorKw,
                    "Matrix" => Tok::MatrixKw,
                    "handle" => Tok::HandleKw,
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
                (b'=', Some(b'>')) => (Tok::FatArrow, 2),
                (b'=', Some(b'=')) => (Tok::EqEq, 2),
                (b'!', Some(b'=')) => (Tok::NotEq, 2),
                (b'<', Some(b'=')) => (Tok::LtEq, 2),
                (b'>', Some(b'=')) => (Tok::GtEq, 2),
                (b'&', Some(b'&')) => (Tok::AndAnd, 2),
                (b'|', Some(b'|')) => (Tok::OrOr, 2),
                (b'.', Some(b'*')) => (Tok::DotStar, 2),
                (b'.', Some(b'/')) => (Tok::DotSlash, 2),
                (b':', Some(b':')) => (Tok::ColonColon, 2),
                _ => {
                    let single = match c {
                        b'(' => Tok::LParen,
                        b')' => Tok::RParen,
                        b'{' => Tok::LBrace,
                        b'}' => Tok::RBrace,
                        b'[' => Tok::LBracket,
                        b']' => Tok::RBracket,
                        b':' => Tok::Colon,
                        b',' => Tok::Comma,
                        b'.' => Tok::Dot,
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
