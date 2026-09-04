//! Tests for Row 12's relying-party identity model.
//!
//! The runtime validates externally-issued mock OIDC/JWT tokens and API keys
//! and returns a `VerifiedIdentity`; authorization views (`RoleView`,
//! `ClaimView`), application sessions, refresh-token handles, and revocation
//! checks are all derived from or managed alongside that identity, never
//! minted by the runtime itself.

use nirdosha::interpreter::Value;
use nirdosha::run;

const JWKS: &str = r#"{"keys":[{"kid":"key1","kty":"oct","k":"bXktc2VjcmV0LWtleQ"}]}"#;
const ISSUER: &str = "https://example.com";
const AUDIENCE: &str = "my-app";

fn escape_nir_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn run_with_token(token: &str) -> Value {
    let src = format!(
        r#"
struct Text {{
    value: str,
}}
fn handle_identity(identity: VerifiedIdentity) -> Text {{
    if check_revocation(identity) {{
        return Text("token revoked")
    }}
    if identity_expired(identity, 1800000000) {{
        return Text("token expired")
    }}
    return match check_role(identity, "physician") {{
        Ok(role_view) => match extract_claim(identity, "department") {{
            Ok(claim_view) => Text(claim_view.value),
            Err(e) => Text(e),
        }},
        Err(e) => Text(e),
    }}
}}

fn main() -> Text {{
    let token: str = "{token}"
    let jwks: str = "{jwks}"
    return match oidc_validate_token(token, "{issuer}", "{audience}", jwks) {{
        Ok(identity) => handle_identity(identity),
        Err(e) => Text(e),
    }}
}}
"#,
        token = escape_nir_str(token),
        jwks = escape_nir_str(JWKS),
        issuer = ISSUER,
        audience = AUDIENCE,
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
fn valid_token_reaches_derived_claim() {
    let token = "eyJhbGciOiAiSFMyNTYiLCAia2lkIjogImtleTEifQ.eyJzdWIiOiAiYWxpY2UiLCAiaXNzIjogImh0dHBzOi8vZXhhbXBsZS5jb20iLCAiYXVkIjogIm15LWFwcCIsICJleHAiOiAyMDAwMDAwMDAwLCAiaWF0IjogMTcwMDAwMDAwMCwgInJvbGVzIjogWyJwaHlzaWNpYW4iXSwgImRlcGFydG1lbnQiOiAiY2FyZGlvbG9neSJ9.nrFdeqNDwXWLeGzud6X9Q4ITzCXULzZBBK8y51LGYXs";
    expect_str(run_with_token(token), "cardiology");
}

#[test]
fn invalid_signature_is_rejected() {
    let token = "eyJhbGciOiAiSFMyNTYiLCAia2lkIjogImtleTEifQ.eyJzdWIiOiAiYWxpY2UiLCAiaXNzIjogImh0dHBzOi8vZXhhbXBsZS5jb20iLCAiYXVkIjogIm15LWFwcCIsICJleHAiOiAyMDAwMDAwMDAwLCAiaWF0IjogMTcwMDAwMDAwMCwgInJvbGVzIjogWyJwaHlzaWNpYW4iXSwgImRlcGFydG1lbnQiOiAiY2FyZGlvbG9neSJ9.badsignature";
    expect_str(run_with_token(token), "invalid JWT signature");
}

#[test]
fn wrong_issuer_is_rejected() {
    let token = "eyJhbGciOiAiSFMyNTYiLCAia2lkIjogImtleTEifQ.eyJzdWIiOiAiYWxpY2UiLCAiaXNzIjogImh0dHBzOi8vZXZpbC5jb20iLCAiYXVkIjogIm15LWFwcCIsICJleHAiOiAyMDAwMDAwMDAwLCAiaWF0IjogMTcwMDAwMDAwMCwgInJvbGVzIjogWyJwaHlzaWNpYW4iXSwgImRlcGFydG1lbnQiOiAiY2FyZGlvbG9neSJ9.EyF4RIdxLDOnS8Rakbtcx4B3XKJAkegp3AMhCeXTXHM";
    expect_str(run_with_token(token), "untrusted issuer: expected `https://example.com`, found `https://evil.com`");
}

#[test]
fn wrong_audience_is_rejected() {
    let token = "eyJhbGciOiAiSFMyNTYiLCAia2lkIjogImtleTEifQ.eyJzdWIiOiAiYWxpY2UiLCAiaXNzIjogImh0dHBzOi8vZXhhbXBsZS5jb20iLCAiYXVkIjogIm90aGVyLWFwcCIsICJleHAiOiAyMDAwMDAwMDAwLCAiaWF0IjogMTcwMDAwMDAwMCwgInJvbGVzIjogWyJwaHlzaWNpYW4iXSwgImRlcGFydG1lbnQiOiAiY2FyZGlvbG9neSJ9.97q7IvOeW_KRFCcCbnNqoZXVDVU5RT0yXtog_wSsFMc";
    expect_str(run_with_token(token), "wrong audience: expected `my-app`, found `other-app`");
}

#[test]
fn expired_token_is_rejected() {
    let token = "eyJhbGciOiAiSFMyNTYiLCAia2lkIjogImtleTEifQ.eyJzdWIiOiAiYWxpY2UiLCAiaXNzIjogImh0dHBzOi8vZXhhbXBsZS5jb20iLCAiYXVkIjogIm15LWFwcCIsICJleHAiOiAxNjAwMDAwMDAwLCAiaWF0IjogMTcwMDAwMDAwMCwgInJvbGVzIjogWyJwaHlzaWNpYW4iXSwgImRlcGFydG1lbnQiOiAiY2FyZGlvbG9neSJ9.6dm6M3cDDe30maZPcdeSKMhxTs4nuXjPZR4vGA29xS8";
    expect_str(run_with_token(token), "token expired");
}

#[test]
fn missing_role_is_rejected() {
    let token = "eyJhbGciOiAiSFMyNTYiLCAia2lkIjogImtleTEifQ.eyJzdWIiOiAiYWxpY2UiLCAiaXNzIjogImh0dHBzOi8vZXhhbXBsZS5jb20iLCAiYXVkIjogIm15LWFwcCIsICJleHAiOiAyMDAwMDAwMDAwLCAiaWF0IjogMTcwMDAwMDAwMCwgImRlcGFydG1lbnQiOiAiY2FyZGlvbG9neSJ9.PuURFQFuzXCoHU6CcXkEs4bdRt-h-D45NuorqiJ0-to";
    expect_str(run_with_token(token), "no field `roles`");
}

#[test]
fn wrong_role_is_rejected() {
    let token = "eyJhbGciOiAiSFMyNTYiLCAia2lkIjogImtleTEifQ.eyJzdWIiOiAiYWxpY2UiLCAiaXNzIjogImh0dHBzOi8vZXhhbXBsZS5jb20iLCAiYXVkIjogIm15LWFwcCIsICJleHAiOiAyMDAwMDAwMDAwLCAiaWF0IjogMTcwMDAwMDAwMCwgInJvbGVzIjogWyJhZG1pbiJdLCAiZGVwYXJ0bWVudCI6ICJjYXJkaW9sb2d5In0.TXzy6A5O7brd9kIC3lrwMTrJPE5vVGSg9CeXQkjfeIk";
    expect_str(run_with_token(token), "insufficient role: `physician`");
}

#[test]
fn missing_claim_is_rejected() {
    let token = "eyJhbGciOiAiSFMyNTYiLCAia2lkIjogImtleTEifQ.eyJzdWIiOiAiYWxpY2UiLCAiaXNzIjogImh0dHBzOi8vZXhhbXBsZS5jb20iLCAiYXVkIjogIm15LWFwcCIsICJleHAiOiAyMDAwMDAwMDAwLCAiaWF0IjogMTcwMDAwMDAwMCwgInJvbGVzIjogWyJwaHlzaWNpYW4iXX0.3aRakArWbV7MZlW4Ndz8h5cJ2Q_puNZxpYp7h5dBVRk";
    expect_str(run_with_token(token), "no field `department`");
}

#[test]
fn revoked_token_is_rejected() {
    // Unlike every other fixture in this file (a hand-encoded base64 JWT
    // with a pre-computed HS256 signature), this one is minted at test
    // time via `mock_issue_token` (dogfooding the language's own
    // builtin, the same way `tests/serve.rs::mint_token` already does) —
    // hand-computing a valid HMAC-SHA256 signature for a one-off claims
    // payload isn't practical to keep correct by hand, and a token with
    // a bad signature would be rejected for *that* reason before
    // `check_revocation` is ever reached, silently testing the wrong
    // thing (a real, fixed pre-existing bug this file had: the original
    // fixture here had a garbage signature, and `handle_identity` never
    // even called `check_revocation` at all).
    let mint_src = format!(
        r#"
struct Text {{
    value: str,
}}
fn main() -> Text {{
    return match mock_issue_token("alice", "{issuer}", "{audience}", 1700000000, 300000000, "{{\"roles\":[\"physician\"],\"department\":\"cardiology\",\"revoked\":true}}", "{jwks}") {{
        Ok(token) => Text(token),
        Err(e) => Text(e),
    }}
}}
"#,
        issuer = ISSUER,
        audience = AUDIENCE,
        jwks = escape_nir_str(JWKS),
    );
    let token = match run(&mint_src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => s.to_string(),
            other => panic!("expected Text(Str(_)), got Text({other:?})"),
        },
        other => panic!("expected a minted token, got {other:?}"),
    };
    expect_str(run_with_token(&token), "token revoked");
}

fn run_raw(src: &str) -> Value {
    run(src).unwrap_or_else(|e| panic!("runtime error: {e:?}"))
}

#[test]
fn api_key_with_valid_hash_succeeds() {
    let src = r#"
struct Text {
    value: str,
}
fn main() -> Text {
    let api_key: str = "my-secret-api-key"
    let expected_hash: str = "325ededd6c3b9988f623c7f964abb9b016b76b0f8b3474df0f7d7c23b941381f"
    return match validate_api_key(api_key, expected_hash) {
        Ok(identity) => match extract_claim(identity, "department") {
            Ok(claim_view) => Text(claim_view.value),
            Err(e) => Text(e),
        },
        Err(e) => Text(e),
    }
}
"#;
    expect_str(run_raw(src), "radiology");
}

#[test]
fn api_key_with_invalid_hash_is_rejected() {
    let src = r#"
struct Text {
    value: str,
}
fn main() -> Text {
    return match validate_api_key("wrong-key", "325ededd6c3b9988f623c7f964abb9b016b76b0f8b3474df0f7d7c23b941381f") {
        Ok(_) => Text("should not succeed"),
        Err(e) => Text(e),
    }
}
"#;
    expect_str(run_raw(src), "invalid API key");
}

#[test]
fn refresh_token_exchange_succeeds_when_not_expired() {
    let src = r#"
struct Text {
    value: str,
}
fn main() -> Text {
    let token: str = "eyJhbGciOiAiSFMyNTYiLCAia2lkIjogImtleTEifQ.eyJzdWIiOiAiYWxpY2UiLCAiaXNzIjogImh0dHBzOi8vZXhhbXBsZS5jb20iLCAiYXVkIjogIm15LWFwcCIsICJleHAiOiAyMDAwMDAwMDAwLCAiaWF0IjogMTcwMDAwMDAwMCwgInJvbGVzIjogWyJwaHlzaWNpYW4iXSwgImRlcGFydG1lbnQiOiAiY2FyZGlvbG9neSJ9.nrFdeqNDwXWLeGzud6X9Q4ITzCXULzZBBK8y51LGYXs"
    let jwks: str = "{\"keys\":[{\"kid\":\"key1\",\"kty\":\"oct\",\"k\":\"bXktc2VjcmV0LWtleQ\"}]}"
    return match oidc_validate_token(token, "https://example.com", "my-app", jwks) {
        Ok(identity) => match exchange_refresh_token(identity, new_refresh_token(2000000000), 1700000000) {
            Ok(refreshed) => match extract_claim(refreshed, "department") {
                Ok(claim_view) => Text(claim_view.value),
                Err(e) => Text(e),
            },
            Err(e) => Text(e),
        },
        Err(e) => Text(e),
    }
}
"#;
    expect_str(run_raw(src), "cardiology");
}

#[test]
fn refresh_token_exchange_fails_when_expired() {
    let src = r#"
struct Text {
    value: str,
}
fn main() -> Text {
    let token: str = "eyJhbGciOiAiSFMyNTYiLCAia2lkIjogImtleTEifQ.eyJzdWIiOiAiYWxpY2UiLCAiaXNzIjogImh0dHBzOi8vZXhhbXBsZS5jb20iLCAiYXVkIjogIm15LWFwcCIsICJleHAiOiAyMDAwMDAwMDAwLCAiaWF0IjogMTcwMDAwMDAwMCwgInJvbGVzIjogWyJwaHlzaWNpYW4iXSwgImRlcGFydG1lbnQiOiAiY2FyZGlvbG9neSJ9.nrFdeqNDwXWLeGzud6X9Q4ITzCXULzZBBK8y51LGYXs"
    let jwks: str = "{\"keys\":[{\"kid\":\"key1\",\"kty\":\"oct\",\"k\":\"bXktc2VjcmV0LWtleQ\"}]}"
    return match oidc_validate_token(token, "https://example.com", "my-app", jwks) {
        Ok(identity) => match exchange_refresh_token(identity, new_refresh_token(1600000000), 1700000000) {
            Ok(_) => Text("should not succeed"),
            Err(e) => Text(e),
        },
        Err(e) => Text(e),
    }
}
"#;
    expect_str(run_raw(src), "refresh token expired");
}

#[test]
fn application_session_cookie_is_generated() {
    let src = r#"
struct Text {
    value: str,
}
fn main() -> Text {
    let token: str = "eyJhbGciOiAiSFMyNTYiLCAia2lkIjogImtleTEifQ.eyJzdWIiOiAiYWxpY2UiLCAiaXNzIjogImh0dHBzOi8vZXhhbXBsZS5jb20iLCAiYXVkIjogIm15LWFwcCIsICJleHAiOiAyMDAwMDAwMDAwLCAiaWF0IjogMTcwMDAwMDAwMCwgInJvbGVzIjogWyJwaHlzaWNpYW4iXSwgImRlcGFydG1lbnQiOiAiY2FyZGlvbG9neSJ9.nrFdeqNDwXWLeGzud6X9Q4ITzCXULzZBBK8y51LGYXs"
    let jwks: str = "{\"keys\":[{\"kid\":\"key1\",\"kty\":\"oct\",\"k\":\"bXktc2VjcmV0LWtleQ\"}]}"
    return match oidc_validate_token(token, "https://example.com", "my-app", jwks) {
        Ok(identity) => Text(session_cookie(create_application_session(identity))),
        Err(e) => Text(e),
    }
}
"#;
    match run_raw(src) {
        Value::Struct(name, fields) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => {
                assert!(s.starts_with("session=example.com_alice_"), "unexpected cookie prefix: {s}");
                assert!(s.contains("HttpOnly"), "cookie missing HttpOnly: {s}");
                assert!(s.contains("Secure"), "cookie missing Secure: {s}");
                assert!(s.contains("SameSite=Strict"), "cookie missing SameSite: {s}");
            }
            other => panic!("expected Text(Str(cookie)), got Text({other:?})"),
        },
        other => panic!("expected Text(cookie), got {other:?}"),
    }
}

#[test]
fn example_runs_to_completion() {
    assert_eq!(run(include_str!("../../../examples/row12_identity.nir")), Ok(Value::Unit));
}
