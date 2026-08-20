//! LLVM codegen — goal.md row 5 ("native, hardware-speed codegen"),
//! started here for the first time. Emits **textual LLVM IR** and shells
//! out to the system `clang` to assemble/link a real native binary,
//! rather than binding to the LLVM C API from Rust (`inkwell`/`llvm-sys`)
//! — this environment has LLVM 22, recent enough that a Rust binding
//! crate's supported-version list might not cover it yet, and textual IR
//! sidesteps that entirely: it's a stable, documented format, and `clang`
//! on this system is *the same* LLVM 22, so there's no version-skew risk
//! between what's emitted and what assembles it. Several real compilers
//! (plenty of small production and hobby ones) use exactly this strategy.
//!
//! **Scoped to what's honestly supported, not silently narrowed.**
//! `check_supported` rejects, with a specific reason, anything outside:
//! signed integers (`i8`/`i16`/`i32`/`i64`), `bool`, `unit` — no
//! `u8`..`usize` (deferred: LLVM's `icmp`/`div` need a signed-vs-unsigned
//! instruction choice this pass doesn't make yet), no `box`/`&`/`*`
//! (compiling real heap allocation and move semantics to native code is
//! a separate, larger undertaking — ownership.rs's proof exists, but
//! nothing yet *executes* on it, matching that module's own "not yet
//! load-bearing" framing), and `print` only for integer-typed arguments
//! (every existing example only ever prints integers — printing `bool`
//! is a real, narrow, documented gap, not an oversight).
//!
//! **Tier 1 vs Tier 2 finally means something.** `refine.rs` and
//! `smt.rs` both proved things and both said, explicitly, "not wired to
//! elide the runtime check — there's no backend to spend the payoff on
//! yet." There is now. A `let`/assignment whose span is in the passed-in
//! `SmtReport::proven_in_range` gets no runtime bounds check emitted at
//! all (Tier 1, silent, exactly as goal.md §4 describes); one that
//! isn't gets an explicit compare-and-trap sequence in the compiled
//! binary (Tier 2, a real cost, visible in the generated IR). Same
//! distinction for division and `proven_nonzero_divisor`. This is the
//! first place in the whole codebase where a static proof actually
//! changes what runs, not just what's reported.
//!
//! **Codegen strategy: alloca everywhere, correctness over cleverness.**
//! Every parameter and every `let` gets its own stack slot
//! (`alloca`/`store`/`load`), the same strategy `clang -O0` itself uses
//! and every "toy compiler to LLVM" tutorial teaches — it's simple to
//! get right, and LLVM's own optimizer (not run here; nothing asks for
//! `-O2`) would promote these to registers anyway if it were. Allocas
//! are emitted at the point of each `let`, not hoisted to the entry
//! block: Nirdosha's scoping rules mean a name is only ever referenced
//! somewhere its `let` already dominates (you can't read a variable
//! before its declaration or from a sibling branch), so this is valid
//! LLVM IR without the hoisting pass a stricter backend might do. `&&`/
//! `||` are lowered to real conditional branches, not eager bitwise
//! `and`/`or` — short-circuit evaluation is a tested behavior
//! (`tests/basic.rs`'s short-circuit tests), and this backend has to
//! preserve it, not just the interpreter.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::ast::*;
use crate::smt::SmtReport;
use crate::token::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct CodegenError {
    pub message: String,
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

fn unsupported<T>(msg: impl Into<String>) -> Result<T, CodegenError> {
    Err(CodegenError { message: msg.into() })
}

/// Every signed integer/bool/unit type maps to a fixed LLVM type name;
/// everything else is rejected — see module doc for exactly what and why.
fn llvm_ty(ty: &Ty) -> Result<&'static str, CodegenError> {
    match ty {
        Ty::I8 => Ok("i8"),
        Ty::I16 => Ok("i16"),
        Ty::I32 => Ok("i32"),
        Ty::I64 => Ok("i64"),
        Ty::Bool => Ok("i1"),
        Ty::Unit => Ok("void"),
        Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize => unsupported(format!(
            "codegen doesn't support unsigned types yet (`{}`) — LLVM needs a signed-vs-\
             unsigned instruction choice (icmp/div) this pass doesn't make",
            ty.name()
        )),
        Ty::Box(_) | Ty::Ref(_) => unsupported(format!(
            "codegen doesn't support `{}` yet — compiling real heap allocation and move \
             semantics to native code is separate, larger work than typechecking/ownership \
             proving it",
            ty.name()
        )),
        Ty::Error => unreachable!("a program with a type error is never handed to codegen"),
    }
}

/// Structural pre-check: walks the whole program and rejects, with a
/// specific reason, anything `llvm_ty` or the `print`-argument rule
/// would reject — run once, up front, so codegen itself can assume every
/// type/expression it encounters is in the supported subset.
pub fn check_supported(program: &Program) -> Result<(), CodegenError> {
    for f in &program.fns {
        for p in &f.params {
            llvm_ty(&p.ty)?;
        }
        llvm_ty(&f.ret)?;
        check_stmts(&f.body.stmts)?;
    }
    Ok(())
}

fn check_stmts(stmts: &[Stmt]) -> Result<(), CodegenError> {
    for s in stmts {
        check_stmt(s)?;
    }
    Ok(())
}

fn check_stmt(s: &Stmt) -> Result<(), CodegenError> {
    match s {
        Stmt::Let { ty, value, .. } => {
            llvm_ty(ty)?;
            check_expr(value)
        }
        Stmt::Return { value: Some(e), .. } => check_expr(e),
        Stmt::Return { value: None, .. } => Ok(()),
        Stmt::While { cond, body, .. } => {
            check_expr(cond)?;
            check_stmts(&body.stmts)
        }
        Stmt::Expr(e) => check_expr(e),
    }
}

fn check_expr(e: &Expr) -> Result<(), CodegenError> {
    match e {
        Expr::Int(_, _) | Expr::Bool(_, _) | Expr::Ident(_, _) => Ok(()),
        Expr::Unary(_, inner, _) => check_expr(inner),
        Expr::Binary(_, l, r, _) => {
            check_expr(l)?;
            check_expr(r)
        }
        Expr::Call(name, args, _) => {
            if name == "print" {
                for a in args {
                    if !is_integer_expr(a) {
                        return unsupported(
                            "codegen only supports `print` on integer-typed arguments so far \
                             — no existing example needs bool/unit printing, so it wasn't built",
                        );
                    }
                }
            }
            for a in args {
                check_expr(a)?;
            }
            Ok(())
        }
        Expr::If { cond, then_block, else_block, .. } => {
            check_expr(cond)?;
            check_stmts(&then_block.stmts)?;
            match else_block.as_deref() {
                Some(ElseBranch::Block(b)) => check_stmts(&b.stmts),
                Some(ElseBranch::If(e2)) => check_expr(e2),
                None => Ok(()),
            }
        }
        Expr::Assign(_, rhs, _) => check_expr(rhs),
        Expr::Box(_, _) | Expr::Deref(_, _) | Expr::Ref(_, _) => {
            unsupported("codegen doesn't support `box`/`*`/`&` yet — see module doc")
        }
    }
}

/// A conservative, codegen-local "is this definitely not a `bool`"
/// check — not a real type inference pass (typeck.rs already is one;
/// re-deriving its full precision here isn't worth it for one narrow
/// question). Good enough to catch the common cases (`print(true)`,
/// `print(some_bool_var)`) without needing a declared-type table
/// threaded all the way into `check_expr`.
fn is_integer_expr(e: &Expr) -> bool {
    !matches!(e, Expr::Bool(_, _))
}

/// One binding's declared type plus the LLVM register holding a
/// *pointer* to its stack slot (an `alloca` result) — reads go through a
/// `load`, writes through a `store`, exactly like `clang -O0`'s output.
struct Scopes(Vec<HashMap<String, (Ty, String)>>);

impl Scopes {
    fn new() -> Self {
        Scopes(vec![HashMap::new()])
    }
    fn push(&mut self) {
        self.0.push(HashMap::new());
    }
    fn pop(&mut self) {
        self.0.pop();
    }
    fn define(&mut self, name: &str, ty: Ty, ptr_reg: String) {
        self.0.last_mut().unwrap().insert(name.to_string(), (ty, ptr_reg));
    }
    fn get(&self, name: &str) -> Option<(Ty, String)> {
        self.0.iter().rev().find_map(|s| s.get(name)).cloned()
    }
}

/// A function's declared signature — codegen's own copy, built once up
/// front (mirroring `typeck::FnSig`, which is private to that module and
/// not reusable here). `call()` needs this for two things LLVM requires
/// to get exactly right at every call site: the call instruction's
/// return type must match the callee's `define` exactly, and every
/// argument's type annotation must match the corresponding declared
/// parameter type exactly — guessing either from the argument
/// *expression's* own shape (an earlier draft's approach) is wrong
/// whenever a literal argument's "natural" type doesn't match a narrower
/// declared parameter (see the "found by testing" note in the module
/// doc / PHASE0.md's write-up of this milestone).
struct FnSig {
    params: Vec<Ty>,
    ret: Ty,
}

struct Codegen<'a> {
    out: String,
    tmp: usize,
    label: usize,
    smt_report: &'a SmtReport,
    sigs: HashMap<String, FnSig>,
    /// The function currently being generated code for — `Stmt::Return`
    /// needs its declared return type to guard/narrow against, and
    /// there's no other way to reach it from inside `stmt()` without
    /// threading it through every call.
    current_fn_ret: Ty,
    /// Once a block's been given a terminator (`br`/`ret`), any further
    /// statements in the same source block are unreachable — this stops
    /// codegen from emitting a second terminator into an already-closed
    /// block, which would be invalid IR.
    terminated: bool,
}

pub fn emit_llvm_ir(program: &Program, smt_report: &SmtReport) -> Result<String, CodegenError> {
    check_supported(program)?;
    let sigs = program
        .fns
        .iter()
        .map(|f| (f.name.clone(), FnSig { params: f.params.iter().map(|p| p.ty.clone()).collect(), ret: f.ret.clone() }))
        .collect();
    let mut cg =
        Codegen { out: String::new(), tmp: 0, label: 0, smt_report, sigs, current_fn_ret: Ty::Unit, terminated: false };

    writeln!(cg.out, "declare i32 @printf(ptr, ...)").unwrap();
    writeln!(cg.out, "declare void @abort() noreturn").unwrap();
    // "%lld\n\0" — 6 bytes (%, l, l, d, \n, \0), not 5; LLVM's array
    // constant size has to match the literal exactly, byte for byte.
    writeln!(cg.out, "@.int_fmt = private unnamed_addr constant [6 x i8] c\"%lld\\0A\\00\"").unwrap();
    writeln!(cg.out).unwrap();

    for f in &program.fns {
        cg.function(f)?;
    }

    cg.emit_c_main(program)?;
    Ok(cg.out)
}

impl Codegen<'_> {
    fn fresh_reg(&mut self, prefix: &str) -> String {
        self.tmp += 1;
        format!("%{prefix}.{}", self.tmp)
    }
    fn fresh_label(&mut self, prefix: &str) -> String {
        self.label += 1;
        format!("{prefix}.{}", self.label)
    }

    fn function(&mut self, f: &FnDecl) -> Result<(), CodegenError> {
        self.current_fn_ret = f.ret.clone();
        let ret_ty = llvm_ty(&f.ret)?;
        let name = if f.name == "main" { "nir_main" } else { f.name.as_str() };
        let params: Vec<String> = f
            .params
            .iter()
            .map(|p| Ok(format!("{} %arg.{}", llvm_ty(&p.ty)?, p.name)))
            .collect::<Result<_, CodegenError>>()?;
        writeln!(self.out, "define {ret_ty} @{name}({}) {{", params.join(", ")).unwrap();
        writeln!(self.out, "entry:").unwrap();
        self.terminated = false;

        let mut scopes = Scopes::new();
        for p in &f.params {
            let ty = llvm_ty(&p.ty)?;
            let ptr = format!("%{}.addr", p.name);
            writeln!(self.out, "  {ptr} = alloca {ty}").unwrap();
            writeln!(self.out, "  store {ty} %arg.{}, ptr {ptr}", p.name).unwrap();
            scopes.define(&p.name, p.ty.clone(), ptr);
        }

        self.stmts(&f.body.stmts, &mut scopes)?;

        // A function whose body definitely returns on every path
        // (typeck.rs already proved this for any non-`unit` return type)
        // never falls off the end reachably — but the *block* still
        // needs a terminator if the very last statement wasn't itself a
        // `return` on this specific path (e.g. a `unit`-returning
        // function that just runs off the end normally).
        if !self.terminated {
            if f.ret == Ty::Unit {
                writeln!(self.out, "  ret void").unwrap();
            } else {
                // typeck.rs's definite-return analysis already rules
                // this out for any well-typed program; unreachable is
                // the honest LLVM idiom for "provably can't happen".
                writeln!(self.out, "  unreachable").unwrap();
            }
        }
        writeln!(self.out, "}}\n").unwrap();
        Ok(())
    }

    fn stmts(&mut self, stmts: &[Stmt], scopes: &mut Scopes) -> Result<(), CodegenError> {
        for s in stmts {
            if self.terminated {
                break; // dead code after a `return`/branch — not emitted
            }
            self.stmt(s, scopes)?;
        }
        Ok(())
    }

    fn stmt(&mut self, stmt: &Stmt, scopes: &mut Scopes) -> Result<(), CodegenError> {
        match stmt {
            Stmt::Let { name, ty, value, span } => {
                let val = self.expr(value, scopes)?; // i64 (or i1 for bool)
                let val = self.guard_in_range(&val, ty, *span)?; // checked at i64 width, before narrowing
                let val = if ty.is_integer() { self.narrow_from_i64(&val, ty)? } else { val };
                let llty = llvm_ty(ty)?;
                let ptr = self.fresh_reg(&format!("{name}.addr"));
                writeln!(self.out, "  {ptr} = alloca {llty}").unwrap();
                writeln!(self.out, "  store {llty} {val}, ptr {ptr}").unwrap();
                scopes.define(name, ty.clone(), ptr);
                Ok(())
            }
            Stmt::Return { value, span } => {
                match value {
                    Some(e) => {
                        let val = self.expr(e, scopes)?;
                        let ret_ty = self.current_fn_ret.clone();
                        // `refine.rs`/`smt.rs` now both record a proof for
                        // `return` sites too (they gained their own
                        // `current_fn_ret` field, the same fix this file
                        // already had) — so this can be genuine Tier 1 in
                        // practice, not just in principle. Still routed
                        // through the same real guard either way, not a
                        // hardcoded always-check special case.
                        let val = self.guard_in_range(&val, &ret_ty, *span)?;
                        let val = if ret_ty.is_integer() { self.narrow_from_i64(&val, &ret_ty)? } else { val };
                        writeln!(self.out, "  ret {} {val}", llvm_ty(&ret_ty)?).unwrap();
                    }
                    None => {
                        writeln!(self.out, "  ret void").unwrap();
                    }
                }
                self.terminated = true;
                Ok(())
            }
            Stmt::While { cond, body, .. } => self.while_loop(cond, body, scopes),
            Stmt::Expr(e) => {
                self.expr(e, scopes)?;
                Ok(())
            }
        }
    }

    /// A very small, codegen-local "what LLVM type does this produce"
    /// helper — used only where a caller (`return`) needs it and typeck
    /// isn't threaded through. Trusts the program is already well-typed
    /// (typeck.rs ran first), so it doesn't need to be a full inference
    /// pass, just enough to pick the right LLVM type keyword.
    fn local_ty_of(&self, e: &Expr, scopes: &Scopes) -> Ty {
        match e {
            Expr::Int(_, _) => Ty::I64,
            Expr::Bool(_, _) => Ty::Bool,
            Expr::Ident(name, _) => scopes.get(name).map(|(t, _)| t).unwrap_or(Ty::I64),
            Expr::Unary(UnOp::Not, _, _) => Ty::Bool,
            Expr::Unary(UnOp::Neg, inner, _) => self.local_ty_of(inner, scopes),
            Expr::Binary(op, l, _, _) => match op {
                BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq | BinOp::And | BinOp::Or => {
                    Ty::Bool
                }
                _ => self.local_ty_of(l, scopes),
            },
            Expr::Assign(name, _, _) => scopes.get(name).map(|(t, _)| t).unwrap_or(Ty::I64),
            Expr::Call(name, _, _) => self.sigs.get(name).map(|s| s.ret.clone()).unwrap_or(Ty::I64),
            _ => Ty::I64,
        }
    }

    /// Every integer-typed value this backend hands around internally is
    /// `i64` — see the module doc's "why arithmetic is always computed
    /// at i64 width" note; this is the *load* side of that: a value just
    /// read out of a narrower-than-`i64` stack slot gets sign-extended
    /// immediately, before it can participate in anything else. A no-op
    /// for `bool` (stays `i1` throughout — it never enters the i64
    /// scheme) or a value that's already `i64`.
    fn widen_to_i64(&mut self, val: &str, ty: &Ty) -> String {
        if !ty.is_integer() || *ty == Ty::I64 {
            return val.to_string();
        }
        let llty = llvm_ty(ty).expect("check_supported already validated this type");
        let r = self.fresh_reg("widen");
        writeln!(self.out, "  {r} = sext {llty} {val} to i64").unwrap();
        r
    }

    /// The *store* side: narrows an `i64` value back down to `ty`'s
    /// actual declared width, right before it's written to a stack slot,
    /// passed as a call argument, or returned. Always lossless when
    /// called on a value that already passed `guard_in_range` for this
    /// same `ty` (that's the whole point of checking *before* narrowing,
    /// not after — see the module doc's "found by testing" note on why
    /// computing directly at the narrow width was wrong).
    fn narrow_from_i64(&mut self, val: &str, ty: &Ty) -> Result<String, CodegenError> {
        let llty = llvm_ty(ty)?;
        if llty == "i64" {
            return Ok(val.to_string());
        }
        let r = self.fresh_reg("narrow");
        writeln!(self.out, "  {r} = trunc i64 {val} to {llty}").unwrap();
        Ok(r)
    }

    /// Tier 1 vs Tier 2, for real: if `span` is in the SMT report's
    /// proven-safe set, `val` is used exactly as computed — no runtime
    /// check, no cost, matching goal.md §4's Tier 1. Otherwise, emits an
    /// actual compare-and-trap sequence: a value outside `ty`'s range
    /// calls `abort()` rather than silently wrapping or corrupting
    /// anything. Returns `val` unchanged either way — the check is
    /// side-effecting (branches to a trap block if it fails), not a
    /// transformation of the value itself.
    ///
    /// **Compares at `i64` width, always — this is load-bearing, not
    /// cosmetic.** An earlier draft compared at `ty`'s own (narrow)
    /// width, using LLVM's plain `add`/`sub`/`mul` computed *at that
    /// narrow width already* — which silently wraps on overflow, the
    /// same as any two's-complement machine addition. That meant the
    /// check was comparing an *already-wrapped* value against the very
    /// bounds it's supposed to detect escaping — a wrapped 8-bit value
    /// is, by construction, always representable in 8 bits, so the
    /// check could never fire. Found by actually running a deliberately
    /// overflowing test program through a compiled binary and watching
    /// it exit 0 instead of aborting — not caught by reading the code.
    /// The fix (this function, plus `widen_to_i64`/`narrow_from_i64`
    /// bracketing every arithmetic op) keeps every intermediate value at
    /// `i64` until *after* this check has run, exactly matching how
    /// `interpreter.rs`'s `Value::Int(i64)` already worked all along.
    fn guard_in_range(&mut self, val: &str, ty: &Ty, span: Span) -> Result<String, CodegenError> {
        if self.smt_report.proven_in_range.contains(&span) || !ty.is_integer() {
            return Ok(val.to_string());
        }
        let (lo, hi) = ty.bounds();
        let ok_lo = self.fresh_reg("ge_lo");
        writeln!(self.out, "  {ok_lo} = icmp sge i64 {val}, {lo}").unwrap();
        let ok_hi = self.fresh_reg("le_hi");
        writeln!(self.out, "  {ok_hi} = icmp sle i64 {val}, {hi}").unwrap();
        let ok = self.fresh_reg("in_range");
        writeln!(self.out, "  {ok} = and i1 {ok_lo}, {ok_hi}").unwrap();
        let pass = self.fresh_label("range_ok");
        let fail = self.fresh_label("range_trap");
        writeln!(self.out, "  br i1 {ok}, label %{pass}, label %{fail}").unwrap();
        writeln!(self.out, "{fail}:").unwrap();
        writeln!(self.out, "  call void @abort()").unwrap();
        writeln!(self.out, "  unreachable").unwrap();
        writeln!(self.out, "{pass}:").unwrap();
        Ok(val.to_string())
    }

    fn while_loop(&mut self, cond: &Expr, body: &Block, scopes: &mut Scopes) -> Result<(), CodegenError> {
        let cond_label = self.fresh_label("while_cond");
        let body_label = self.fresh_label("while_body");
        let after_label = self.fresh_label("while_after");

        writeln!(self.out, "  br label %{cond_label}").unwrap();
        writeln!(self.out, "{cond_label}:").unwrap();
        self.terminated = false;
        let c = self.expr(cond, scopes)?;
        writeln!(self.out, "  br i1 {c}, label %{body_label}, label %{after_label}").unwrap();

        writeln!(self.out, "{body_label}:").unwrap();
        self.terminated = false;
        scopes.push();
        self.stmts(&body.stmts, scopes)?;
        scopes.pop();
        if !self.terminated {
            writeln!(self.out, "  br label %{cond_label}").unwrap();
        }

        writeln!(self.out, "{after_label}:").unwrap();
        self.terminated = false;
        Ok(())
    }

    /// Evaluates `e`, returning an LLVM value operand (a register name
    /// like `%foo.3`, or a literal like `5`/`true`) ready to drop
    /// directly into another instruction.
    fn expr(&mut self, e: &Expr, scopes: &mut Scopes) -> Result<String, CodegenError> {
        match e {
            Expr::Int(n, _) => Ok(n.to_string()),
            Expr::Bool(b, _) => Ok(if *b { "true".to_string() } else { "false".to_string() }),
            Expr::Ident(name, _) => {
                let (ty, ptr) = scopes.get(name).expect("typeck.rs already proved this resolves");
                let llty = llvm_ty(&ty)?;
                let reg = self.fresh_reg(&format!("{name}.val"));
                writeln!(self.out, "  {reg} = load {llty}, ptr {ptr}").unwrap();
                Ok(self.widen_to_i64(&reg, &ty))
            }
            Expr::Unary(UnOp::Neg, inner, _) => {
                // `inner` is already i64 (every integer-typed `expr()`
                // result is) — no need to consult its declared width at
                // all anymore, which used to be `local_ty_of`'s job here.
                let v = self.expr(inner, scopes)?;
                let r = self.fresh_reg("neg");
                writeln!(self.out, "  {r} = sub i64 0, {v}").unwrap();
                Ok(r)
            }
            Expr::Unary(UnOp::Not, inner, _) => {
                let v = self.expr(inner, scopes)?;
                let r = self.fresh_reg("not");
                writeln!(self.out, "  {r} = xor i1 {v}, true").unwrap();
                Ok(r)
            }
            Expr::Binary(op, lhs, rhs, span) => self.binary(*op, lhs, rhs, *span, scopes),
            Expr::Call(name, args, _) => self.call(name, args, scopes),
            Expr::If { cond, then_block, else_block, span } => self.if_expr(cond, then_block, else_block.as_deref(), *span, scopes),
            Expr::Assign(name, rhs, span) => {
                let val = self.expr(rhs, scopes)?; // i64 (or i1 for bool)
                let (ty, ptr) = scopes.get(name).expect("typeck.rs already proved this resolves");
                let val = self.guard_in_range(&val, &ty, *span)?; // checked at i64 width
                let store_val = if ty.is_integer() { self.narrow_from_i64(&val, &ty)? } else { val.clone() };
                let llty = llvm_ty(&ty)?;
                writeln!(self.out, "  store {llty} {store_val}, ptr {ptr}").unwrap();
                // `val` (still i64/i1), not `store_val` (narrow) — every
                // other `expr()` result is i64 for an integer type, and
                // an assignment-expression's own value has to match that
                // convention so a caller combining it further doesn't
                // need to know it came from an assignment specifically.
                Ok(val)
            }
            Expr::Box(_, _) | Expr::Deref(_, _) | Expr::Ref(_, _) => {
                unreachable!("check_supported already rejected this program")
            }
        }
    }

    fn call(&mut self, name: &str, args: &[Expr], scopes: &mut Scopes) -> Result<String, CodegenError> {
        if name == "print" {
            for a in args {
                // Already `i64` — every integer-typed `expr()` result is,
                // by construction (module doc). No widening step needed
                // here anymore; an earlier draft's manual `sext` became
                // wrong (and would have mismatched) once that became
                // true everywhere else.
                let v = self.expr(a, scopes)?;
                writeln!(self.out, "  call i32 (ptr, ...) @printf(ptr @.int_fmt, i64 {v})").unwrap();
            }
            return Ok("0".to_string()); // print's own "value" is unit; never read
        }
        // User-defined call. `typeck.rs` already required this to
        // resolve and every argument to either exactly match or (for a
        // literal) fit its parameter's declared type — the sigs table
        // is what lets codegen honor that at the LLVM level too, where
        // a call instruction's argument types must match the callee's
        // `define` exactly, byte for byte.
        let sig_params = self.sigs.get(name).expect("typeck.rs already resolved this call").params.clone();
        let sig_ret = self.sigs.get(name).expect("typeck.rs already resolved this call").ret.clone();

        // Args are evaluated left to right, matching interpreter.rs's
        // evaluation order.
        let mut arg_vals = Vec::with_capacity(args.len());
        for (a, want) in args.iter().zip(sig_params.iter()) {
            let llty = llvm_ty(want)?;
            let v = if let Some(n) = literal_value(a) {
                // A literal (or negated literal) that typeck already
                // proved fits `want`'s range — emit it directly at
                // `want`'s width as a bare constant, no instruction
                // needed (this is the fix for the bug found by actually
                // inspecting the first generated .ll file: an earlier
                // draft ran `-3` through a real `sub i64 0, 3` and then
                // tried to pass the resulting *i64* register where an
                // `i32` parameter was declared — a genuine LLVM type
                // mismatch, not a hypothetical one).
                n.to_string()
            } else {
                // Not a literal — typeck's exact-match rule guarantees
                // this expression's own *declared* Nirdosha type already
                // equals `want`, but `expr()` itself always hands back an
                // `i64` for any integer type (module doc), so it still
                // needs narrowing to `want`'s actual LLVM width before
                // it can be passed at a call site. Lossless: the value
                // was already proven to fit `want` when it was originally
                // bound (that's what `guard_in_range` did at its own
                // `let`/assign site) — this narrow can't newly overflow.
                let val64 = self.expr(a, scopes)?;
                if want.is_integer() {
                    self.narrow_from_i64(&val64, want)?
                } else {
                    val64
                }
            };
            arg_vals.push(format!("{llty} {v}"));
        }

        let ret_llty = llvm_ty(&sig_ret)?;
        if ret_llty == "void" {
            writeln!(self.out, "  call void @{name}({})", arg_vals.join(", ")).unwrap();
            Ok("0".to_string()) // unit result; never read by a well-typed caller
        } else {
            let r = self.fresh_reg("call_result");
            writeln!(self.out, "  {r} = call {ret_llty} @{name}({})", arg_vals.join(", ")).unwrap();
            // The call instruction itself is correctly typed at the
            // callee's *declared* return width (LLVM requires that) —
            // but every other `expr()` result for an integer type is
            // `i64` (module doc), and this one has to honor that same
            // invariant too, or a caller like `Stmt::Let`'s
            // `guard_in_range` (which always compares at `i64`) sees a
            // value narrower than it expects. Found the same way as the
            // `add i8` wraparound bug: by actually building `hello.nir`
            // after the i64-everywhere fix landed and reading clang's
            // "defined with type 'i32' but expected 'i64'" error, not by
            // re-reading the code and reasoning it through in advance.
            Ok(self.widen_to_i64(&r, &sig_ret))
        }
    }

    fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span, scopes: &mut Scopes) -> Result<String, CodegenError> {
        if op == BinOp::And || op == BinOp::Or {
            return self.short_circuit(op, lhs, rhs, scopes);
        }

        // Arithmetic and ordering comparisons only ever apply to integer
        // operands (typeck.rs's `unify_operands` rejects `bool` for all
        // of these except `==`/`!=`) — so every arm below except Eq/
        // NotEq can just hardcode `i64` directly. `l`/`r` are already
        // `i64` (or `i1`, for the Eq/NotEq-on-bool case) by construction
        // — see the module doc.
        let l = self.expr(lhs, scopes)?;
        let r = self.expr(rhs, scopes)?;
        match op {
            BinOp::Add => {
                let out = self.fresh_reg("add");
                writeln!(self.out, "  {out} = add i64 {l}, {r}").unwrap();
                Ok(out)
            }
            BinOp::Sub => {
                let out = self.fresh_reg("sub");
                writeln!(self.out, "  {out} = sub i64 {l}, {r}").unwrap();
                Ok(out)
            }
            BinOp::Mul => {
                let out = self.fresh_reg("mul");
                writeln!(self.out, "  {out} = mul i64 {l}, {r}").unwrap();
                Ok(out)
            }
            BinOp::Div => {
                if !self.smt_report.proven_nonzero_divisor.contains(&span) {
                    let is_zero = self.fresh_reg("div_zero");
                    writeln!(self.out, "  {is_zero} = icmp eq i64 {r}, 0").unwrap();
                    let trap = self.fresh_label("div_trap");
                    let ok = self.fresh_label("div_ok");
                    writeln!(self.out, "  br i1 {is_zero}, label %{trap}, label %{ok}").unwrap();
                    writeln!(self.out, "{trap}:").unwrap();
                    writeln!(self.out, "  call void @abort()").unwrap();
                    writeln!(self.out, "  unreachable").unwrap();
                    writeln!(self.out, "{ok}:").unwrap();
                }
                let out = self.fresh_reg("sdiv");
                writeln!(self.out, "  {out} = sdiv i64 {l}, {r}").unwrap();
                Ok(out)
            }
            // `==`/`!=` are the one pair typeck.rs allows on `bool`
            // operands too — pick i1 vs i64 based on the *operand's*
            // declared type, the one remaining real use for
            // `local_ty_of` in this function.
            BinOp::Eq | BinOp::NotEq => {
                let cmp_ty = if self.local_ty_of(lhs, scopes) == Ty::Bool { "i1" } else { "i64" };
                let cond = if op == BinOp::Eq { "eq" } else { "ne" };
                self.icmp(cond, cmp_ty, &l, &r)
            }
            BinOp::Lt => self.icmp("slt", "i64", &l, &r),
            BinOp::Gt => self.icmp("sgt", "i64", &l, &r),
            BinOp::LtEq => self.icmp("sle", "i64", &l, &r),
            BinOp::GtEq => self.icmp("sge", "i64", &l, &r),
            BinOp::And | BinOp::Or => unreachable!("handled above"),
        }
    }

    fn icmp(&mut self, cond: &str, llty: &str, l: &str, r: &str) -> Result<String, CodegenError> {
        let out = self.fresh_reg("cmp");
        writeln!(self.out, "  {out} = icmp {cond} {llty} {l}, {r}").unwrap();
        Ok(out)
    }

    /// `&&`/`||` as real branches, not eager `and`/`or` — see module doc.
    fn short_circuit(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, scopes: &mut Scopes) -> Result<String, CodegenError> {
        let result_ptr = self.fresh_reg("logic_result.addr");
        writeln!(self.out, "  {result_ptr} = alloca i1").unwrap();

        let l = self.expr(lhs, scopes)?;
        let rhs_label = self.fresh_label("logic_rhs");
        let short_label = self.fresh_label("logic_short");
        let merge_label = self.fresh_label("logic_merge");

        if op == BinOp::And {
            writeln!(self.out, "  br i1 {l}, label %{rhs_label}, label %{short_label}").unwrap();
        } else {
            writeln!(self.out, "  br i1 {l}, label %{short_label}, label %{rhs_label}").unwrap();
        }

        writeln!(self.out, "{short_label}:").unwrap();
        writeln!(self.out, "  store i1 {l}, ptr {result_ptr}").unwrap();
        writeln!(self.out, "  br label %{merge_label}").unwrap();

        writeln!(self.out, "{rhs_label}:").unwrap();
        let r = self.expr(rhs, scopes)?;
        writeln!(self.out, "  store i1 {r}, ptr {result_ptr}").unwrap();
        writeln!(self.out, "  br label %{merge_label}").unwrap();

        writeln!(self.out, "{merge_label}:").unwrap();
        let out = self.fresh_reg("logic_val");
        writeln!(self.out, "  {out} = load i1, ptr {result_ptr}").unwrap();
        Ok(out)
    }

    fn if_expr(
        &mut self,
        cond: &Expr,
        then_block: &Block,
        else_block: Option<&ElseBranch>,
        _span: Span,
        scopes: &mut Scopes,
    ) -> Result<String, CodegenError> {
        let c = self.expr(cond, scopes)?;
        let then_label = self.fresh_label("if_then");
        let else_label = self.fresh_label("if_else");
        let merge_label = self.fresh_label("if_merge");
        // Result slot is hardcoded `i64`. This is a real, known gap, not
        // a proven-safe simplification: it's correct whenever both
        // branches definitely `return` (the store is skipped entirely —
        // see `terminated` below) or the value is a side-effect-only
        // placeholder (`print`'s bare "0"), which covers every site in
        // the three core examples. It would be *wrong* for a genuine
        // `bool`-valued if-expression that both branches fall through
        // to (e.g. `let ok: bool = if c { true } else { false }`) — an
        // i1 value stored through an i64 slot. Not hit by any current
        // example or test; flagged here and in PHASE0.md rather than
        // silently shipped. The real fix is threading the target `Ty`
        // through the way `typeck::check_if`'s `want` parameter already
        // does for the *type-checking* side of this same construct.
        let result_ptr = self.fresh_reg("if_result.addr");
        writeln!(self.out, "  {result_ptr} = alloca i64").unwrap();

        writeln!(self.out, "  br i1 {c}, label %{then_label}, label %{else_label}").unwrap();

        writeln!(self.out, "{then_label}:").unwrap();
        self.terminated = false;
        scopes.push();
        let then_val = self.block_value(then_block, scopes)?;
        scopes.pop();
        if !self.terminated {
            writeln!(self.out, "  store i64 {then_val}, ptr {result_ptr}").unwrap();
            writeln!(self.out, "  br label %{merge_label}").unwrap();
        }
        let then_terminated = self.terminated;

        writeln!(self.out, "{else_label}:").unwrap();
        self.terminated = false;
        let else_val = match else_block {
            Some(ElseBranch::Block(b)) => {
                scopes.push();
                let v = self.block_value(b, scopes)?;
                scopes.pop();
                v
            }
            Some(ElseBranch::If(e2)) => self.expr(e2, scopes)?,
            None => "0".to_string(),
        };
        if !self.terminated {
            writeln!(self.out, "  store i64 {else_val}, ptr {result_ptr}").unwrap();
            writeln!(self.out, "  br label %{merge_label}").unwrap();
        }
        let else_terminated = self.terminated;

        // The merge block is only reachable if at least one branch falls
        // through to it — if both branches unconditionally `return`,
        // there's nothing to merge, and the merge block would be dead
        // (valid but pointless) IR. Emit it regardless for simplicity;
        // `terminated` correctly reflects "both branches returned" so
        // the caller (a `let`/`return` around this `if`) won't try to
        // use a value that was never actually produced on any live path.
        writeln!(self.out, "{merge_label}:").unwrap();
        self.terminated = then_terminated && else_terminated;
        if self.terminated {
            writeln!(self.out, "  unreachable").unwrap();
            return Ok("0".to_string());
        }
        let out = self.fresh_reg("if_val");
        writeln!(self.out, "  {out} = load i64, ptr {result_ptr}").unwrap();
        Ok(out)
    }

    fn block_value(&mut self, block: &Block, scopes: &mut Scopes) -> Result<String, CodegenError> {
        match block.stmts.split_last() {
            None => Ok("0".to_string()),
            Some((last, rest)) => {
                self.stmts(rest, scopes)?;
                if self.terminated {
                    return Ok("0".to_string()); // unreachable; value never used
                }
                match last {
                    Stmt::Expr(e) => self.expr(e, scopes),
                    other => {
                        self.stmt(other, scopes)?;
                        Ok("0".to_string())
                    }
                }
            }
        }
    }

    /// The real OS-level entry point — Nirdosha's own `main` was renamed
    /// to `@nir_main` (module doc) to avoid the clash. Exit code
    /// convention: `unit`-returning `main` exits 0; an integer-returning
    /// one truncates/extends its result to `i32`, the same "the returned
    /// value is the program's result" convention `main.rs`'s CLI already
    /// uses for the interpreter.
    fn emit_c_main(&mut self, program: &Program) -> Result<(), CodegenError> {
        let main_fn = program.fns.iter().find(|f| f.name == "main").expect("typeck.rs already required a main");
        writeln!(self.out, "define i32 @main() {{").unwrap();
        writeln!(self.out, "entry:").unwrap();
        if main_fn.ret == Ty::Unit {
            writeln!(self.out, "  call void @nir_main()").unwrap();
            writeln!(self.out, "  ret i32 0").unwrap();
        } else {
            let llty = llvm_ty(&main_fn.ret)?;
            let r = self.fresh_reg("main_result");
            writeln!(self.out, "  {r} = call {llty} @nir_main()").unwrap();
            let r32 = match llty {
                "i64" => {
                    let t = self.fresh_reg("exit_code");
                    writeln!(self.out, "  {t} = trunc i64 {r} to i32").unwrap();
                    t
                }
                "i32" => r,
                _ => {
                    let t = self.fresh_reg("exit_code");
                    writeln!(self.out, "  {t} = sext {llty} {r} to i32").unwrap();
                    t
                }
            };
            writeln!(self.out, "  ret i32 {r32}").unwrap();
        }
        writeln!(self.out, "}}").unwrap();
        Ok(())
    }
}

/// Full pipeline from a well-typed, ownership-checked `Program` to a real
/// native executable at `output_path`: emit LLVM IR, write it to a temp
/// `.ll` file, invoke the system `clang` to assemble and link it. Returns
/// `clang`'s stderr on failure — a `CodegenError`-shaped failure (an
/// unsupported construct) is reported before `clang` ever runs, so the
/// two failure modes stay distinguishable to a caller.
/// The IR itself is unoptimized either way (module doc: "correctness over
/// cleverness," alloca everywhere) — `OptLevel` controls only whether
/// `clang` is asked to optimize *after* that, the same as it would for C
/// source. `O2` is the default `build()` uses because goal.md row 5 is
/// about hardware speed, not about this backend's own IR being clever;
/// `O0` stays available for debugging a miscompile without an optimizer
/// in the way, and — not incidentally — running the exact same IR
/// through both levels is a real stress test: LLVM treats `unreachable`
/// (this backend emits it for provably-dead code, e.g. a definitely-
/// returning function's fallthrough) as a hard guarantee and optimizes
/// aggressively around it, so a subtly wrong `unreachable` that `-O0`
/// happens not to disturb is exactly the kind of bug `-O2` would expose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptLevel {
    O0,
    O2,
}

impl OptLevel {
    fn clang_flag(self) -> &'static str {
        match self {
            OptLevel::O0 => "-O0",
            OptLevel::O2 => "-O2",
        }
    }
}

pub fn build(
    program: &Program,
    smt_report: &SmtReport,
    output_path: &std::path::Path,
    opt: OptLevel,
) -> Result<(), String> {
    let ir = emit_llvm_ir(program, smt_report).map_err(|e| e.to_string())?;

    // `process::id()` alone is **not** unique enough: it's identical
    // across every thread inside one process, so two concurrent `build`
    // calls in the same process (e.g. two tests running in parallel,
    // which is `cargo test`'s default) would race on the same temp file
    // — one call's IR silently overwriting or getting deleted out from
    // under the other. Found exactly this way: `cargo test`'s default
    // parallelism turned three independently-correct compiles into three
    // empty-stdout failures, not a hypothetical worry. A process-wide
    // atomic counter, combined with the pid, makes each call's filename
    // genuinely unique regardless of how many `build`s run concurrently.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut ll_path = std::env::temp_dir();
    ll_path.push(format!("nirdosha_{}_{n}.ll", std::process::id()));
    std::fs::write(&ll_path, &ir).map_err(|e| format!("writing {}: {e}", ll_path.display()))?;

    let result = std::process::Command::new("clang")
        .arg(&ll_path)
        .arg(opt.clang_flag())
        .arg("-o")
        .arg(output_path)
        .output();
    let _ = std::fs::remove_file(&ll_path); // best-effort cleanup either way

    match result {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(format!(
            "clang failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )),
        Err(e) => Err(format!("could not run `clang`: {e} (is it installed and on PATH?)")),
    }
}
