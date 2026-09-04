//! Tests for `effect(...)` (`docs/PROTOLANG_PORT.md`'s "Locked design 1") —
//! fully inferred by default, checked against a declared annotation only
//! when one is present. No new I/O keywords: `effect` is the only new
//! reserved word; `pure`/`rng`/`io`/`concurrent`/`network` are matched by
//! identifier text inside the parens, same treatment `transact`'s slot
//! names already get.

use nirdosha::ast::{Effect, TypeRegistry};
use nirdosha::effects::infer_effects;
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::{typecheck, TypeErrorKind};

fn parse_ok(src: &str) -> nirdosha::ast::Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    Parser::new(toks).parse_program().expect("parse should succeed")
}

fn first_type_error(src: &str) -> TypeErrorKind {
    let program = parse_ok(src);
    match typecheck(&program) {
        Ok(()) => panic!("expected a type error, but the program type-checked cleanly"),
        Err(errors) => errors.into_iter().next().unwrap().kind,
    }
}

// ---- inference, no declared annotation at all ------------------------------

#[test]
fn an_undeclared_function_is_still_fully_inferred() {
    let program = parse_ok(
        r#"
        fn helper() -> i64 {
            print(1)
            return 1
        }
        fn main() -> i64 {
            return helper()
        }
    "#,
    );
    typecheck(&program).expect("no annotation means nothing to check against");
    let effects = infer_effects(&program, &TypeRegistry::build(&program));
    assert_eq!(effects["helper"].inferred, [Effect::Io].into_iter().collect());
    // `main` calls `helper`, so it inherits `helper`'s effect too —
    // effect polymorphism by default (docs/goal.md §3's "Effects" layer).
    assert_eq!(effects["main"].inferred, [Effect::Io].into_iter().collect());
}

#[test]
fn arithmetic_and_dense_linear_algebra_are_pure() {
    let program = parse_ok(
        r#"
        fn main() -> f64 {
            let v: Vector(f64, 3) = [1.0, 2.0, 3.0]
            let n: f64 = norm(v)
            return n + 1.0
        }
    "#,
    );
    typecheck(&program).expect("should typecheck cleanly");
    let effects = infer_effects(&program, &TypeRegistry::build(&program));
    assert!(effects["main"].inferred.is_empty(), "expected the empty (pure) set, got {:?}", effects["main"].inferred);
}

#[test]
fn rand_calls_are_the_rng_effect_not_io() {
    let program = parse_ok(
        r#"
        fn main() -> f64 {
            rand_seed(1)
            return rand_f64()
        }
    "#,
    );
    typecheck(&program).expect("should typecheck cleanly");
    let effects = infer_effects(&program, &TypeRegistry::build(&program));
    assert_eq!(effects["main"].inferred, [Effect::Rng].into_iter().collect());
}

#[test]
fn spawn_and_join_are_the_concurrent_effect() {
    let program = parse_ok(
        r#"
        fn worker() -> i64 {
            return 1
        }
        fn main() -> i64 {
            let t: thread i64 = spawn worker()
            return join(t)
        }
    "#,
    );
    typecheck(&program).expect("should typecheck cleanly");
    let effects = infer_effects(&program, &TypeRegistry::build(&program));
    assert_eq!(effects["main"].inferred, [Effect::Concurrent].into_iter().collect());
}

#[test]
fn tcp_send_is_the_network_effect_not_concurrent() {
    let program = parse_ok(
        r#"
        fn main() -> i64 {
            let conn: tcp = connect("localhost", 80)
            send(conn, "hi")
            stop conn
            return 0
        }
    "#,
    );
    typecheck(&program).expect("should typecheck cleanly");
    let effects = infer_effects(&program, &TypeRegistry::build(&program));
    assert_eq!(effects["main"].inferred, [Effect::Network].into_iter().collect());
}

#[test]
fn chan_send_is_the_concurrent_effect_not_network() {
    let program = parse_ok(
        r#"
        fn main() -> i64 {
            let c: chan i64 = chan
            send(c, 1)
            return recv(c)
        }
    "#,
    );
    typecheck(&program).expect("should typecheck cleanly");
    let effects = infer_effects(&program, &TypeRegistry::build(&program));
    assert_eq!(effects["main"].inferred, [Effect::Concurrent].into_iter().collect());
}

#[test]
fn file_io_is_the_io_effect_not_network() {
    let program = parse_ok(
        r#"
        fn main() -> unit {
            let f: file = open("/tmp/nirdosha_effects_test.txt", "w")
            send(f, "hi")
            stop f
        }
    "#,
    );
    typecheck(&program).expect("should typecheck cleanly");
    let effects = infer_effects(&program, &TypeRegistry::build(&program));
    assert_eq!(effects["main"].inferred, [Effect::Io].into_iter().collect());
}

#[test]
fn db_connect_with_a_literal_sqlite_path_is_the_io_effect_only() {
    let program = parse_ok(
        r#"
        fn main() -> unit {
            match db_connect(":memory:") {
                Ok(conn) => stop conn,
                Err(e) => print(e),
            }
        }
    "#,
    );
    typecheck(&program).expect("should typecheck cleanly");
    let effects = infer_effects(&program, &TypeRegistry::build(&program));
    assert_eq!(effects["main"].inferred, [Effect::Io].into_iter().collect());
}

#[test]
fn db_connect_with_a_literal_postgres_url_is_also_the_network_effect() {
    let program = parse_ok(
        r#"
        fn main() -> unit {
            match db_connect("postgres://user@host/db") {
                Ok(conn) => stop conn,
                Err(e) => print(e),
            }
        }
    "#,
    );
    typecheck(&program).expect("should typecheck cleanly");
    let effects = infer_effects(&program, &TypeRegistry::build(&program));
    assert_eq!(effects["main"].inferred, [Effect::Io, Effect::Network].into_iter().collect());
}

#[test]
fn db_connect_with_a_non_literal_connection_string_conservatively_needs_network_too() {
    // The connection string isn't a literal *at the call site* (it's a
    // local binding), so `db_connect_effect` can't rule out Postgres at
    // compile time and has to assume the worse case rather than silently
    // under-report -- same reasoning as the call-through-value case above.
    let program = parse_ok(
        r#"
        fn main() -> unit {
            let conn_str: str = ":memory:"
            match db_connect(conn_str) {
                Ok(conn) => stop conn,
                Err(e) => print(e),
            }
        }
    "#,
    );
    typecheck(&program).expect("should typecheck cleanly");
    let effects = infer_effects(&program, &TypeRegistry::build(&program));
    assert_eq!(effects["main"].inferred, [Effect::Io, Effect::Network].into_iter().collect());
}

#[test]
fn declaring_only_io_for_a_literal_postgres_db_connect_is_a_type_error() {
    let kind = first_type_error(
        r#"
        fn main() -> unit effect(io) {
            match db_connect("postgres://user@host/db") {
                Ok(conn) => stop conn,
                Err(e) => print(e),
            }
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::EffectNotDeclared { fn_name: "main".to_string(), missing: Effect::Network });
}

#[test]
fn mutual_recursion_still_converges_to_the_right_effect_set() {
    // Neither function directly performs an effect on its own base case,
    // but `is_odd` calls `is_even` (io) and vice versa — the fixpoint
    // iteration has to propagate that through the cycle both ways.
    let program = parse_ok(
        r#"
        fn is_even(n: i64) -> bool {
            if n == 0 {
                return true
            }
            print(n)
            return is_odd(n - 1)
        }
        fn is_odd(n: i64) -> bool {
            if n == 0 {
                return false
            }
            return is_even(n - 1)
        }
        fn main() -> bool {
            return is_even(4)
        }
    "#,
    );
    typecheck(&program).expect("should typecheck cleanly");
    let effects = infer_effects(&program, &TypeRegistry::build(&program));
    assert_eq!(effects["is_even"].inferred, [Effect::Io].into_iter().collect());
    assert_eq!(effects["is_odd"].inferred, [Effect::Io].into_iter().collect());
}

// ---- enforcement against a declared annotation -----------------------------

#[test]
fn a_correctly_declared_pure_function_typechecks() {
    let program = parse_ok(
        r#"
        fn add(a: i64, b: i64) -> i64 effect(pure) {
            return a + b
        }
        fn main() -> i64 {
            return add(1, 2)
        }
    "#,
    );
    typecheck(&program).expect("should typecheck cleanly");
}

#[test]
fn declaring_pure_on_a_function_that_prints_is_a_type_error() {
    let kind = first_type_error(
        r#"
        fn oops() -> unit effect(pure) {
            print(1)
        }
        fn main() -> unit {
            oops()
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::EffectNotDeclared { fn_name: "oops".to_string(), missing: Effect::Io });
}

#[test]
fn declaring_pure_while_calling_through_a_function_valued_parameter_that_prints_is_a_type_error() {
    // A fixed red-team finding: `effects.rs`'s `Expr::Call` arm only
    // ever attributed effects via a global-name lookup, so a call
    // through a local `fn(...)->...` parameter (an ordinary higher-order
    // argument, no `acquire` involved at all) silently contributed
    // nothing — `caller` here used to typecheck clean as `effect(pure)`
    // no matter what `f` actually did at runtime.
    let kind = first_type_error(
        r#"
        fn side_effect() -> i64 {
            print(42)
            return 1
        }
        fn caller(f: fn() -> i64) -> i64 effect(pure) {
            return f()
        }
        fn main() -> i64 {
            return caller(side_effect)
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::EffectNotDeclared { fn_name: "caller".to_string(), missing: Effect::Io });
}

#[test]
fn declaring_a_broader_effect_than_actually_used_is_not_an_error() {
    // Effect subsumption (docs/goal.md §3): declaring more than you use is
    // fine, only the reverse is checked.
    let program = parse_ok(
        r#"
        fn main() -> unit effect(io, network) {
            print(1)
        }
    "#,
    );
    typecheck(&program).expect("declaring an unused effect must not be rejected");
}

#[test]
fn a_transitive_effect_through_a_call_must_still_be_declared() {
    let kind = first_type_error(
        r#"
        fn logger(msg: i64) -> unit {
            print(msg)
        }
        fn oops() -> unit effect(pure) {
            logger(1)
        }
        fn main() -> unit {
            oops()
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::EffectNotDeclared { fn_name: "oops".to_string(), missing: Effect::Io });
}

// ---- static rejections ------------------------------------------------------

#[test]
fn pure_cannot_be_combined_with_another_effect() {
    let toks = Lexer::new(
        r#"
        fn main() -> unit effect(pure, io) {
            print(1)
        }
    "#,
    )
    .tokenize()
    .expect("lex should succeed");
    let err = Parser::new(toks).parse_program().expect_err("combining `pure` with another effect must be a parse error");
    assert!(err.message.contains("pure"), "unexpected message: {}", err.message);
}

#[test]
fn an_unknown_effect_name_is_a_parse_error() {
    let toks = Lexer::new(
        r#"
        fn main() -> unit effect(networking) {
            print(1)
        }
    "#,
    )
    .tokenize()
    .expect("lex should succeed");
    Parser::new(toks).parse_program().expect_err("an unrecognized effect name must be a parse error");
}
