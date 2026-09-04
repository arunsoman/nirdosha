//! Tests for `check_role_path`/`extract_claim_path` — `check_role`/
//! `extract_claim`'s additive, dotted-path siblings for IdPs that nest
//! the roles array or a claim under a path instead of a flat top-level
//! field (Keycloak's `realm_access.roles`, for one real example).
//! `check_role`/`extract_claim` themselves are untouched — see
//! `interpreter.rs::json_field_path`'s doc comment on why a flat claim
//! whose own name contains a literal dot (Auth0-style namespaced claims)
//! must stay on the flat form, never the dotted-path one.
//!
//! Self-contained: mints its own token via `mock_issue_token` (the
//! deliberate, explicitly-named inverse of `oidc_validate_token`) rather
//! than hand-crafting a raw JWT string, so a nested `claims_json` shape
//! is just an ordinary string literal.

use nirdosha::interpreter::Value;
use nirdosha::run;

const JWKS: &str = r#"{"keys":[{"kid":"key1","kty":"oct","k":"bXktc2VjcmV0LWtleQ"}]}"#;

fn run_with_claims(claims_json: &str, body: &str) -> Value {
    let src = format!(
        r#"
struct Text {{
    value: str,
}}

fn handle_identity(identity: VerifiedIdentity) -> Text {{
    {body}
}}

fn main() -> Text {{
    let claims_json: str = "{claims_json}"
    let jwks: str = "{jwks}"
    return match mock_issue_token("alice", "https://example.com", "my-app", 1700000000, 999999999, claims_json, jwks) {{
        Ok(token) => match oidc_validate_token(token, "https://example.com", "my-app", jwks) {{
            Ok(identity) => handle_identity(identity),
            Err(e) => Text(e),
        }},
        Err(e) => Text(e),
    }}
}}
"#,
        claims_json = claims_json.replace('\\', "\\\\").replace('"', "\\\""),
        jwks = JWKS.replace('\\', "\\\\").replace('"', "\\\""),
        body = body,
    );
    run(&src).unwrap_or_else(|e| panic!("runtime error: {e:?}"))
}

fn expect_str(v: Value, want: &str) {
    match v {
        Value::Struct(name, fields) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, want, "unexpected string result"),
            other => panic!("expected Text(Str({want:?})), got Text({other:?})"),
        },
        other => panic!("expected Text({want:?}), got {other:?}"),
    }
}

#[test]
fn check_role_path_finds_a_role_nested_under_an_object() {
    let v = run_with_claims(
        r#"{"realm_access":{"roles":["admin","viewer"]}}"#,
        r#"return match check_role_path(identity, "realm_access.roles", "admin") {
            Ok(view) => Text(view.role),
            Err(e) => Text(e),
        }"#,
    );
    expect_str(v, "admin");
}

#[test]
fn check_role_path_rejects_a_role_not_in_the_nested_array() {
    let v = run_with_claims(
        r#"{"realm_access":{"roles":["viewer"]}}"#,
        r#"return match check_role_path(identity, "realm_access.roles", "admin") {
            Ok(view) => Text(view.role),
            Err(e) => Text(e),
        }"#,
    );
    expect_str(v, "insufficient role: `admin`");
}

#[test]
fn extract_claim_path_reads_a_nested_string_claim() {
    let v = run_with_claims(
        r#"{"profile":{"department":"cardiology"}}"#,
        r#"return match extract_claim_path(identity, "profile.department") {
            Ok(view) => Text(view.value),
            Err(e) => Text(e),
        }"#,
    );
    expect_str(v, "cardiology");
}

#[test]
fn extract_claim_path_on_a_missing_segment_is_a_clean_error_not_a_panic() {
    let v = run_with_claims(
        r#"{"profile":{"department":"cardiology"}}"#,
        r#"return match extract_claim_path(identity, "profile.title") {
            Ok(view) => Text(view.value),
            Err(e) => Text(e),
        }"#,
    );
    expect_str(v, "no field `title`");
}

#[test]
fn plain_check_role_and_extract_claim_are_unaffected_by_the_new_builtins() {
    // Same flat-claim shape `check_role`/`extract_claim` always
    // supported, run through the exact same builtins as before --
    // regression guard that adding the path-based siblings didn't touch
    // their behavior at all.
    let v = run_with_claims(
        r#"{"roles":["admin"],"department":"radiology"}"#,
        r#"return match check_role(identity, "admin") {
            Ok(_) => match extract_claim(identity, "department") {
                Ok(view) => Text(view.value),
                Err(e) => Text(e),
            },
            Err(e) => Text(e),
        }"#,
    );
    expect_str(v, "radiology");
}
