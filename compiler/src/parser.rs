//! Recursive-descent parser. Every `parse_*` function looks at exactly one
//! token — `self.peek()` — before deciding what to do, and never puts a
//! token back. That single-token-lookahead, no-backtracking discipline is
//! what makes this LL(1) by construction; see GRAMMAR.md for the claim and
//! its scope. Mirrors the EBNF in GRAMMAR.md production-for-production, so
//! the two stay honest about each other — if they drift, one of them is
//! wrong.

use crate::ast::*;
use crate::token::{Span, Tok, Token};

#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

type PResult<T> = Result<T, ParseError>;

impl Parser {
    pub fn new(toks: Vec<Token>) -> Self {
        Parser { toks, pos: 0 }
    }

    fn peek(&self) -> &Token {
        &self.toks[self.pos]
    }

    fn span(&self) -> Span {
        self.peek().span
    }

    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn expect(&mut self, want: &Tok, what: &str) -> PResult<Token> {
        if &self.peek().tok == want {
            Ok(self.bump())
        } else {
            Err(ParseError {
                message: format!("expected {what}, found {:?}", self.peek().tok),
                span: self.span(),
            })
        }
    }

    fn expect_ident(&mut self) -> PResult<String> {
        match &self.peek().tok {
            Tok::Ident(s) => {
                let s = s.clone();
                self.bump();
                Ok(s)
            }
            other => Err(ParseError {
                message: format!("expected identifier, found {other:?}"),
                span: self.span(),
            }),
        }
    }

    /// A bare non-negative integer literal in *type* position — a
    /// `Vector`/`Matrix` dimension. Dimensions are fixed at compile time
    /// ("Sized by Default" — the unified plan's §2), so this only ever
    /// accepts a literal, never an arbitrary expression the way an array
    /// index does.
    fn expect_usize_literal(&mut self) -> PResult<usize> {
        let span = self.span();
        match self.peek().tok.clone() {
            Tok::Int(n) => {
                self.bump();
                usize::try_from(n).map_err(|_| ParseError {
                    message: format!("`{n}` is not a valid Vector/Matrix dimension (must be non-negative)"),
                    span,
                })
            }
            other => Err(ParseError { message: format!("expected a dimension, found {other:?}"), span }),
        }
    }

    // type ::= "&" type | "box" type | i8 | i16 | ... | bool | unit
    fn expect_type(&mut self) -> PResult<Ty> {
        if self.peek().tok == Tok::Amp {
            self.bump();
            let inner = self.expect_type()?;
            return Ok(Ty::Ref(Box::new(inner)));
        }
        if self.peek().tok == Tok::Box {
            self.bump();
            let inner = self.expect_type()?;
            return Ok(Ty::Box(Box::new(inner)));
        }
        if self.peek().tok == Tok::Thread {
            self.bump();
            let inner = self.expect_type()?;
            return Ok(Ty::Thread(Box::new(inner)));
        }
        if self.peek().tok == Tok::Chan {
            self.bump();
            let inner = self.expect_type()?;
            return Ok(Ty::Channel(Box::new(inner)));
        }
        // Unlike `box`/`thread`/`chan`, `sandbox` takes no inner type — it
        // doesn't recurse into `expect_type()` again, it just resolves
        // directly, the mirror image of `chan` being a bare keyword in
        // *expression* position (see `parse_unary` below) but a wrapping
        // type-former here.
        if self.peek().tok == Tok::Sandbox {
            self.bump();
            return Ok(Ty::Sandbox);
        }
        if self.peek().tok == Tok::VectorKw {
            self.bump();
            self.expect(&Tok::LParen, "`(`")?;
            let elem = self.expect_type()?;
            self.expect(&Tok::Comma, "`,`")?;
            let n = self.expect_usize_literal()?;
            self.expect(&Tok::RParen, "`)`")?;
            return Ok(Ty::Vector(Box::new(elem), n));
        }
        if self.peek().tok == Tok::MatrixKw {
            self.bump();
            self.expect(&Tok::LParen, "`(`")?;
            let elem = self.expect_type()?;
            self.expect(&Tok::Comma, "`,`")?;
            let rows = self.expect_usize_literal()?;
            self.expect(&Tok::Comma, "`,`")?;
            let cols = self.expect_usize_literal()?;
            self.expect(&Tok::RParen, "`)`")?;
            return Ok(Ty::Matrix(Box::new(elem), rows, cols));
        }
        match &self.peek().tok {
            Tok::TypeName(name) => {
                let ty = Ty::from_name(name).expect("lexer only emits valid type names");
                self.bump();
                Ok(ty)
            }
            // A declared `struct`/`enum` name (Row 11) — accepted
            // syntactically for *any* identifier here, same as a function
            // call name; whether it actually names a real declared type
            // is `typeck.rs`'s job (`TypeErrorKind::UnknownType`), not
            // this parser's — it has no declaration table to check
            // against, and Nirdosha's functions/types are already
            // resolved by name in a table built after parsing completes
            // (`LANGUAGE.md` §6), not as each name is scanned.
            Tok::Ident(name) => {
                let n = name.clone();
                self.bump();
                Ok(Ty::Named(n))
            }
            other => Err(ParseError {
                message: format!("expected a type, found {other:?}"),
                span: self.span(),
            }),
        }
    }

    // program ::= item*
    pub fn parse_program(&mut self) -> PResult<Program> {
        let mut fns = Vec::new();
        let mut structs = Vec::new();
        let mut enums = Vec::new();
        while self.peek().tok != Tok::Eof {
            match self.peek().tok {
                Tok::Struct => structs.push(self.parse_struct_decl()?),
                Tok::Enum => enums.push(self.parse_enum_decl()?),
                _ => fns.push(self.parse_fn_decl()?),
            }
        }
        Ok(Program { fns, structs, enums })
    }

    // struct_decl ::= "struct" ident "{" field ("," field)* ","? "}"
    // field       ::= ident ":" type
    fn parse_struct_decl(&mut self) -> PResult<StructDecl> {
        let span = self.span();
        self.expect(&Tok::Struct, "`struct`")?;
        let name = self.expect_ident()?;
        self.expect(&Tok::LBrace, "`{`")?;
        let mut fields = Vec::new();
        if self.peek().tok != Tok::RBrace {
            loop {
                let fname = self.expect_ident()?;
                self.expect(&Tok::Colon, "`:`")?;
                let fty = self.expect_type()?;
                fields.push(Field { name: fname, ty: fty });
                if self.peek().tok == Tok::Comma {
                    self.bump();
                    if self.peek().tok == Tok::RBrace {
                        break; // trailing comma
                    }
                    continue;
                }
                break;
            }
        }
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(StructDecl { name, fields, span })
    }

    // enum_decl ::= "enum" ident "{" variant ("," variant)* ","? "}"
    // variant   ::= ident ("(" type ("," type)* ")")?
    fn parse_enum_decl(&mut self) -> PResult<EnumDecl> {
        let span = self.span();
        self.expect(&Tok::Enum, "`enum`")?;
        let name = self.expect_ident()?;
        self.expect(&Tok::LBrace, "`{`")?;
        let mut variants = Vec::new();
        if self.peek().tok != Tok::RBrace {
            loop {
                let vspan = self.span();
                let vname = self.expect_ident()?;
                let mut payload = Vec::new();
                if self.peek().tok == Tok::LParen {
                    self.bump();
                    if self.peek().tok != Tok::RParen {
                        loop {
                            payload.push(self.expect_type()?);
                            if self.peek().tok == Tok::Comma {
                                self.bump();
                                continue;
                            }
                            break;
                        }
                    }
                    self.expect(&Tok::RParen, "`)`")?;
                }
                variants.push(Variant { name: vname, payload, span: vspan });
                if self.peek().tok == Tok::Comma {
                    self.bump();
                    if self.peek().tok == Tok::RBrace {
                        break; // trailing comma
                    }
                    continue;
                }
                break;
            }
        }
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(EnumDecl { name, variants, span })
    }

    // fn_decl ::= "fn" ident "(" params? ")" ("->" type)? block
    fn parse_fn_decl(&mut self) -> PResult<FnDecl> {
        let span = self.span();
        self.expect(&Tok::Fn, "`fn`")?;
        let name = self.expect_ident()?;
        self.expect(&Tok::LParen, "`(`")?;
        let mut params = Vec::new();
        if self.peek().tok != Tok::RParen {
            loop {
                let pname = self.expect_ident()?;
                self.expect(&Tok::Colon, "`:`")?;
                let pty = self.expect_type()?;
                params.push(Param { name: pname, ty: pty });
                if self.peek().tok == Tok::Comma {
                    self.bump();
                    continue;
                }
                break;
            }
        }
        self.expect(&Tok::RParen, "`)`")?;
        let ret = if self.peek().tok == Tok::Arrow {
            self.bump();
            self.expect_type()?
        } else {
            Ty::Unit
        };
        let declared_effects = self.parse_effect_annotation()?;
        let body = self.parse_block()?;
        Ok(FnDecl { name, params, ret, body, span, declared_effects })
    }

    /// `effect(pure)` / `effect(io, network)` / ... — `None` if there's no
    /// `effect(...)` at all (the common, fully-inferred case). `pure`
    /// denotes the empty set and must appear alone (`effect(pure, io)` is
    /// a parse error, not silently "just `io`") — see `ast::FnDecl::
    /// declared_effects`'s doc comment for what an empty set means.
    fn parse_effect_annotation(&mut self) -> PResult<Option<std::collections::BTreeSet<Effect>>> {
        if self.peek().tok != Tok::Effect {
            return Ok(None);
        }
        let ann_span = self.span();
        self.bump();
        self.expect(&Tok::LParen, "`(`")?;
        let mut saw_pure = false;
        let mut set = std::collections::BTreeSet::new();
        loop {
            let span = self.span();
            let name = self.expect_ident()?;
            match name.as_str() {
                "pure" => saw_pure = true,
                "rng" => {
                    set.insert(Effect::Rng);
                }
                "io" => {
                    set.insert(Effect::Io);
                }
                "concurrent" => {
                    set.insert(Effect::Concurrent);
                }
                "network" => {
                    set.insert(Effect::Network);
                }
                other => {
                    return Err(ParseError {
                        message: format!(
                            "unknown effect `{other}` — expected `pure`, `rng`, `io`, `concurrent`, or `network`"
                        ),
                        span,
                    });
                }
            }
            if self.peek().tok == Tok::Comma {
                self.bump();
                continue;
            }
            break;
        }
        self.expect(&Tok::RParen, "`)`")?;
        if saw_pure && !set.is_empty() {
            return Err(ParseError {
                message: "`pure` declares the empty effect set and can't be combined with other effects".to_string(),
                span: ann_span,
            });
        }
        Ok(Some(set))
    }

    // block ::= "{" stmt* "}"
    fn parse_block(&mut self) -> PResult<Block> {
        self.expect(&Tok::LBrace, "`{`")?;
        let mut stmts = Vec::new();
        while self.peek().tok != Tok::RBrace {
            stmts.push(self.parse_stmt()?);
        }
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(Block { stmts })
    }

    // stmt ::= let_stmt | return_stmt | while_stmt | expr_stmt
    fn parse_stmt(&mut self) -> PResult<Stmt> {
        match &self.peek().tok {
            Tok::Let => self.parse_let_stmt(),
            Tok::Return => self.parse_return_stmt(),
            Tok::While => self.parse_while_stmt(),
            Tok::Audited => self.parse_audited_stmt(),
            _ => Ok(Stmt::Expr(self.parse_expr()?)),
        }
    }

    // audited_stmt ::= "audited" str_lit block
    fn parse_audited_stmt(&mut self) -> PResult<Stmt> {
        let span = self.span();
        self.expect(&Tok::Audited, "`audited`")?;
        let justification = match self.peek().tok.clone() {
            Tok::Str(s) => {
                self.bump();
                s
            }
            other => {
                return Err(ParseError {
                    message: format!("expected a string literal justification after `audited`, found {other:?}"),
                    span: self.span(),
                })
            }
        };
        let body = self.parse_block()?;
        Ok(Stmt::Audited { justification, body: body.stmts, span })
    }

    fn parse_let_stmt(&mut self) -> PResult<Stmt> {
        let span = self.span();
        self.expect(&Tok::Let, "`let`")?;
        let name = self.expect_ident()?;
        self.expect(&Tok::Colon, "`:`")?;
        let ty = self.expect_type()?;
        self.expect(&Tok::Assign, "`=`")?;
        let value = self.parse_expr()?;
        Ok(Stmt::Let { name, ty, value, span })
    }

    fn parse_return_stmt(&mut self) -> PResult<Stmt> {
        let span = self.span();
        self.expect(&Tok::Return, "`return`")?;
        let value = if matches!(self.peek().tok, Tok::RBrace) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        Ok(Stmt::Return { value, span })
    }

    fn parse_while_stmt(&mut self) -> PResult<Stmt> {
        let span = self.span();
        self.expect(&Tok::While, "`while`")?;
        let cond = self.parse_expr()?;
        let body = self.parse_block()?;
        Ok(Stmt::While { cond, body, span })
    }

    // expr ::= if_expr | transact_expr | match_expr | assignment
    fn parse_expr(&mut self) -> PResult<Expr> {
        if self.peek().tok == Tok::If {
            self.parse_if_expr()
        } else if self.peek().tok == Tok::Transact {
            self.parse_transact_expr()
        } else if self.peek().tok == Tok::Match {
            self.parse_match_expr()
        } else {
            self.parse_assignment()
        }
    }

    // match_expr ::= "match" expr "{" match_arm ("," match_arm)* ","? "}"
    // match_arm  ::= ident ("(" ident ("," ident)* ")")? "=>" expr
    // Scrutinee uses full `parse_expr`, same as `if`'s own `cond` does —
    // no struct-literal-shaped expression exists in this grammar to make
    // "expr immediately followed by `{`" ambiguous the way it can be in
    // languages with brace-delimited struct literals (construction here
    // is always `Ident(args)`, never `Ident { .. }` — see `StructDecl`'s
    // doc comment).
    fn parse_match_expr(&mut self) -> PResult<Expr> {
        let span = self.span();
        self.expect(&Tok::Match, "`match`")?;
        let scrutinee = Box::new(self.parse_expr()?);
        self.expect(&Tok::LBrace, "`{`")?;
        let mut arms = Vec::new();
        loop {
            let arm_span = self.span();
            let variant = self.expect_ident()?;
            let mut bindings = Vec::new();
            if self.peek().tok == Tok::LParen {
                self.bump();
                if self.peek().tok != Tok::RParen {
                    loop {
                        bindings.push(self.expect_ident()?);
                        if self.peek().tok == Tok::Comma {
                            self.bump();
                            continue;
                        }
                        break;
                    }
                }
                self.expect(&Tok::RParen, "`)`")?;
            }
            self.expect(&Tok::FatArrow, "`=>`")?;
            let body = Box::new(self.parse_expr()?);
            arms.push(MatchArm { variant, bindings, body, span: arm_span });
            if self.peek().tok == Tok::Comma {
                self.bump();
                if self.peek().tok == Tok::RBrace {
                    break; // trailing comma
                }
                continue;
            }
            break;
        }
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(Expr::Match { scrutinee, arms, span })
    }

    // transact_expr ::= "transact" "{"
    //                      "network"    ":" call
    //                      "verify"     ":" call
    //                      "commit"     ":" call
    //                      ("compensate" ":" call)?
    //                      ("log"        ":" call)?
    //                    "}"
    // TRANSACT.md's Layer 1 grammar -- no `retry`/`timeout` on `network`
    // yet (Layer 2), no durability log (Layer 3+). Slot order is fixed,
    // not permutation-parsed: "no permutation parsing, keeps this LL(1)
    // the same way every other fixed-arity form in this grammar already
    // is" (TRANSACT.md).
    fn parse_transact_expr(&mut self) -> PResult<Expr> {
        let span = self.span();
        self.expect(&Tok::Transact, "`transact`")?;
        self.expect(&Tok::LBrace, "`{`")?;
        let network = self.parse_transact_slot("network")?;
        let verify = self.parse_transact_slot("verify")?;
        let commit = self.parse_transact_slot("commit")?;
        let compensate = self.parse_optional_transact_slot("compensate")?;
        let log = self.parse_optional_transact_slot("log")?;
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(Expr::Transact { network, verify, commit, compensate, log, span })
    }

    // One `label ":" call` slot. `label` is a fixed word, matched by
    // identifier text rather than promoted to a global `Tok` keyword: it
    // only ever appears in this one fixed grammar position, so reserving
    // it everywhere would cost every other identifier position the
    // ability to use it, for no benefit — the same reasoning `TYPE_NAMES`
    // (token.rs) already applies to scalar type names, which aren't `Tok`
    // keywords either.
    fn parse_transact_slot(&mut self, label: &str) -> PResult<TransactSlot> {
        let span = self.span();
        match &self.peek().tok {
            Tok::Ident(s) if s == label => {
                self.bump();
            }
            other => {
                return Err(ParseError {
                    message: format!("expected `{label}` in `transact`, found {other:?}"),
                    span,
                });
            }
        }
        self.expect(&Tok::Colon, "`:`")?;
        // Same "parse normally, then validate what came out" restriction
        // `spawn`/`sandbox` already enforce — a slot is exactly one named
        // call, never an arbitrary expression (TRANSACT.md's "Decisions").
        let call = self.parse_call()?;
        match call {
            Expr::Call(name, args, call_span) => Ok(TransactSlot { name, args, span: call_span }),
            _ => Err(ParseError {
                message: format!(
                    "`{label}` in `transact` requires a plain function call, e.g. `{label}: worker(x)`"
                ),
                span,
            }),
        }
    }

    fn parse_optional_transact_slot(&mut self, label: &str) -> PResult<Option<TransactSlot>> {
        match &self.peek().tok {
            Tok::Ident(s) if s == label => Ok(Some(self.parse_transact_slot(label)?)),
            _ => Ok(None),
        }
    }

    // assignment ::= ident "=" assignment | logic_or
    // See GRAMMAR.md for why "parse logic_or, then check for a trailing `=`
    // on a bare Ident" is the LL(1)-faithful way to write this, rather than
    // trying `ident "="` as a distinct first alternative.
    fn parse_assignment(&mut self) -> PResult<Expr> {
        let lhs = self.parse_logic_or()?;
        if self.peek().tok == Tok::Assign {
            let eq_span = self.span();
            match lhs {
                Expr::Ident(name, span) => {
                    self.bump(); // `=`
                    let rhs = self.parse_assignment()?; // right-associative
                    Ok(Expr::Assign(name, Box::new(rhs), span))
                }
                _ => Err(ParseError {
                    message: "left-hand side of `=` must be a plain variable name".to_string(),
                    span: eq_span,
                }),
            }
        } else {
            Ok(lhs)
        }
    }

    fn parse_if_expr(&mut self) -> PResult<Expr> {
        let span = self.span();
        self.expect(&Tok::If, "`if`")?;
        let cond = Box::new(self.parse_expr()?);
        let then_block = self.parse_block()?;
        let else_block = if self.peek().tok == Tok::Else {
            self.bump();
            if self.peek().tok == Tok::If {
                Some(Box::new(ElseBranch::If(self.parse_if_expr()?)))
            } else {
                Some(Box::new(ElseBranch::Block(self.parse_block()?)))
            }
        } else {
            None
        };
        Ok(Expr::If { cond, then_block, else_block, span })
    }

    // Precedence climbing, lowest to highest — this is what keeps the
    // expression grammar LL(1) without left recursion (GRAMMAR.md).
    fn parse_logic_or(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_logic_and()?;
        while self.peek().tok == Tok::OrOr {
            let span = self.span();
            self.bump();
            let rhs = self.parse_logic_and()?;
            lhs = Expr::Binary(BinOp::Or, Box::new(lhs), Box::new(rhs), span);
        }
        Ok(lhs)
    }

    fn parse_logic_and(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_equality()?;
        while self.peek().tok == Tok::AndAnd {
            let span = self.span();
            self.bump();
            let rhs = self.parse_equality()?;
            lhs = Expr::Binary(BinOp::And, Box::new(lhs), Box::new(rhs), span);
        }
        Ok(lhs)
    }

    fn parse_equality(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_comparison()?;
        loop {
            let op = match self.peek().tok {
                Tok::EqEq => BinOp::Eq,
                Tok::NotEq => BinOp::NotEq,
                _ => break,
            };
            let span = self.span();
            self.bump();
            let rhs = self.parse_comparison()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs), span);
        }
        Ok(lhs)
    }

    fn parse_comparison(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_additive()?;
        loop {
            let op = match self.peek().tok {
                Tok::Lt => BinOp::Lt,
                Tok::Gt => BinOp::Gt,
                Tok::LtEq => BinOp::LtEq,
                Tok::GtEq => BinOp::GtEq,
                _ => break,
            };
            let span = self.span();
            self.bump();
            let rhs = self.parse_additive()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs), span);
        }
        Ok(lhs)
    }

    fn parse_additive(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            let op = match self.peek().tok {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            let span = self.span();
            self.bump();
            let rhs = self.parse_multiplicative()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs), span);
        }
        Ok(lhs)
    }

    fn parse_multiplicative(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match self.peek().tok {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::DotStar => BinOp::ElemMul,
                Tok::DotSlash => BinOp::ElemDiv,
                _ => break,
            };
            let span = self.span();
            self.bump();
            let rhs = self.parse_unary()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs), span);
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        let span = self.span();
        match self.peek().tok {
            Tok::Bang => {
                self.bump();
                Ok(Expr::Unary(UnOp::Not, Box::new(self.parse_unary()?), span))
            }
            Tok::Minus => {
                self.bump();
                Ok(Expr::Unary(UnOp::Neg, Box::new(self.parse_unary()?), span))
            }
            // `*` here is unary deref, not multiplication — `multiplicative`
            // only ever sees `*` in infix position, after a full `unary`
            // has already been parsed, so there's no ambiguity to resolve:
            // the current token alone (and the fact we're at the start of
            // a `unary`, not mid-expression) determines which one this is.
            Tok::Star => {
                self.bump();
                Ok(Expr::Deref(Box::new(self.parse_unary()?), span))
            }
            Tok::Box => {
                self.bump();
                Ok(Expr::Box(Box::new(self.parse_unary()?), span))
            }
            Tok::Amp => {
                self.bump();
                let operand = self.parse_unary()?;
                // Restricted to a plain name — see `Expr::Ref`'s doc
                // comment for why. Same pattern as `parse_assignment`'s
                // "left-hand side must be a plain variable name" check:
                // parse normally, then validate what came out.
                match operand {
                    Expr::Ident(..) => Ok(Expr::Ref(Box::new(operand), span)),
                    _ => Err(ParseError {
                        message: "`&` can only borrow a plain variable name".to_string(),
                        span,
                    }),
                }
            }
            Tok::Spawn => {
                self.bump();
                // Restricted to a plain call, same "parse normally, then
                // validate" pattern as `&`'s Ident restriction above —
                // `spawn` runs a *named function*, not an arbitrary
                // expression, so it reuses `parse_call` and destructures
                // the result rather than inventing a separate production
                // that would just duplicate `call`'s grammar.
                let operand = self.parse_call()?;
                match operand {
                    Expr::Call(name, args, _) => Ok(Expr::Spawn(name, args, span)),
                    _ => Err(ParseError {
                        message: "`spawn` requires a function call, e.g. `spawn worker(x)`".to_string(),
                        span,
                    }),
                }
            }
            Tok::Join => {
                self.bump();
                Ok(Expr::Join(Box::new(self.parse_unary()?), span))
            }
            Tok::Chan => {
                self.bump();
                Ok(Expr::Chan(span))
            }
            Tok::Send => {
                self.bump();
                self.expect(&Tok::LParen, "`(`")?;
                let chan = self.parse_expr()?;
                self.expect(&Tok::Comma, "`,`")?;
                let value = self.parse_expr()?;
                self.expect(&Tok::RParen, "`)`")?;
                Ok(Expr::Send(Box::new(chan), Box::new(value), span))
            }
            Tok::Recv => {
                self.bump();
                self.expect(&Tok::LParen, "`(`")?;
                let chan = self.parse_expr()?;
                self.expect(&Tok::RParen, "`)`")?;
                Ok(Expr::Recv(Box::new(chan), span))
            }
            Tok::Sandbox => {
                self.bump();
                // Same "parse normally, then validate what came out"
                // technique `spawn` already uses — `sandbox` runs a named
                // function as a separate process, not an arbitrary
                // expression.
                let operand = self.parse_call()?;
                match operand {
                    Expr::Call(name, args, _) => Ok(Expr::SpawnSandbox(name, args, span)),
                    _ => Err(ParseError {
                        message: "`sandbox` requires a function call, e.g. `sandbox worker(x)`".to_string(),
                        span,
                    }),
                }
            }
            Tok::Stop => {
                self.bump();
                Ok(Expr::StopSandbox(Box::new(self.parse_unary()?), span))
            }
            Tok::Connect => {
                self.bump();
                self.expect(&Tok::LParen, "`(`")?;
                let host = self.parse_expr()?;
                self.expect(&Tok::Comma, "`,`")?;
                let port = self.parse_expr()?;
                self.expect(&Tok::RParen, "`)`")?;
                Ok(Expr::Connect(Box::new(host), Box::new(port), span))
            }
            Tok::Listen => {
                self.bump();
                self.expect(&Tok::LParen, "`(`")?;
                let port = self.parse_expr()?;
                self.expect(&Tok::RParen, "`)`")?;
                Ok(Expr::Listen(Box::new(port), span))
            }
            Tok::Accept => {
                self.bump();
                self.expect(&Tok::LParen, "`(`")?;
                let listener = self.parse_expr()?;
                self.expect(&Tok::RParen, "`)`")?;
                Ok(Expr::Accept(Box::new(listener), span))
            }
            Tok::Open => {
                self.bump();
                self.expect(&Tok::LParen, "`(`")?;
                let path = self.parse_expr()?;
                self.expect(&Tok::Comma, "`,`")?;
                let mode = self.parse_expr()?;
                self.expect(&Tok::RParen, "`)`")?;
                Ok(Expr::Open(Box::new(path), Box::new(mode), span))
            }
            _ => self.parse_call(),
        }
    }

    fn parse_call(&mut self) -> PResult<Expr> {
        let primary = self.parse_postfix()?;
        if self.peek().tok == Tok::LParen {
            let span = primary.span();
            let name = match &primary {
                Expr::Ident(n, _) => n.clone(),
                _ => {
                    return Err(ParseError {
                        message: "only a plain name can be called".to_string(),
                        span,
                    })
                }
            };
            self.bump(); // (
            let mut args = Vec::new();
            if self.peek().tok != Tok::RParen {
                loop {
                    args.push(self.parse_expr()?);
                    if self.peek().tok == Tok::Comma {
                        self.bump();
                        continue;
                    }
                    break;
                }
            }
            self.expect(&Tok::RParen, "`)`")?;
            Ok(Expr::Call(name, args, span))
        } else {
            Ok(primary)
        }
    }

    // postfix ::= primary ("[" expr ("," expr)* "]" | "." ident)*
    // Sits between `call` and `primary` (module doc): `v[i]` and `m[i, j]`
    // are one bracket group each, not chained `[i][j]` subscripting — see
    // `Expr::Index`'s doc comment for why a single `Vec<Expr>` is the
    // right shape for that, rather than nesting `Expr::Index` inside
    // itself per index. `.ident` (Row 11 field access) is the one new
    // alternative here — `a.b.c` chains naturally through the same loop,
    // same as `v[i][j]` already does; neither can follow a *call*'s
    // result (`f().field`/`f()[i]`), since `parse_call` only invokes this
    // once, on `primary`, before ever seeing a trailing `(` — a real,
    // pre-existing limitation this doesn't newly introduce.
    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut e = self.parse_primary()?;
        loop {
            match self.peek().tok {
                Tok::LBracket => {
                    let span = self.span();
                    self.bump();
                    let mut indices = vec![self.parse_expr()?];
                    while self.peek().tok == Tok::Comma {
                        self.bump();
                        indices.push(self.parse_expr()?);
                    }
                    self.expect(&Tok::RBracket, "`]`")?;
                    e = Expr::Index(Box::new(e), indices, span);
                }
                Tok::Dot => {
                    let span = self.span();
                    self.bump();
                    let field = self.expect_ident()?;
                    e = Expr::FieldAccess(Box::new(e), field, span);
                }
                _ => break,
            }
        }
        Ok(e)
    }

    // primary ::= int_lit | float_lit | "true" | "false" | ident | "(" expr ")"
    fn parse_primary(&mut self) -> PResult<Expr> {
        let span = self.span();
        match self.peek().tok.clone() {
            Tok::Int(n) => {
                self.bump();
                Ok(Expr::Int(n, span))
            }
            Tok::Float(f) => {
                self.bump();
                Ok(Expr::Float(f, span))
            }
            Tok::Str(s) => {
                self.bump();
                Ok(Expr::Str(s, span))
            }
            Tok::True => {
                self.bump();
                Ok(Expr::Bool(true, span))
            }
            Tok::False => {
                self.bump();
                Ok(Expr::Bool(false, span))
            }
            Tok::Ident(name) => {
                self.bump();
                Ok(Expr::Ident(name, span))
            }
            Tok::LParen => {
                self.bump();
                let e = self.parse_expr()?;
                self.expect(&Tok::RParen, "`)`")?;
                Ok(e)
            }
            // `[e1, e2, ...]` -- a vector or matrix literal (`Expr::
            // ArrayLit`'s doc comment; `typeck.rs` decides which). Always
            // at least one element -- no `[]` alternative, the same
            // "requires a real expr inside" restriction `"(" expr ")"`
            // above already has for parens.
            Tok::LBracket => {
                self.bump();
                let mut elements = vec![self.parse_expr()?];
                while self.peek().tok == Tok::Comma {
                    self.bump();
                    elements.push(self.parse_expr()?);
                }
                self.expect(&Tok::RBracket, "`]`")?;
                Ok(Expr::ArrayLit(elements, span))
            }
            other => Err(ParseError {
                message: format!("expected an expression, found {other:?}"),
                span,
            }),
        }
    }
}
