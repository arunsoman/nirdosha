//! Desugars `workflow Name { ... }` (`ast::WorkflowDecl`) into ordinary
//! `FnDecl`s/`EnumDecl`s/`StructDecl`s, called once from
//! `Parser::parse_program` right after parsing — see `WorkflowDecl`'s own
//! doc comment for why (same "pure lowering, zero new dispatch machinery"
//! shape `module` already uses, `docs/LANGUAGE.md` §12). Every synthesized
//! function's body is a one-line call into one of three shared interpreter
//! builtins (`__workflow_start`/`__workflow_advance`/
//! `__workflow_link_advance`, `ast::BUILTIN_NAMES`) with this workflow's
//! own name baked in as a literal `str` — the real state-machine logic
//! (running `on_entry`/`on_exit`, moving `state`, minting/consuming
//! magic-link tokens) lives in `interpreter.rs`'s dispatch for those three
//! names, driven by the *original* `WorkflowDecl` (kept on
//! `Program.workflows`, not drained here — see that field's doc comment).
//!
//! Structural sanity that must hold to safely generate well-formed
//! `FnDecl`s (no duplicate/unknown state names) is checked here, as a
//! `ParseError`. Deeper semantic rules that don't affect what gets
//! generated (every non-terminal state has a transition, `data.<field>`/
//! `link_<Event>` references resolve) are `typeck.rs::check_workflow_decl`'s
//! job instead — it runs after lowering and reads the same `WorkflowDecl`.
//!
//! `docs/WORKFLOW.md`'s "state ownership + a generated queue UI" section adds a
//! fourth synthesized fn/builtin pair, `list_<workflow>_pending_for_me`/
//! `__workflow_pending_for_me`, and a real, disclosed breaking-change
//! `identity: VerifiedIdentity` leading param on `advance_<workflow>` — see
//! that fn's own doc comment below for why the owner check has to live at
//! runtime (`interpreter.rs::workflow_advance`) rather than as a static
//! `requires(...)` here.

use crate::ast::*;
use crate::parser::ParseError;

fn to_snake_case(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn workflow_action_error_ty() -> Ty {
    Ty::Named("WorkflowActionError".to_string(), vec![])
}

fn result_ty(ok: Ty) -> Ty {
    Ty::Named("Result".to_string(), vec![ok, workflow_action_error_ty()])
}

/// A no-op for a program with zero `workflow` blocks — no extra work, no
/// extra declarations, matching every other additive construct in this
/// grammar's own "unused feature costs nothing" convention.
pub fn lower(program: &mut Program) -> Result<(), ParseError> {
    if program.workflows.is_empty() {
        return Ok(());
    }
    let workflows = program.workflows.clone();
    for w in &workflows {
        lower_one(w, program)?;
    }
    Ok(())
}

fn lower_one(w: &WorkflowDecl, program: &mut Program) -> Result<(), ParseError> {
    let span = w.span;

    let mut seen_states = std::collections::HashSet::new();
    for s in &w.states {
        if !seen_states.insert(s.name.as_str()) {
            return Err(ParseError {
                message: format!("`workflow {}` declares `state {}` more than once", w.name, s.name),
                span: s.span,
            });
        }
    }
    for s in &w.states {
        for t in &s.transitions {
            if !seen_states.contains(t.target.as_str()) {
                return Err(ParseError {
                    message: format!(
                        "`workflow {}`'s state `{}` transitions to unknown state `{}`",
                        w.name, s.name, t.target
                    ),
                    span: t.span,
                });
            }
        }
    }

    // 1. `<Workflow>Event` — one zero-payload variant per distinct event
    // name, first-appearance order (reads naturally top-to-bottom against
    // the source `workflow` block, not alphabetized).
    let mut event_names: Vec<String> = Vec::new();
    for s in &w.states {
        for t in &s.transitions {
            if !event_names.contains(&t.event) {
                event_names.push(t.event.clone());
            }
        }
    }
    let event_enum_name = format!("{}Event", w.name);
    program.enums.push(EnumDecl {
        name: event_enum_name.clone(),
        type_params: vec![],
        variants: event_names.iter().map(|e| Variant { name: e.clone(), payload: vec![], span }).collect(),
        span,
        module: None,
        ns: None,
        exported: true,
    });

    // 2. `<Workflow>LinkToken { value: str }` — only if this workflow has
    // at least one `link`-marked transition. A fresh, workflow-scoped
    // carrier struct rather than depending on the user having declared
    // their own `Text` (docs/LANGUAGE.md §6b's convention name) — guarantees no
    // collision with whatever the program itself declares.
    let mut link_events: Vec<String> = Vec::new();
    for s in &w.states {
        for t in &s.transitions {
            if t.via_link && !link_events.contains(&t.event) {
                link_events.push(t.event.clone());
            }
        }
    }
    let link_token_name = format!("{}LinkToken", w.name);
    if !link_events.is_empty() {
        program.structs.push(StructDecl {
            name: link_token_name.clone(),
            type_params: vec![],
            fields: vec![Field { name: "value".to_string(), ty: Ty::Str }],
            span,
            module: None,
            ns: None,
            exported: true,
        });
    }

    // 2b. `<Workflow>Data { ...w.data fields... }` — the typed carrier for
    // `start_*`'s parameter and for `data.<field>` implicit bindings
    // inside every state's `on_entry`/`on_exit` (`interpreter.rs` decodes
    // it back from the stored `data_json` via `serve.rs::decode_value`,
    // the same generic struct<->JSON codec the RPC layer already uses).
    // Synthesized even with zero fields, so `start_*`'s signature doesn't
    // need a conditional shape.
    let data_struct_name = format!("{}Data", w.name);
    program.structs.push(StructDecl {
        name: data_struct_name.clone(),
        type_params: vec![],
        fields: w.data.clone(),
        span,
        module: None,
        ns: None,
        exported: true,
    });

    // 3. `start_<workflow_snake>(identity: Option(VerifiedIdentity), data:
    // <Workflow>Data) -> Result(i64, WorkflowActionError)` — `identity` is
    // a real, disclosed breaking-change param (`docs/WORKFLOW.md`'s "who
    // submitted this" section), *optional* rather than required like
    // `advance_<workflow>`'s own `identity: VerifiedIdentity`: unlike
    // firing a state's transition, starting a workflow is legitimately
    // anonymous in real programs today (`kyc_onboarding.nir`'s own public
    // intake — a brand-new applicant has no account yet). `serve.rs::
    // dispatch` injects `Some(id)`/`None` for an `Option(VerifiedIdentity)`
    // param from whatever bearer token (if any) was presented, never a
    // 401 either way — a new, general capability, not workflow-specific.
    program.fns.push(FnDecl {
        name: format!("start_{}", to_snake_case(&w.name)),
        params: vec![
            Param {
                name: "identity".to_string(),
                ty: Ty::Named("Option".to_string(), vec![Ty::Named("VerifiedIdentity".to_string(), vec![])]),
            },
            Param { name: "data".to_string(), ty: Ty::Named(data_struct_name.clone(), vec![]) },
        ],
        ret: result_ty(Ty::I64),
        body: one_return(Expr::Call(
            "__workflow_start".to_string(),
            vec![Expr::Str(w.name.clone(), span), Expr::Ident("identity".to_string(), span), Expr::Ident("data".to_string(), span)],
            span,
        )),
        span,
        declared_effects: None,
        requires: None,
        explicit_public: false,
        module: None,
        ns: None,
        exported: true,
    });

    // 3b. `list_<workflow_snake>_submitted_by_me(identity: VerifiedIdentity)
    // -> Result(json, WorkflowActionError)` — `docs/WORKFLOW.md`'s "who
    // submitted this" section, read side: every instance whose
    // `started_by_subject` (`workflow_log.rs`) matches the caller's own
    // subject. Unlike `start_*` above, `identity` here is **required**,
    // not optional — "show me my own requests" is meaningless with no
    // identity to scope it to.
    program.fns.push(FnDecl {
        name: format!("list_{}_submitted_by_me", to_snake_case(&w.name)),
        params: vec![Param { name: "identity".to_string(), ty: Ty::Named("VerifiedIdentity".to_string(), vec![]) }],
        ret: result_ty(Ty::Json),
        body: one_return(Expr::Call(
            "__workflow_submitted_by_me".to_string(),
            vec![Expr::Str(w.name.clone(), span), Expr::Ident("identity".to_string(), span)],
            span,
        )),
        span,
        declared_effects: None,
        requires: None,
        explicit_public: false,
        module: None,
        ns: None,
        exported: true,
    });

    // 3c. `get_<workflow_snake>_history(identity: VerifiedIdentity,
    // instance_id: i64) -> Result(json, WorkflowActionError)` —
    // `docs/WORKFLOW.md`'s "audit trail" section: the full, append-only
    // transition log for one instance (who/when/via-link-or-not/comment,
    // oldest first). Gated to "any signed-in identity may view any
    // instance's history" — a disclosed simplification, not a per-viewer
    // ACL (only participants/the original requester, say) — see
    // `docs/WORKFLOW.md`'s own note on this.
    program.fns.push(FnDecl {
        name: format!("get_{}_history", to_snake_case(&w.name)),
        params: vec![
            Param { name: "identity".to_string(), ty: Ty::Named("VerifiedIdentity".to_string(), vec![]) },
            Param { name: "instance_id".to_string(), ty: Ty::I64 },
        ],
        ret: result_ty(Ty::Json),
        body: one_return(Expr::Call(
            "__workflow_history".to_string(),
            vec![Expr::Str(w.name.clone(), span), Expr::Ident("instance_id".to_string(), span)],
            span,
        )),
        span,
        declared_effects: None,
        requires: None,
        explicit_public: false,
        module: None,
        ns: None,
        exported: true,
    });

    // 4. `advance_<workflow_snake>(identity: VerifiedIdentity, instance_id:
    // i64, event: <Workflow>Event, payload: json) -> Result(bool,
    // WorkflowActionError)` — `identity` is a real, disclosed breaking
    // change from the pre-ownership signature (`docs/WORKFLOW.md`'s "state
    // ownership" section): one `advance_*` fn serves every instance/state
    // of this workflow, so *who* may fire the current state's transition
    // can only be checked at runtime, against the live instance
    // (`interpreter.rs::workflow_advance`), never via a static
    // `requires(...)` here — no `requires` is set on this fn itself, the
    // `VerifiedIdentity` param alone is what makes `serve.rs::dispatch`
    // demand *a* valid token (any signed-in caller may attempt any
    // transition; whether *this* caller may fire *this* instance's
    // current event is the interpreter-level `owner` check).
    program.fns.push(FnDecl {
        name: format!("advance_{}", to_snake_case(&w.name)),
        params: vec![
            Param { name: "identity".to_string(), ty: Ty::Named("VerifiedIdentity".to_string(), vec![]) },
            Param { name: "instance_id".to_string(), ty: Ty::I64 },
            Param { name: "event".to_string(), ty: Ty::Named(event_enum_name, vec![]) },
            Param { name: "payload".to_string(), ty: Ty::Json },
        ],
        ret: result_ty(Ty::Bool),
        body: one_return(Expr::Call(
            "__workflow_advance".to_string(),
            vec![
                Expr::Str(w.name.clone(), span),
                Expr::Ident("identity".to_string(), span),
                Expr::Ident("instance_id".to_string(), span),
                Expr::Ident("event".to_string(), span),
                Expr::Ident("payload".to_string(), span),
            ],
            span,
        )),
        span,
        declared_effects: None,
        requires: None,
        explicit_public: false,
        module: None,
        ns: None,
        exported: true,
    });

    // 4b. `list_<workflow_snake>_pending_for_me(identity: VerifiedIdentity)
    // -> Result(json, WorkflowActionError)` — `docs/WORKFLOW.md`'s "state
    // ownership" section, read side: every instance currently sitting in
    // a state whose `owner` `identity` satisfies. Same "gains a real
    // `VerifiedIdentity` param, no `requires`" posture as `advance_*`
    // above — this fn's caller only ever asks about *their own* pending
    // items, so no static role can name who may call it (anyone signed
    // in may ask "what's waiting on me", the query itself is what filters
    // by identity).
    program.fns.push(FnDecl {
        name: format!("list_{}_pending_for_me", to_snake_case(&w.name)),
        params: vec![Param { name: "identity".to_string(), ty: Ty::Named("VerifiedIdentity".to_string(), vec![]) }],
        ret: result_ty(Ty::Json),
        body: one_return(Expr::Call(
            "__workflow_pending_for_me".to_string(),
            vec![Expr::Str(w.name.clone(), span), Expr::Ident("identity".to_string(), span)],
            span,
        )),
        span,
        declared_effects: None,
        requires: None,
        explicit_public: false,
        module: None,
        ns: None,
        exported: true,
    });

    // 5. `<event>_via_link(instance_id: i64, token: <Workflow>LinkToken,
    // payload: json) -> Result(bool, WorkflowActionError)` — one per
    // distinct `link`-marked event, unauthenticated (no `requires`, no
    // `VerifiedIdentity` param), same shape `trade_finance.nir:637-689`'s
    // hand-written `decide_approval_via_link` already established.
    for event in &link_events {
        program.fns.push(FnDecl {
            name: format!("{}_via_link", to_snake_case(event)),
            params: vec![
                Param { name: "instance_id".to_string(), ty: Ty::I64 },
                Param { name: "token".to_string(), ty: Ty::Named(link_token_name.clone(), vec![]) },
                Param { name: "payload".to_string(), ty: Ty::Json },
            ],
            ret: result_ty(Ty::Bool),
            body: one_return(Expr::Call(
                "__workflow_link_advance".to_string(),
                vec![
                    Expr::Str(w.name.clone(), span),
                    Expr::Ident("instance_id".to_string(), span),
                    Expr::Call(event.clone(), vec![], span),
                    Expr::Ident("token".to_string(), span),
                    Expr::Ident("payload".to_string(), span),
                ],
                span,
            )),
            span,
            declared_effects: None,
            requires: None,
            // Genuinely intentional, not an oversight — the documented
            // carve-out `docs/API_TRUST_MODEL.md` §1 names explicitly (a
            // single-use magic-link token compare stands in for
            // `IdentityValid` here). Marking it so keeps the new
            // ungated-fn warning (`docs/ROADMAP.md` A10) silent for exactly
            // this one deliberately-public shape, not every `workflow`
            // function.
            explicit_public: true,
            module: None,
            ns: None,
            exported: true,
        });
    }

    Ok(())
}

fn one_return(value: Expr) -> Block {
    let span = value.span();
    Block { stmts: vec![Stmt::Return { value: Some(value), span }] }
}
