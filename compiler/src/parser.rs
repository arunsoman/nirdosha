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
        match &self.peek().tok {
            Tok::TypeName(name) => {
                let ty = Ty::from_name(name).expect("lexer only emits valid type names");
                self.bump();
                Ok(ty)
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
        while self.peek().tok != Tok::Eof {
            fns.push(self.parse_fn_decl()?);
        }
        Ok(Program { fns })
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
        let body = self.parse_block()?;
        Ok(FnDecl { name, params, ret, body, span })
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
            _ => Ok(Stmt::Expr(self.parse_expr()?)),
        }
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

    // expr ::= if_expr | assignment
    fn parse_expr(&mut self) -> PResult<Expr> {
        if self.peek().tok == Tok::If {
            self.parse_if_expr()
        } else {
            self.parse_assignment()
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
            _ => self.parse_call(),
        }
    }

    fn parse_call(&mut self) -> PResult<Expr> {
        let primary = self.parse_primary()?;
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

    // primary ::= int_lit | "true" | "false" | ident | "(" expr ")"
    fn parse_primary(&mut self) -> PResult<Expr> {
        let span = self.span();
        match self.peek().tok.clone() {
            Tok::Int(n) => {
                self.bump();
                Ok(Expr::Int(n, span))
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
            other => Err(ParseError {
                message: format!("expected an expression, found {other:?}"),
                span,
            }),
        }
    }
}
