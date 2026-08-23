//! Tests for `mock_issue_token` — the deliberate, explicitly-named
//! inverse of `oidc_validate_token` (see its doc comment in
//! `interpreter.rs`). A token this builtin mints must round-trip
//! through the real, unmodified `oidc_validate_token`/`check_role`/
//! `extract_claim` unchanged — that round-trip, not the token's raw
//! shape, is what makes it useful as a local mock identity provider.
//! Same mock JWKS fixture shape `tests/row12_identity.rs` and
//! `tests/privileged_fn.rs` already use.

use nirdosha::interpreter::Value;
use nirdosha::run;

// Pre-escaped for direct embedding inside a Nirdosha string literal
// (`\"` is one of the few escapes `str` literals support -- LANGUAGE.md
// §2) -- same fixture shape `tests/row12_identity.rs`/
// `tests/privileged_fn.rs` already use, just written so `format!` can
// splice it straight into a `let jwks: str = "{JWKS}"` line below
// without breaking the surrounding literal on its own embedded `"`s.
const JWKS: &str = r#"{\"keys\":[{\"kid\":\"key1\",\"kty\":\"oct\",\"k\":\"bXktc2VjcmV0LWtleQ\"}]}"#;

#[test]
fn minted_token_round_trips_through_real_validation() {
    let src = format!(
        r#"
        struct Text {{
            value: str,
        }}
        fn main() -> Text {{
            let jwks: str = "{JWKS}"
            return match mock_issue_token("alice", "https://mock-idp.local", "store-app", 1700000000, 3600, "{{\"roles\":[\"admin\"]}}", jwks) {{
                Ok(token) => match oidc_validate_token(token, "https://mock-idp.local", "store-app", jwks) {{
                    Ok(identity) => Text(identity.subject),
                    Err(e) => Text(e),
                }},
                Err(e) => Text(e),
            }}
        }}
    "#
    );
    match run(&src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "alice"),
            other => panic!("expected Text(Str(\"alice\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"alice\")), got {other:?}"),
    }
}

#[test]
fn embedded_claims_survive_the_round_trip_for_check_role() {
    let src = format!(
        r#"
        struct Text {{
            value: str,
        }}
        fn main() -> Text {{
            let jwks: str = "{JWKS}"
            return match mock_issue_token("bob", "https://mock-idp.local", "store-app", 1700000000, 3600, "{{\"roles\":[\"admin\"]}}", jwks) {{
                Ok(token) => match oidc_validate_token(token, "https://mock-idp.local", "store-app", jwks) {{
                    Ok(identity) => match check_role(identity, "admin") {{
                        Ok(proof) => Text(proof.role),
                        Err(e) => Text(e),
                    }},
                    Err(e) => Text(e),
                }},
                Err(e) => Text(e),
            }}
        }}
    "#
    );
    match run(&src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "admin"),
            other => panic!("expected Text(Str(\"admin\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"admin\")), got {other:?}"),
    }
}

#[test]
fn expired_token_is_rejected_by_real_validation_downstream() {
    // `issued_at` far in the past + a 1-second ttl means `exp` is already
    // behind any real clock -- but `oidc_validate_token` itself doesn't
    // check `exp` against wall-clock time (that's `identity_expired`'s
    // job, called separately), so this instead proves the more basic
    // claim: a `mock_issue_token`-minted token is a well-formed JWT that
    // validates cleanly regardless of how short its ttl was set.
    let src = format!(
        r#"
        fn main() -> bool {{
            let jwks: str = "{JWKS}"
            return match mock_issue_token("carol", "https://mock-idp.local", "store-app", 1000000000, 1, "{{}}", jwks) {{
                Ok(token) => match oidc_validate_token(token, "https://mock-idp.local", "store-app", jwks) {{
                    Ok(identity) => identity_expired(identity, 2000000000),
                    Err(e) => false,
                }},
                Err(e) => false,
            }}
        }}
    "#
    );
    match run(&src) {
        Ok(Value::Bool(b)) => assert!(b, "a token issued at 1_000_000_000 with a 1s ttl should read as expired at 2_000_000_000"),
        other => panic!("expected Ok(Bool(true)), got {other:?}"),
    }
}
