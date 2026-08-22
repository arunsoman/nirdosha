//! Tests for privileged first-class functions: `fn(..)->..` values, and
//! `requires(role: ..)`/`requires(claim: .., ..)` gating a function's
//! *value*, not just its behavior — the only way to obtain a callable
//! handle for a gated function is `acquire`, presenting a matching
//! `RoleView`/`ClaimView` proof from the existing identity feature
//! (`oidc_validate_token`/`check_role`/`extract_claim`). A direct call or
//! a bare value-reference to a gated function's name is a *static* error
//! (`TypeErrorKind::PrivilegedFnNotAcquired`), never a runtime one.

use nirdosha::ast::Requirement;
use nirdosha::interpreter::Value;
use nirdosha::run;
use nirdosha::typeck::TypeErrorKind;

fn parse_ok(src: &str) -> nirdosha::ast::Program {
    let toks = nirdosha::token::Lexer::new(src).tokenize().expect("lex should succeed");
    nirdosha::parser::Parser::new(toks).parse_program().expect("parse should succeed")
}

fn first_type_error(src: &str) -> TypeErrorKind {
    let program = parse_ok(src);
    match nirdosha::typeck::typecheck(&program) {
        Ok(()) => panic!("expected a type error, but the program type-checked cleanly"),
        Err(errors) => errors.into_iter().next().unwrap().kind,
    }
}

fn run_ok(src: &str) -> Value {
    run(src).unwrap_or_else(|e| panic!("runtime error: {e:?}"))
}

fn expect_int(v: Value, want: i64) {
    match v {
        Value::Int(n) => assert_eq!(n, want),
        other => panic!("expected Int({want}), got {other:?}"),
    }
}

fn expect_str(v: Value, want: &str) {
    match v {
        Value::Str(s) => assert_eq!(s.as_ref(), want),
        other => panic!("expected Str({want:?}), got {other:?}"),
    }
}

// ---- plain (ungated) first-class functions -----------------------------

#[test]
fn plain_fn_call_through_variable() {
    let src = r#"
        fn double(x: i64) -> i64 {
            return x * 2
        }
        fn main() -> i64 {
            let f: fn(i64) -> i64 = double
            return f(21)
        }
    "#;
    expect_int(run_ok(src), 42);
}

#[test]
fn plain_fn_as_higher_order_argument() {
    let src = r#"
        fn double(x: i64) -> i64 {
            return x * 2
        }
        fn apply(f: fn(i64) -> i64, x: i64) -> i64 {
            return f(x)
        }
        fn main() -> i64 {
            return apply(double, 21)
        }
    "#;
    expect_int(run_ok(src), 42);
}

// ---- requires(role: ..) --------------------------------------------------

const GATED_ROLE_SRC_PREFIX: &str = r#"
    fn transfer_funds(amount: i64) -> i64 requires(role: "admin") {
        return amount
    }
"#;

#[test]
fn direct_call_of_role_gated_fn_is_rejected_statically() {
    let src = format!(
        r#"{GATED_ROLE_SRC_PREFIX}
        fn main() -> i64 {{
            return transfer_funds(500)
        }}
    "#
    );
    assert_eq!(
        first_type_error(&src),
        TypeErrorKind::PrivilegedFnNotAcquired {
            name: "transfer_funds".to_string(),
            requirement: Requirement::Role("admin".to_string()),
        }
    );
}

#[test]
fn bare_reference_of_role_gated_fn_is_rejected_statically() {
    let src = format!(
        r#"{GATED_ROLE_SRC_PREFIX}
        fn main() -> i64 {{
            let f: fn(i64) -> i64 = transfer_funds
            return f(500)
        }}
    "#
    );
    assert_eq!(
        first_type_error(&src),
        TypeErrorKind::PrivilegedFnNotAcquired {
            name: "transfer_funds".to_string(),
            requirement: Requirement::Role("admin".to_string()),
        }
    );
}

#[test]
fn acquire_of_ungated_fn_is_rejected_statically() {
    // The proof comes from a real `check_role` call (never actually run —
    // this only typechecks) rather than being constructed directly:
    // `RoleView`/`ClaimView` aren't legal to construct via ordinary struct
    // syntax at all (`TypeErrorKind::UnforgeableProofConstruction`, a
    // fixed red-team finding — see typeck.rs's `infer_call`), precisely
    // because that would forge a proof unrelated to any validated
    // identity. `double` isn't gated, so the specific proof value or how
    // it was obtained doesn't matter for what this test actually checks —
    // `infer_acquire` reports `AcquireOfUngatedFn` before it ever looks at
    // the proof's type at all.
    let src = format!(
        r#"{GATED_ROLE_SRC_PREFIX}
        fn double(x: i64) -> i64 {{
            return x * 2
        }}
        fn main() -> i64 {{
            return match oidc_validate_token("{token}", "{issuer}", "{audience}", "{jwks}") {{
                Ok(identity) => match check_role(identity, "physician") {{
                    Ok(proof) => match acquire double(proof) {{
                        Ok(f) => f(21),
                        Err(e) => 0,
                    }},
                    Err(e) => -2,
                }},
                Err(e) => -3,
            }}
        }}
    "#,
        token = TOKEN,
        issuer = ISSUER,
        audience = AUDIENCE,
        jwks = escape_nir_str(JWKS),
    );
    assert_eq!(first_type_error(&src), TypeErrorKind::AcquireOfUngatedFn("double".to_string()));
}

// Every source below nests `match` as a single expression per arm — no
// `let`/`return` inside an arm — since `match` arm bodies only accept one
// expression (a pre-existing parser limitation, unrelated to this
// feature; see `row12_identity.rs`'s own passing tests for the same
// nested-match style).

#[test]
fn acquire_with_matching_role_succeeds_and_the_result_is_callable() {
    let src = format!(
        r#"{GATED_ROLE_SRC_PREFIX}
        fn main() -> i64 {{
            return match oidc_validate_token("{token}", "{issuer}", "{audience}", "{jwks}") {{
                Ok(identity) => match check_role(identity, "physician") {{
                    Ok(proof) => match acquire transfer_funds(proof) {{
                        Ok(f) => f(500),
                        Err(e) => -1,
                    }},
                    Err(e) => -2,
                }},
                Err(e) => -3,
            }}
        }}
    "#,
        token = TOKEN,
        issuer = ISSUER,
        audience = AUDIENCE,
        jwks = escape_nir_str(JWKS),
    );
    // `transfer_funds` requires role "admin"; the token only carries
    // "physician" — acquisition must fail, exercising the `Err` arm.
    expect_int(run_ok(&src), -1);
}

#[test]
fn acquire_with_correct_role_end_to_end() {
    let src = format!(
        r#"
        fn transfer_funds(amount: i64) -> i64 requires(role: "physician") {{
            return amount
        }}
        fn main() -> i64 {{
            return match oidc_validate_token("{token}", "{issuer}", "{audience}", "{jwks}") {{
                Ok(identity) => match check_role(identity, "physician") {{
                    Ok(proof) => match acquire transfer_funds(proof) {{
                        Ok(f) => f(500),
                        Err(e) => -1,
                    }},
                    Err(e) => -2,
                }},
                Err(e) => -3,
            }}
        }}
    "#,
        token = TOKEN,
        issuer = ISSUER,
        audience = AUDIENCE,
        jwks = escape_nir_str(JWKS),
    );
    expect_int(run_ok(&src), 500);
}

// ---- requires(claim: .., ..) ---------------------------------------------

#[test]
fn acquire_with_correct_claim_end_to_end() {
    let src = format!(
        r#"
        fn read_chart(patient_id: str) -> str requires(claim: "department", "cardiology") {{
            return patient_id
        }}
        fn main() -> str {{
            return match oidc_validate_token("{token}", "{issuer}", "{audience}", "{jwks}") {{
                Ok(identity) => match extract_claim(identity, "department") {{
                    Ok(proof) => match acquire read_chart(proof) {{
                        Ok(f) => f("patient-42"),
                        Err(e) => e,
                    }},
                    Err(e) => e,
                }},
                Err(e) => e,
            }}
        }}
    "#,
        token = TOKEN,
        issuer = ISSUER,
        audience = AUDIENCE,
        jwks = escape_nir_str(JWKS),
    );
    expect_str(run_ok(&src), "patient-42");
}

#[test]
fn acquire_with_wrong_claim_value_yields_err() {
    let src = format!(
        r#"
        fn read_chart(patient_id: str) -> str requires(claim: "department", "oncology") {{
            return patient_id
        }}
        fn main() -> str {{
            return match oidc_validate_token("{token}", "{issuer}", "{audience}", "{jwks}") {{
                Ok(identity) => match extract_claim(identity, "department") {{
                    Ok(proof) => match acquire read_chart(proof) {{
                        Ok(f) => f("patient-42"),
                        Err(e) => e,
                    }},
                    Err(e) => e,
                }},
                Err(e) => e,
            }}
        }}
    "#,
        token = TOKEN,
        issuer = ISSUER,
        audience = AUDIENCE,
        jwks = escape_nir_str(JWKS),
    );
    match run_ok(&src) {
        Value::Str(s) => assert!(s.contains("insufficient privilege"), "unexpected message: {s}"),
        other => panic!("expected Str, got {other:?}"),
    }
}

// Same mock JWT/JWKS fixtures `row12_identity.rs` already uses (copied
// rather than shared across test binaries — each integration test crate
// compiles independently). Claims: sub=alice, roles=["physician"],
// department="cardiology", exp far in the future. `JWKS` is real JSON
// (unescaped double quotes), so every use above substitutes it through
// `escape_nir_str` — a bare `"{jwks}"` would otherwise close the Nirdosha
// string literal early, the same reason `row12_identity.rs`'s own
// `run_with_token` helper escapes it.
const JWKS: &str = r#"{"keys":[{"kid":"key1","kty":"oct","k":"bXktc2VjcmV0LWtleQ"}]}"#;
const ISSUER: &str = "https://example.com";
const AUDIENCE: &str = "my-app";
const TOKEN: &str = "eyJhbGciOiAiSFMyNTYiLCAia2lkIjogImtleTEifQ.eyJzdWIiOiAiYWxpY2UiLCAiaXNzIjogImh0dHBzOi8vZXhhbXBsZS5jb20iLCAiYXVkIjogIm15LWFwcCIsICJleHAiOiAyMDAwMDAwMDAwLCAiaWF0IjogMTcwMDAwMDAwMCwgInJvbGVzIjogWyJwaHlzaWNpYW4iXSwgImRlcGFydG1lbnQiOiAiY2FyZGlvbG9neSJ9.nrFdeqNDwXWLeGzud6X9Q4ITzCXULzZBBK8y51LGYXs";

fn escape_nir_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
