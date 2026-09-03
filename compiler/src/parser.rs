//! Recursive-descent parser. Every `parse_*` function looks at exactly one
//! token — `self.peek()` — before deciding what to do, and never puts a
//! token back. That single-token-lookahead, no-backtracking discipline is
//! what makes this LL(1) by construction; see GRAMMAR.md for the claim and
//! its scope. Mirrors the EBNF in GRAMMAR.md production-for-production, so
//! the two stay honest about each other — if they drift, one of them is
//! wrong.

use crate::ast::*;
use crate::token::{Span, Tok, Token};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
    /// Nesting depth across `parse_expr`/`parse_unary` combined (the
    /// grammar's two independent recursive-descent entry points --
    /// parenthesized/bracketed sub-expressions recurse back through
    /// `parse_expr`, while chained prefix operators like `!`/`-`/`*`/
    /// `box`/`&` recurse through `parse_unary` directly, never touching
    /// `parse_expr` per level). See `MAX_PARSE_DEPTH`'s doc comment for
    /// why this exists at all.
    depth: usize,
}

/// Past this many combined `parse_expr`/`parse_unary` nesting levels,
/// fail with a normal `ParseError` instead of recursing further.
/// Without this, a red-team finding confirmed a few KB of adversarial
/// source (~1000 nested parens, or a few thousand chained unary `-`)
/// reliably overflows the real Rust stack -- a hard, uncatchable
/// process abort (SIGABRT via the stack guard page), not a panic --
/// which matters most for any deployment that ever compiles/runs
/// untrusted `.nir` source directly (a "paste your code" service; a
/// live `nirdosha serve` instance itself only parses its own fixed file
/// once at startup, not per request, so this specific path isn't
/// reachable through its HTTP API -- see `interpreter::MAX_CALL_DEPTH`
/// for the analogous *interpreter*-level guard that protects the part
/// of an already-running app request handlers actually re-enter).
/// 50 is conservative on purpose: legitimate hand-written expressions
/// essentially never nest anywhere close to this deep. It has to be set
/// low enough for a debug build's own default-stack-size crash
/// threshold, which is far lower than release's: measured empirically,
/// an unguarded debug build overflowed for real somewhere between depth
/// 80 and 100 (parenthesized-expression nesting) on the default thread
/// stack -- there's no bigger-stack wrapper for parsing the way
/// `interpreter::run_on_big_stack` gives call depth much more headroom,
/// so this constant has to stay safe on the *default* stack outright.
const MAX_PARSE_DEPTH: usize = 50;

type PResult<T> = Result<T, ParseError>;

impl Parser {
    pub fn new(toks: Vec<Token>) -> Self {
        Parser { toks, pos: 0, depth: 0 }
    }

    /// Shared entry/exit pair for `parse_expr`/`parse_unary` — see
    /// `depth`'s and `MAX_PARSE_DEPTH`'s doc comments.
    fn enter_nesting(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            let span = self.span();
            self.depth -= 1;
            return Err(ParseError { message: "expression nested too deeply".to_string(), span });
        }
        Ok(())
    }

    fn exit_nesting(&mut self) {
        self.depth -= 1;
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
        // `fn(T1, T2) -> R` — a first-class function value's type. Reuses
        // `Tok::Fn` (the same keyword a declaration starts with) rather
        // than a second dedicated token, since the two are never
        // syntactically ambiguous: a `fn` at declaration position is
        // always followed by a name, never `(`.
        if self.peek().tok == Tok::Fn {
            self.bump();
            self.expect(&Tok::LParen, "`(`")?;
            let mut params = Vec::new();
            if self.peek().tok != Tok::RParen {
                loop {
                    params.push(self.expect_type()?);
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
            return Ok(Ty::Fn(params, Box::new(ret)));
        }
        match &self.peek().tok {
            Tok::TypeName(name) => {
                let ty = Ty::from_name(name).expect("lexer only emits valid type names");
                self.bump();
                Ok(ty)
            }
            // A declared `struct`/`enum` name (Row 11), optionally applied
            // to concrete type arguments (layer 6, generics —
            // `nirdosha_row11_amendment.md` §3.3's "the same production
            // shape `Vector(T, N)`/`Matrix(T, R, C)` already use"). Bare
            // `ident` is accepted syntactically for *any* identifier here,
            // same as a function call name; whether it actually names a
            // real declared type, and whether the argument count matches
            // that declaration's own type-parameter count, is `typeck.rs`'s
            // job (`TypeErrorKind::UnknownType`/`WrongTypeArity`), not this
            // parser's — it has no declaration table to check against, and
            // Nirdosha's functions/types are already resolved by name in a
            // table built after parsing completes (`LANGUAGE.md` §6), not
            // as each name is scanned.
            Tok::Ident(name) => {
                let n = name.clone();
                self.bump();
                let mut args = Vec::new();
                if self.peek().tok == Tok::LParen {
                    self.bump();
                    if self.peek().tok != Tok::RParen {
                        loop {
                            args.push(self.expect_type()?);
                            if self.peek().tok == Tok::Comma {
                                self.bump();
                                continue;
                            }
                            break;
                        }
                    }
                    self.expect(&Tok::RParen, "`)`")?;
                }
                Ok(Ty::Named(n, args))
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
        // Row 11 layer 7's prelude (`ast::prelude_enums`/`prelude_structs`'
        // doc comments): `Option(T)`/`Result(T, E)`, and `HttpResponse`
        // (`http_get`/`http_post`'s result shape), prepended as if the
        // user had written them — no special-casing anywhere downstream.
        let mut structs = prelude_structs();
        let mut enums = prelude_enums();
        let mut screens = Vec::new();
        let mut dashboard = None;
        let mut workflows = Vec::new();
        let mut workspaces = Vec::new();
        while self.peek().tok != Tok::Eof {
            match self.peek().tok {
                Tok::Struct => structs.push(self.parse_struct_decl()?),
                Tok::Enum => enums.push(self.parse_enum_decl()?),
                Tok::Screen => screens.push(self.parse_screen_decl()?),
                Tok::Workflow => workflows.push(self.parse_workflow_decl()?),
                Tok::Workspace => workspaces.push(self.parse_workspace_decl()?),
                Tok::Dashboard => {
                    let d = self.parse_dashboard_decl()?;
                    if dashboard.is_some() {
                        return Err(ParseError {
                            message: "only one `dashboard { ... }` block is allowed per program".to_string(),
                            span: d.span,
                        });
                    }
                    dashboard = Some(d);
                }
                Tok::Module => {
                    let (mut mfns, mut mstructs, mut menums) = self.parse_module_decl()?;
                    fns.append(&mut mfns);
                    structs.append(&mut mstructs);
                    enums.append(&mut menums);
                }
                _ => fns.push(self.parse_fn_decl()?),
            }
        }
        let mut program = Program { fns, structs, enums, screens, dashboard, workflows, workspaces };
        crate::workflow_lower::lower(&mut program)?;
        Ok(program)
    }

    /// `module_decl ::= "module" STRING "{" (fn_decl | struct_decl |
    /// enum_decl)* "}"` (`GRAMMAR.md`) — pure nav-grouping sugar, not a
    /// real scoping construct. Every declaration inside is parsed by the
    /// exact same `parse_fn_decl`/`parse_struct_decl`/`parse_enum_decl`
    /// the top-level loop itself calls (each self-consumes its own
    /// leading keyword, so they work standalone here unchanged), then
    /// tagged with this module's display name and returned for the
    /// caller to fold into its own flat `fns`/`structs`/`enums` — there
    /// is no separate `Program`-shaped nested item list anywhere, no new
    /// namespace, nothing else in the checker even knows this block
    /// existed. Single-level only: nesting a `module` inside a `module`,
    /// or a `screen`/`dashboard` inside one, is a parse error, matching
    /// this grammar's existing fixed-arity/no-arbitrary-nesting
    /// discipline elsewhere (e.g. `transact` slots never nest either).
    fn parse_module_decl(&mut self) -> PResult<(Vec<FnDecl>, Vec<StructDecl>, Vec<EnumDecl>)> {
        self.expect(&Tok::Module, "`module`")?;
        let name = self.expect_str_lit("a module display name")?;
        self.expect(&Tok::LBrace, "`{`")?;
        let mut fns = Vec::new();
        let mut structs = Vec::new();
        let mut enums = Vec::new();
        while self.peek().tok != Tok::RBrace {
            match self.peek().tok {
                Tok::Struct => {
                    let mut s = self.parse_struct_decl()?;
                    s.module = Some(name.clone());
                    structs.push(s);
                }
                Tok::Enum => {
                    let mut e = self.parse_enum_decl()?;
                    e.module = Some(name.clone());
                    enums.push(e);
                }
                Tok::Module => {
                    return Err(ParseError {
                        message: "`module` blocks cannot be nested".to_string(),
                        span: self.span(),
                    });
                }
                Tok::Screen | Tok::Dashboard => {
                    return Err(ParseError {
                        message: "`screen`/`dashboard` blocks must be declared at top level, outside any `module`".to_string(),
                        span: self.span(),
                    });
                }
                Tok::Eof => {
                    return Err(ParseError {
                        message: "unterminated `module` block, expected `}`".to_string(),
                        span: self.span(),
                    });
                }
                _ => {
                    let mut f = self.parse_fn_decl()?;
                    f.module = Some(name.clone());
                    fns.push(f);
                }
            }
        }
        self.expect(&Tok::RBrace, "`}`")?;
        Ok((fns, structs, enums))
    }

    /// One `key: value` slot shared by `screen`/`field`/`action`/
    /// `paginate` bodies. The value is parsed as an ordinary expression —
    /// `parse_expr()` handles string/int/bool literals, a bare `Ident`
    /// (`list: list_product`), and a `Call` (`view: role("admin")`)
    /// alike, with no dedicated value grammar. Typeck later narrows each
    /// key to its expected shape.
    fn parse_kv_entry(&mut self) -> PResult<KvEntry> {
        let key = self.expect_ident()?;
        self.expect(&Tok::Colon, "`:`")?;
        let value = self.parse_expr()?;
        Ok((key, value))
    }

    /// `screen_decl ::= "screen" IDENT "{" screen_item* "}"`
    /// `screen_item ::= paginate_block | field_override | action_decl | kv_entry`
    ///
    /// Dispatch inside the body is on the first token's identifier TEXT
    /// alone — `"paginate"`/`"field"`/`"action"` vs. anything else is a
    /// plain `kv_entry` — so this stays LL(1) with no second-token
    /// lookahead. `field`/`action`/`paginate` are therefore reserved only
    /// as a screen body's leading key, exactly like `role`/`claim` inside
    /// `requires(...)`; they remain ordinary identifiers everywhere else
    /// (`trade_finance.nir` already uses `action` as a struct field name).
    ///
    /// `paginate { ... }` gets no dedicated `ScreenDecl` field — its
    /// entries are folded directly into the screen's own `entries` list,
    /// each key prefixed `"paginate."` (`paginate { page_size: 25 }`
    /// becomes the entry `("paginate.page_size", 25)`). Flat and
    /// prefix-filterable by typeck/`ui_gen.rs`, no new AST node needed.
    fn parse_screen_decl(&mut self) -> PResult<ScreenDecl> {
        let span = self.span();
        self.expect(&Tok::Screen, "`screen`")?;
        let struct_name = self.expect_ident()?;
        self.expect(&Tok::LBrace, "`{`")?;
        let mut entries = Vec::new();
        let mut fields = Vec::new();
        let mut actions = Vec::new();
        while self.peek().tok != Tok::RBrace {
            let is_kw = matches!(&self.peek().tok, Tok::Ident(s) if s == "paginate" || s == "field" || s == "action");
            if is_kw {
                let kw = self.expect_ident()?;
                match kw.as_str() {
                    "paginate" => {
                        self.expect(&Tok::LBrace, "`{`")?;
                        while self.peek().tok != Tok::RBrace {
                            let (k, v) = self.parse_kv_entry()?;
                            entries.push((format!("paginate.{k}"), v));
                        }
                        self.expect(&Tok::RBrace, "`}`")?;
                    }
                    "field" => fields.push(self.parse_field_override()?),
                    "action" => actions.push(self.parse_action_decl()?),
                    _ => unreachable!(),
                }
            } else {
                entries.push(self.parse_kv_entry()?);
            }
        }
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(ScreenDecl { struct_name, entries, fields, actions, span })
    }

    /// `field_override ::= "field" IDENT "{" kv_entry* "}"`
    fn parse_field_override(&mut self) -> PResult<FieldOverride> {
        let span = self.span();
        let field_name = self.expect_ident()?;
        self.expect(&Tok::LBrace, "`{`")?;
        let mut entries = Vec::new();
        while self.peek().tok != Tok::RBrace {
            entries.push(self.parse_kv_entry()?);
        }
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(FieldOverride { field_name, entries, span })
    }

    /// `action_decl ::= "action" STRING "->" IDENT "{" kv_entry* "}"`
    /// The trailing `{ ... }` is optional — `action "Delete" -> delete_product`
    /// alone is a valid, style-less/confirm-less action.
    fn parse_action_decl(&mut self) -> PResult<ActionDecl> {
        let span = self.span();
        let label = self.expect_str_lit("an action label")?;
        self.expect(&Tok::Arrow, "`->`")?;
        let target_fn = self.expect_ident()?;
        let mut entries = Vec::new();
        if self.peek().tok == Tok::LBrace {
            self.bump();
            while self.peek().tok != Tok::RBrace {
                entries.push(self.parse_kv_entry()?);
            }
            self.expect(&Tok::RBrace, "`}`")?;
        }
        Ok(ActionDecl { label, target_fn, entries, span })
    }

    /// `dashboard_decl ::= "dashboard" "{" dashboard_item* "}"`
    /// `dashboard_item ::= ("tile" | "chart") STRING "->" IDENT`
    /// Same first-token-text dispatch as `screen`'s body; `tile`/`chart`
    /// are reserved only as a dashboard body's leading item keyword.
    fn parse_dashboard_decl(&mut self) -> PResult<DashboardDecl> {
        let span = self.span();
        self.expect(&Tok::Dashboard, "`dashboard`")?;
        self.expect(&Tok::LBrace, "`{`")?;
        let mut tiles = Vec::new();
        let mut charts = Vec::new();
        while self.peek().tok != Tok::RBrace {
            let kw = self.expect_ident()?;
            let item_span = self.span();
            match kw.as_str() {
                "tile" | "chart" => {
                    let label = self.expect_str_lit("a tile/chart label")?;
                    self.expect(&Tok::Arrow, "`->`")?;
                    let target_fn = self.expect_ident()?;
                    let m = MetricRef { label, target_fn, span: item_span };
                    if kw == "tile" {
                        tiles.push(m);
                    } else {
                        charts.push(m);
                    }
                }
                other => {
                    return Err(ParseError {
                        message: format!("expected `tile` or `chart`, found identifier `{other}`"),
                        span: item_span,
                    });
                }
            }
        }
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(DashboardDecl { tiles, charts, span })
    }

    /// `workspace_decl ::= "workspace" IDENT "{" workspace_item* "}"`
    /// `workspace_item ::= panel_decl | kv_entry`
    /// (`GRAMMAR.md`, `ROADMAP.md` Track E1) — same first-token-text
    /// dispatch as `screen_decl`'s own body: `"panel"` vs. anything else
    /// is a plain `kv_entry` (`subject: Case`, `title: "..."`), so this
    /// stays LL(1) with no second-token lookahead, mirroring
    /// `parse_screen_decl` production-for-production.
    fn parse_workspace_decl(&mut self) -> PResult<WorkspaceDecl> {
        let span = self.span();
        self.expect(&Tok::Workspace, "`workspace`")?;
        let name = self.expect_ident()?;
        self.expect(&Tok::LBrace, "`{`")?;
        let mut entries = Vec::new();
        let mut panels = Vec::new();
        while self.peek().tok != Tok::RBrace {
            let is_panel = matches!(&self.peek().tok, Tok::Ident(s) if s == "panel");
            if is_panel {
                self.expect_ident()?; // consume "panel"
                panels.push(self.parse_panel_decl()?);
            } else {
                entries.push(self.parse_kv_entry()?);
            }
        }
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(WorkspaceDecl { name, entries, panels, span })
    }

    /// `panel_decl ::= "panel" STRING "{" panel_item* "}"`
    /// `panel_item ::= action_decl | kv_entry`
    /// Called with the leading `"panel"` token already consumed by
    /// `parse_workspace_decl` (the same "caller consumes the dispatch
    /// keyword" shape `parse_dashboard_decl`'s `tile`/`chart` arm uses).
    /// `action_decl` inside a panel is `parse_action_decl` reused
    /// completely unchanged — zero new syntax for panel actions beyond
    /// what a screen's own actions already have.
    fn parse_panel_decl(&mut self) -> PResult<PanelDecl> {
        let span = self.span();
        let title = self.expect_str_lit("a panel title")?;
        self.expect(&Tok::LBrace, "`{`")?;
        let mut entries = Vec::new();
        let mut actions = Vec::new();
        while self.peek().tok != Tok::RBrace {
            let is_action = matches!(&self.peek().tok, Tok::Ident(s) if s == "action");
            if is_action {
                self.expect_ident()?; // consume "action"
                actions.push(self.parse_action_decl()?);
            } else {
                entries.push(self.parse_kv_entry()?);
            }
        }
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(PanelDecl { title, entries, actions, span })
    }

    /// `workflow_decl ::= "workflow" IDENT "{" data_block? state_decl+ "}"`
    /// (`GRAMMAR.md`, `WORKFLOW.md`) — parsed into source-form
    /// `ast::WorkflowDecl`, later desugared by `workflow_lower.rs` into
    /// ordinary `FnDecl`s/`EnumDecl`s. `data`/`state` bodies dispatch on
    /// leading-token identifier text exactly like `screen`'s own body
    /// does (see `parse_screen_decl`'s doc comment) — no second-token
    /// lookahead anywhere.
    fn parse_workflow_decl(&mut self) -> PResult<WorkflowDecl> {
        let span = self.span();
        self.expect(&Tok::Workflow, "`workflow`")?;
        let name = self.expect_ident()?;
        self.expect(&Tok::LBrace, "`{`")?;
        let mut data = Vec::new();
        if matches!(&self.peek().tok, Tok::Ident(s) if s == "data") {
            data = self.parse_workflow_data_block()?;
        }
        let mut states = Vec::new();
        while self.peek().tok != Tok::RBrace {
            match self.peek().tok {
                Tok::State => states.push(self.parse_state_decl()?),
                Tok::Eof => {
                    return Err(ParseError {
                        message: "unterminated `workflow` block, expected `}`".to_string(),
                        span: self.span(),
                    });
                }
                ref other => {
                    return Err(ParseError {
                        message: format!("expected `state` inside `workflow {name}`, found {other:?}"),
                        span: self.span(),
                    });
                }
            }
        }
        self.expect(&Tok::RBrace, "`}`")?;
        if states.is_empty() {
            return Err(ParseError { message: format!("`workflow {name}` declares no `state`s"), span });
        }
        Ok(WorkflowDecl { name, data, states, span })
    }

    // data_block ::= "data" "{" field ("," field)* ","? "}"
    fn parse_workflow_data_block(&mut self) -> PResult<Vec<Field>> {
        self.bump(); // "data" (identifier-matched, not a Tok keyword)
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
                        break;
                    }
                    continue;
                }
                break;
            }
        }
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(fields)
    }

    /// `state_decl ::= "state" IDENT "terminal"? "{" state_item* "}"`
    /// `state_item ::= on_entry_block | on_exit_block | transition | kv_entry`
    /// — `terminal` is matched by identifier text right after the state
    /// name (same "keyword only in this one slot" treatment as everywhere
    /// else in this grammar), not a reserved `Tok`. Any leading identifier
    /// other than `on_entry`/`on_exit`/`on` is a plain `kv_entry`
    /// (`owner: role(...)`, `label: "..."` — `WORKFLOW.md`'s "state
    /// ownership" section) — same "first-token-text dispatch, no second-
    /// token lookahead" shape `parse_screen_decl`'s body already uses, so
    /// `owner`/`label` stay ordinary identifiers everywhere else.
    fn parse_state_decl(&mut self) -> PResult<StateDecl> {
        let span = self.span();
        self.expect(&Tok::State, "`state`")?;
        let name = self.expect_ident()?;
        let terminal = if matches!(&self.peek().tok, Tok::Ident(s) if s == "terminal") {
            self.bump();
            true
        } else {
            false
        };
        self.expect(&Tok::LBrace, "`{`")?;
        let mut on_entry = Vec::new();
        let mut on_exit = Vec::new();
        let mut transitions = Vec::new();
        let mut entries = Vec::new();
        while self.peek().tok != Tok::RBrace {
            match &self.peek().tok {
                Tok::Ident(s) if s == "on_entry" => {
                    self.bump();
                    on_entry = self.parse_action_block()?;
                }
                Tok::Ident(s) if s == "on_exit" => {
                    self.bump();
                    on_exit = self.parse_action_block()?;
                }
                Tok::Ident(s) if s == "on" => transitions.push(self.parse_transition()?),
                Tok::Ident(_) => entries.push(self.parse_kv_entry()?),
                Tok::Eof => {
                    return Err(ParseError {
                        message: format!("unterminated `state {name}` block, expected `}}`"),
                        span: self.span(),
                    });
                }
                other => {
                    return Err(ParseError {
                        message: format!(
                            "expected `on_entry`, `on_exit`, `on`, or a `key: value` entry inside `state {name}`, found {other:?}"
                        ),
                        span: self.span(),
                    });
                }
            }
        }
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(StateDecl { name, terminal, on_entry, on_exit, transitions, entries, span })
    }

    // action_block ::= "{" action_call* "}"
    fn parse_action_block(&mut self) -> PResult<Vec<TransactSlot>> {
        self.expect(&Tok::LBrace, "`{`")?;
        let mut actions = Vec::new();
        while self.peek().tok != Tok::RBrace {
            actions.push(self.parse_action_call()?);
        }
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(actions)
    }

    // One bare `name(args)` inside an `on_entry`/`on_exit` block. Same
    // "must parse as a plain call" restriction `parse_transact_slot`
    // enforces, reusing `TransactSlot` as the node — see `WorkflowDecl`'s
    // doc comment for why on_entry/on_exit reuse that type.
    fn parse_action_call(&mut self) -> PResult<TransactSlot> {
        let span = self.span();
        let call = self.parse_call()?;
        match call {
            Expr::Call(name, args, call_span) => Ok(TransactSlot { name, args, span: call_span }),
            _ => Err(ParseError {
                message: "an `on_entry`/`on_exit` action must be a plain function call, e.g. `send_email(...)`"
                    .to_string(),
                span,
            }),
        }
    }

    // transition ::= "on" "link"? IDENT "->" IDENT
    fn parse_transition(&mut self) -> PResult<Transition> {
        let span = self.span();
        self.bump(); // "on"
        let via_link = if matches!(&self.peek().tok, Tok::Ident(s) if s == "link") {
            self.bump();
            true
        } else {
            false
        };
        let event = self.expect_ident()?;
        self.expect(&Tok::Arrow, "`->`")?;
        let target = self.expect_ident()?;
        Ok(Transition { event, target, via_link, span })
    }

    // A `struct`/`enum` declaration's optional type-parameter list --
    // just bare names (`(A, B)`), never types (contrast a *use* of a
    // generic type, `Pair(i64, str)`, which is `expect_type`'s own
    // argument-list parsing). Empty `Vec` when absent, same "omitted
    // entirely" convention every other optional Row 11 production uses.
    fn parse_type_param_list(&mut self) -> PResult<Vec<String>> {
        let mut params = Vec::new();
        if self.peek().tok == Tok::LParen {
            self.bump();
            if self.peek().tok != Tok::RParen {
                loop {
                    params.push(self.expect_ident()?);
                    if self.peek().tok == Tok::Comma {
                        self.bump();
                        continue;
                    }
                    break;
                }
            }
            self.expect(&Tok::RParen, "`)`")?;
        }
        Ok(params)
    }

    // struct_decl ::= "struct" ident ("(" ident ("," ident)* ")")?
    //                 "{" field ("," field)* ","? "}"
    // field       ::= ident ":" type
    fn parse_struct_decl(&mut self) -> PResult<StructDecl> {
        let span = self.span();
        self.expect(&Tok::Struct, "`struct`")?;
        let name = self.expect_ident()?;
        let type_params = self.parse_type_param_list()?;
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
        Ok(StructDecl { name, type_params, fields, span, module: None })
    }

    // enum_decl ::= "enum" ident ("(" ident ("," ident)* ")")?
    //               "{" variant ("," variant)* ","? "}"
    // variant   ::= ident ("(" type ("," type)* ")")?
    fn parse_enum_decl(&mut self) -> PResult<EnumDecl> {
        let span = self.span();
        self.expect(&Tok::Enum, "`enum`")?;
        let name = self.expect_ident()?;
        let type_params = self.parse_type_param_list()?;
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
        Ok(EnumDecl { name, type_params, variants, span, module: None })
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
        let (requires, explicit_public) = self.parse_requires_annotation()?;
        let body = self.parse_block()?;
        Ok(FnDecl { name, params, ret, body, span, declared_effects, requires, explicit_public, module: None })
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

    /// `requires(role: "admin")` / `requires(claim: "department",
    /// "cardiology")` / `requires(public)` — `(None, false)` if there's no
    /// `requires(...)` at all (the common, ungated case). `role`/`claim`/
    /// `public` are matched by identifier text, same "keyword only within
    /// this one slot" treatment `parse_effect_annotation`'s own names get.
    /// `public` is deliberately not a `Requirement` variant — see
    /// `ast::FnDecl::explicit_public`'s doc comment for why — so it's
    /// returned out-of-band as the second tuple element instead of
    /// through the `Option<Requirement>` slot `role`/`claim` use.
    fn parse_requires_annotation(&mut self) -> PResult<(Option<Requirement>, bool)> {
        if self.peek().tok != Tok::Requires {
            return Ok((None, false));
        }
        self.bump();
        self.expect(&Tok::LParen, "`(`")?;
        let kind_span = self.span();
        let kind = self.expect_ident()?;
        let result = match kind.as_str() {
            "role" => {
                self.expect(&Tok::Colon, "`:`")?;
                (Some(Requirement::Role(self.expect_str_lit("a role name")?)), false)
            }
            "claim" => {
                self.expect(&Tok::Colon, "`:`")?;
                let name = self.expect_str_lit("a claim name")?;
                self.expect(&Tok::Comma, "`,`")?;
                let value = self.expect_str_lit("the claim's required value")?;
                (Some(Requirement::Claim(name, value)), false)
            }
            "public" => (None, true),
            other => {
                return Err(ParseError {
                    message: format!("unknown requirement `{other}` — expected `role`, `claim`, or `public`"),
                    span: kind_span,
                });
            }
        };
        self.expect(&Tok::RParen, "`)`")?;
        Ok(result)
    }

    /// A bare string literal, for the handful of grammar slots (`requires`'
    /// role/claim names, `audited`'s justification) that need one directly
    /// rather than as part of a general expression.
    fn expect_str_lit(&mut self, what: &str) -> PResult<String> {
        match self.peek().tok.clone() {
            Tok::Str(s) => {
                self.bump();
                Ok(s)
            }
            other => Err(ParseError {
                message: format!("expected {what} as a string literal, found {other:?}"),
                span: self.span(),
            }),
        }
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
    pub(crate) fn parse_expr(&mut self) -> PResult<Expr> {
        self.enter_nesting()?;
        let result = if self.peek().tok == Tok::If {
            self.parse_if_expr()
        } else if self.peek().tok == Tok::Transact {
            self.parse_transact_expr()
        } else if self.peek().tok == Tok::Match {
            self.parse_match_expr()
        } else {
            self.parse_assignment()
        };
        self.exit_nesting();
        result
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
            // Dispatch on the arm's first token alone (LL(1), same
            // discipline every other contextual-word construct in this
            // grammar already follows): a literal token or the specific
            // identifier `_` is a new literal/wildcard pattern arm
            // (`ast::LiteralPattern`); any other identifier is the
            // original enum-variant arm, unchanged.
            let (variant, pattern) = match &self.peek().tok {
                Tok::Str(s) => {
                    let s = s.clone();
                    self.bump();
                    (format!("{s:?}"), Some(LiteralPattern::Str(s)))
                }
                Tok::Int(n) => {
                    let n = *n;
                    self.bump();
                    (n.to_string(), Some(LiteralPattern::Int(n)))
                }
                Tok::True => {
                    self.bump();
                    ("true".to_string(), Some(LiteralPattern::Bool(true)))
                }
                Tok::False => {
                    self.bump();
                    ("false".to_string(), Some(LiteralPattern::Bool(false)))
                }
                Tok::Ident(name) if name == "_" => {
                    self.bump();
                    ("_".to_string(), Some(LiteralPattern::Wildcard))
                }
                _ => (self.expect_ident()?, None),
            };
            let mut bindings = Vec::new();
            // Only the enum-variant form ever binds a payload — a
            // literal/wildcard pattern arm has none, so `(...)` isn't
            // even attempted for it (a stray `(` there falls through to
            // the `=>` expectation below with a plain parse error).
            if pattern.is_none() && self.peek().tok == Tok::LParen {
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
            arms.push(MatchArm { variant, bindings, pattern, body, span: arm_span });
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
    //                      ("precheck"  ":" call)?
    //                      "network"    ":" call ("retry" int_lit)? ("timeout" int_lit)?
    //                      "verify"     ":" call
    //                      "commit"     ":" call
    //                      ("compensate" ":" call)?
    //                      ("log"        ":" call)?
    //                    "}"
    // Slot order is fixed, not permutation-parsed: "no permutation
    // parsing, keeps this LL(1) the same way every other fixed-arity
    // form in this grammar already is" (TRANSACT.md).
    fn parse_transact_expr(&mut self) -> PResult<Expr> {
        let span = self.span();
        self.expect(&Tok::Transact, "`transact`")?;
        self.expect(&Tok::LBrace, "`{`")?;
        let precheck = self.parse_optional_transact_slot("precheck")?;
        let network = self.parse_transact_slot("network")?;
        let network_retry = self.parse_optional_int_modifier("retry")?;
        let network_timeout = self.parse_optional_int_modifier("timeout")?;
        let verify = self.parse_transact_slot("verify")?;
        let commit = self.parse_transact_slot("commit")?;
        let compensate = self.parse_optional_transact_slot("compensate")?;
        let log = self.parse_optional_transact_slot("log")?;
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(Expr::Transact { precheck, network, network_retry, network_timeout, verify, commit, compensate, log, span })
    }

    // `("retry"|"timeout") int_lit` — same non-keyword, matched-by-text
    // treatment `parse_transact_slot`'s own `label` already gets (see its
    // doc comment); plain `int_lit`, no unit suffix, TRANSACT.md's own
    // decision against inventing a duration literal for `timeout`.
    fn parse_optional_int_modifier(&mut self, label: &str) -> PResult<Option<i64>> {
        match &self.peek().tok {
            Tok::Ident(s) if s == label => {
                self.bump();
                let span = self.span();
                match self.peek().tok {
                    Tok::Int(n) => {
                        self.bump();
                        Ok(Some(n))
                    }
                    ref other => {
                        Err(ParseError { message: format!("expected an integer literal after `{label}`, found {other:?}"), span })
                    }
                }
            }
            _ => Ok(None),
        }
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
        self.enter_nesting()?;
        let result = self.parse_unary_inner();
        self.exit_nesting();
        result
    }

    fn parse_unary_inner(&mut self) -> PResult<Expr> {
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
            Tok::Acquire => {
                self.bump();
                // Same "parse a normal call, then destructure" trick as
                // `spawn` above, restricted to exactly one argument (the
                // proof) — enforced here rather than left to `typeck.rs`
                // since it's a syntactic shape, not a type question.
                let operand = self.parse_call()?;
                match operand {
                    Expr::Call(name, mut args, _) if args.len() == 1 => {
                        Ok(Expr::Acquire(name, Box::new(args.remove(0)), span))
                    }
                    Expr::Call(..) => Err(ParseError {
                        message: "`acquire` takes exactly one argument, the proof — e.g. `acquire transfer_funds(proof)`".to_string(),
                        span,
                    }),
                    _ => Err(ParseError {
                        message: "`acquire` requires a function call, e.g. `acquire transfer_funds(proof)`".to_string(),
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

/// Parses `src` as one bare expression, not a whole program — the entry
/// point `contract_check.rs` uses to turn a Hoare predicate string (a
/// user story's `pre_logic`/`post_logic`, a `routing_fn`'s own, straight
/// out of an extraction JSON like `scratch/extracted_typed_v1.json`)
/// into a real `Expr`, using the exact same grammar/precedence a `.nir`
/// file's own expressions get — no separate predicate mini-language to
/// keep in sync. Requires the whole string to be consumed by one
/// expression (a stray trailing token is a parse error, not silently
/// ignored), the same "no leftover input" discipline `parse_program`
/// gets from consuming every token up to `Tok::Eof`.
pub fn parse_standalone_expr(src: &str) -> Result<Expr, String> {
    let toks = crate::token::Lexer::new(src)
        .tokenize()
        .map_err(|e| format!("lex error at {}:{}: {}", e.span.line, e.span.col, e.message))?;
    let mut parser = Parser::new(toks);
    let expr = parser.parse_expr().map_err(|e| format!("parse error at {}:{}: {}", e.span.line, e.span.col, e.message))?;
    if parser.peek().tok != Tok::Eof {
        let span = parser.span();
        return Err(format!("{}:{}: unexpected trailing input after the expression", span.line, span.col));
    }
    Ok(expr)
}
