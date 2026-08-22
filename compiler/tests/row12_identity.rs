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
fn handle_identity(identity: VerifiedIdentity) -> str {{
    if identity_expired(identity, 1800000000) {{
        return "token expired"
    }}
    return match check_role(identity, "physician") {{
        Ok(role_view) => match extract_claim(identity, "department") {{
            Ok(claim_view) => claim_view.value,
            Err(e) => e,
        }},
        Err(e) => e,
    }}
}}

fn main() -> str {{
    let token: str = "{token}"
    let jwks: str = "{jwks}"
    return match oidc_validate_token(token, "{issuer}", "{audience}", jwks) {{
        Ok(identity) => handle_identity(identity),
        Err(e) => e,
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
        Value::Str(s) => assert_eq!(&*s, want, "unexpected string result"),
        other => panic!("expected Str({want:?}), got {other:?}"),
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
    let token = "eyJhbGciOiAiSFMyNTYiLCAia2lkIjogImtleTEifQ.eyJzdWIiOiAiYWxpY2UiLCAiaXNzIjogImh0dHBzOi8vZXhhbXBsZS5jb20iLCAiYXVkIjogIm15LWFwcCIsICJleHAiOiAyMDAwMDAwMDAwLCAiaWF0IjogMTcwMDAwMDAwMCwgInJvbGVzIjogWyJwaHlzaWNpYW4iXSwgImRlcGFydG1lbnQiOiAiY2FyZGlvbG9neSIsICJyZXZva2VkIjogdHJ1ZX0.q2m7q2m7q2m7q2m7q2m7q2m7q2m7q2m7q2m7q2m7q2m7q2m7q2m7";
    expect_str(run_with_token(token), "token revoked");
}

fn run_raw(src: &str) -> Value {
    run(src).unwrap_or_else(|e| panic!("runtime error: {e:?}"))
}

#[test]
fn api_key_with_valid_hash_succeeds() {
    let src = r#"
fn main() -> str {
    let api_key: str = "my-secret-api-key"
    let expected_hash: str = "325ededd6c3b9988f623c7f964abb9b016b76b0f8b3474df0f7d7c23b941381f"
    return match validate_api_key(api_key, expected_hash) {
        Ok(identity) => match extract_claim(identity, "department") {
            Ok(claim_view) => claim_view.value,
            Err(e) => e,
        },
        Err(e) => e,
    }
}
"#;
    expect_str(run_raw(src), "radiology");
}

#[test]
fn api_key_with_invalid_hash_is_rejected() {
    let src = r#"
fn main() -> str {
    return match validate_api_key("wrong-key", "325ededd6c3b9988f623c7f964abb9b016b76b0f8b3474df0f7d7c23b941381f") {
        Ok(_) => "should not succeed",
        Err(e) => e,
    }
}
"#;
    expect_str(run_raw(src), "invalid API key");
}

#[test]
fn refresh_token_exchange_succeeds_when_not_expired() {
    let src = r#"
fn main() -> str {
    let token: str = "eyJhbGciOiAiSFMyNTYiLCAia2lkIjogImtleTEifQ.eyJzdWIiOiAiYWxpY2UiLCAiaXNzIjogImh0dHBzOi8vZXhhbXBsZS5jb20iLCAiYXVkIjogIm15LWFwcCIsICJleHAiOiAyMDAwMDAwMDAwLCAiaWF0IjogMTcwMDAwMDAwMCwgInJvbGVzIjogWyJwaHlzaWNpYW4iXSwgImRlcGFydG1lbnQiOiAiY2FyZGlvbG9neSJ9.nrFdeqNDwXWLeGzud6X9Q4ITzCXULzZBBK8y51LGYXs"
    let jwks: str = "{\"keys\":[{\"kid\":\"key1\",\"kty\":\"oct\",\"k\":\"bXktc2VjcmV0LWtleQ\"}]}"
    return match oidc_validate_token(token, "https://example.com", "my-app", jwks) {
        Ok(identity) => {
            let refresh: RefreshTokenHandle = new_refresh_token(2000000000)
            return match exchange_refresh_token(identity, refresh, 1700000000) {
                Ok(refreshed) => match extract_claim(refreshed, "department") {
                    Ok(claim_view) => claim_view.value,
                    Err(e) => e,
                },
                Err(e) => e,
            }
        },
        Err(e) => e,
    }
}
"#;
    expect_str(run_raw(src), "cardiology");
}

#[test]
fn refresh_token_exchange_fails_when_expired() {
    let src = r#"
fn main() -> str {
    let token: str = "eyJhbGciOiAiSFMyNTYiLCAia2lkIjogImtleTEifQ.eyJzdWIiOiAiYWxpY2UiLCAiaXNzIjogImh0dHBzOi8vZXhhbXBsZS5jb20iLCAiYXVkIjogIm15LWFwcCIsICJleHAiOiAyMDAwMDAwMDAwLCAiaWF0IjogMTcwMDAwMDAwMCwgInJvbGVzIjogWyJwaHlzaWNpYW4iXSwgImRlcGFydG1lbnQiOiAiY2FyZGlvbG9neSJ9.nrFdeqNDwXWLeGzud6X9Q4ITzCXULzZBBK8y51LGYXs"
    let jwks: str = "{\"keys\":[{\"kid\":\"key1\",\"kty\":\"oct\",\"k\":\"bXktc2VjcmV0LWtleQ\"}]}"
    return match oidc_validate_token(token, "https://example.com", "my-app", jwks) {
        Ok(identity) => {
            let refresh: RefreshTokenHandle = new_refresh_token(1600000000)
            return match exchange_refresh_token(identity, refresh, 1700000000) {
                Ok(_) => "should not succeed",
                Err(e) => e,
            }
        },
        Err(e) => e,
    }
}
"#;
    expect_str(run_raw(src), "refresh token expired");
}

#[test]
fn application_session_cookie_is_generated() {
    let src = r#"
fn main() -> str {
    let token: str = "eyJhbGciOiAiSFMyNTYiLCAia2lkIjogImtleTEifQ.eyJzdWIiOiAiYWxpY2UiLCAiaXNzIjogImh0dHBzOi8vZXhhbXBsZS5jb20iLCAiYXVkIjogIm15LWFwcCIsICJleHAiOiAyMDAwMDAwMDAwLCAiaWF0IjogMTcwMDAwMDAwMCwgInJvbGVzIjogWyJwaHlzaWNpYW4iXSwgImRlcGFydG1lbnQiOiAiY2FyZGlvbG9neSJ9.nrFdeqNDwXWLeGzud6X9Q4ITzCXULzZBBK8y51LGYXs"
    let jwks: str = "{\"keys\":[{\"kid\":\"key1\",\"kty\":\"oct\",\"k\":\"bXktc2VjcmV0LWtleQ\"}]}"
    return match oidc_validate_token(token, "https://example.com", "my-app", jwks) {
        Ok(identity) => {
            let session: ApplicationSession = create_application_session(identity)
            let cookie: str = session_cookie(session)
            return cookie
        },
        Err(e) => e,
    }
}
"#;
    match run_raw(src) {
        Value::Str(s) => {
            assert!(s.starts_with("session=example.com_alice_"), "unexpected cookie prefix: {s}");
            assert!(s.contains("HttpOnly"), "cookie missing HttpOnly: {s}");
            assert!(s.contains("Secure"), "cookie missing Secure: {s}");
            assert!(s.contains("SameSite=Strict"), "cookie missing SameSite: {s}");
        }
        other => panic!("expected Str(cookie), got {other:?}"),
    }
}

#[test]
fn example_runs_to_completion() {
    match run(include_str!("../examples/row12_identity.nir")) {
        Ok(Value::Str(s)) => assert_eq!(&*s, "done"),
        other => panic!("expected Ok(Str(\"done\")), got {other:?}"),
    }
}
