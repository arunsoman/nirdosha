//! `docs/ROADMAP.md` A11 / `docs/API_TRUST_MODEL.md` §3: `oidc_validate_token` used
//! to accept only a symmetric (`kty: "oct"`) JWKS key, verified via
//! unconditional HMAC-SHA256, regardless of the JWT header's own `alg` —
//! no mainstream IdP (Auth0/Okta/Keycloak/Azure AD, all RS256/ES256 by
//! default) could be plugged in. This exercises the fix: real RSA
//! (`RS256`) and EC P-256 (`ES256`) signature verification against real
//! key material, plus the algorithm-confusion case the fix has to close
//! (a JWKS's asymmetric public key replayed as an HS256 HMAC secret).
//!
//! Signing happens directly in Rust (`ring`), not via `mock_issue_token`
//! — that builtin is deliberately HS256-only (its own doc comment), so
//! it can't produce the RS256/ES256 tokens this test needs.

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use nirdosha::interpreter::Value;
use nirdosha::run;
use ring::signature::KeyPair;

const ISSUER: &str = "https://example.com";
const AUDIENCE: &str = "my-app";

// A real, freshly generated 2048-bit RSA keypair (PKCS8 DER, base64
// standard-encoded), used only to sign/verify this test's own tokens —
// not a secret shared with anything else.
const RSA_PKCS8_B64: &str = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDLh+V9geBHm2qfpQQxRwSuAFa5eT28N7F6b9UznLJ5hUoKeYvJxR8okgdTRqPSWT8wSjZELO0K4746b8apGzT03jNZFoIdXlHDEMpGTqolrOEhnQMIRnnIKuu1uvCbtyqUR2ULwNhd62HGChjc5i3/rrQ9l38asdWKypc32IQ8kyrfW2Qct00qSr/LZPXCgQHaDEJn8C6zYo62cG+r5yw5jHORpn0hviecfzhtZ8/Mq48w7ZKhlTascuMwb4TL7BO0sNwVzFkU33l9cxJvQxv3SLcQZ2CRodJd1hEo1h2eXE6csrwcU0sTDHpY01cHkCeYc95xdY49vAEtHplEiS41AgMBAAECggEADpFatOSJETxdDIc1qRycXPig5yP37hMNwiXjXI8whm/VZbGJzAvZbUB32MHmHjtiwUnr325YcHPuR5AnkoQO2CCtYTOHZaMSIW8ADg85sM+nyjhK9sUrQ09bJrCg4dcMQP0T4kb6gE0a5vS4ZIXvMJf9xlN5IV0thoJ2Ag/JIJgGryVXeIRUPz/g4J9Zb2tDI/uMEhnJZCs1vOVKx1ivEF6R+MGADJScg2Q7S+q2XHPMfCMfmt3G3KgmUMwUNPIMO7eVORGkE/j+q0hpCb/2Wli+UF9XpWRicFPcRpoS+a4oV4FK/1jWg+BJs5U71/A3hAA+uDJ2kH3m4LYw2/120wKBgQDqhbVJ8lSxjmxRJVEMKb81zsAUxRj/OzYB56m0hXJMwdpFMSuoKMU1rvQ0CLHDxLT2VoQXuINFmH8YtToCYVFZaD10EEn2SygLRc18RYUhGKpYMCth7NhZ9DxEDu/oPAHDr+PqJbnzRJn7I+kI44wTf0f90QZ/HDxGRkv/vSIizwKBgQDeK5rO2fG0vsfXLVZj5IXhS/CgaEfh25gcx1WWqy8PDbt1WfgCXXQdV37F6mi8cYBtsC8CLIfYu3MyWT38iOEeffpmUkjwLN2ItQ4D/qDSjKkfQ1L14Xs164kWLbMmXMCmgOpxn1yLn1Lk3i6CW+7u8s85pBI9qHkBn8D34N5vuwKBgQCz+5l9/rRw79TBEdp5czCDowBW8EyW1GJGY/whxqhJsBxLLclLbL6szHAt3t5OWBBpXUxSyBA2wSoJVEwIt5cu/ojrIfUhR9ybih6BWPkqxTs2IwGoTZRctMvrj2se166i98H6WKm0wNlLm+ukHr3J2MzvRhOuSYUYiaqZwhUIkQKBgCKPn/Z7uvkGEKptmBnUC+ufV6BseHovIfugGrVkjd6GoVnBFC6yAQUrfkIllAN0mKj3lmh9KujRSDtV3KmzVWnb6R8Pv97068V/fN2sN7JpRUnivD4ZxIP/zwSPOWZHWYTNWysvwkLKFygZVBzdVk7/oLLzzg6fbauOAYPODGxPAoGAGrRnQERflEjXwZOglRDAWVuEc8PZxU1UcEAMuy6zkDoC73ShIjSIMeimZbOKghFcdlZaeJBul3/7gDHKQThp3KePygT66SUHVazcbuujisTOeFuqHyeFUDLsrd9OpBiY1f/qU00/6CYvyl/77YPu0PhJrvd4kVFxXiWojMsZaSs=";
const RSA_N_B64URL: &str = "y4flfYHgR5tqn6UEMUcErgBWuXk9vDexem_VM5yyeYVKCnmLycUfKJIHU0aj0lk_MEo2RCztCuO-Om_GqRs09N4zWRaCHV5RwxDKRk6qJazhIZ0DCEZ5yCrrtbrwm7cqlEdlC8DYXethxgoY3OYt_660PZd_GrHVisqXN9iEPJMq31tkHLdNKkq_y2T1woEB2gxCZ_Aus2KOtnBvq-csOYxzkaZ9Ib4nnH84bWfPzKuPMO2SoZU2rHLjMG-Ey-wTtLDcFcxZFN95fXMSb0Mb90i3EGdgkaHSXdYRKNYdnlxOnLK8HFNLEwx6WNNXB5AnmHPecXWOPbwBLR6ZRIkuNQ";
const RSA_E_B64URL: &str = "AQAB";

fn b64u(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Signs `header_json`.`payload_json` with an RSA key (`RS256` padding),
/// returning the full `h.p.s` compact JWT. `ring::signature::RsaKeyPair`
/// and `EcdsaKeyPair` don't share a common signing trait/signature in
/// this `ring` version, so RSA and EC each get their own helper.
fn sign_jwt_rsa(header_json: &str, payload_json: &str, key: &ring::signature::RsaKeyPair) -> String {
    let header_b64 = b64u(header_json.as_bytes());
    let payload_b64 = b64u(payload_json.as_bytes());
    let signed_input = format!("{header_b64}.{payload_b64}");
    let rng = ring::rand::SystemRandom::new();
    let mut signature = vec![0u8; key.public().modulus_len()];
    key.sign(&ring::signature::RSA_PKCS1_SHA256, &rng, signed_input.as_bytes(), &mut signature)
        .expect("RSA signing succeeds");
    format!("{signed_input}.{}", b64u(&signature))
}

/// Same as `sign_jwt_rsa`, for an EC P-256 key (`ES256`).
fn sign_jwt_ec(header_json: &str, payload_json: &str, key: &ring::signature::EcdsaKeyPair) -> String {
    let header_b64 = b64u(header_json.as_bytes());
    let payload_b64 = b64u(payload_json.as_bytes());
    let signed_input = format!("{header_b64}.{payload_b64}");
    let rng = ring::rand::SystemRandom::new();
    let signature = key.sign(&rng, signed_input.as_bytes()).expect("EC signing succeeds");
    format!("{signed_input}.{}", b64u(signature.as_ref()))
}

fn payload_json(sub: &str) -> String {
    format!(
        r#"{{"iss":"{ISSUER}","aud":"{AUDIENCE}","sub":"{sub}","iat":1700000000,"exp":1900000000}}"#
    )
}

fn escape_nir_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Runs `oidc_validate_token(token, ISSUER, AUDIENCE, jwks)` through the
/// real interpreter (parser/typeck/interpreter, not a Rust-level unit
/// test of `validate_oidc_token` directly) and returns `"ok"` or the
/// error string.
fn validate(token: &str, jwks: &str) -> String {
    let src = format!(
        r#"
struct Text {{
    value: str,
}}
fn main() -> Text {{
    let token: str = "{token}"
    let jwks: str = "{jwks}"
    return match oidc_validate_token(token, "{ISSUER}", "{AUDIENCE}", jwks) {{
        Ok(identity) => Text("ok"),
        Err(e) => Text(e),
    }}
}}
"#,
        token = escape_nir_str(token),
        jwks = escape_nir_str(jwks),
    );
    let result = run(&src).unwrap_or_else(|e| panic!("runtime error: {e:?}"));
    match result {
        Value::Struct(name, fields) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => s.to_string(),
            other => panic!("expected Text(Str(_)), got Text({other:?})"),
        },
        other => panic!("expected Text(_), got {other:?}"),
    }
}

fn rsa_signing_key() -> ring::signature::RsaKeyPair {
    let der = STANDARD.decode(RSA_PKCS8_B64).expect("valid base64");
    ring::signature::RsaKeyPair::from_pkcs8(&der).expect("valid PKCS8 RSA key")
}

fn ec_signing_key() -> (ring::signature::EcdsaKeyPair, Vec<u8>, Vec<u8>) {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::EcdsaKeyPair::generate_pkcs8(&ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
        .expect("key generation succeeds");
    let key_pair =
        ring::signature::EcdsaKeyPair::from_pkcs8(&ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .expect("valid PKCS8 EC key");
    // Uncompressed SEC1 point: 0x04 || x (32 bytes) || y (32 bytes).
    let public = key_pair.public_key().as_ref().to_vec();
    assert_eq!(public.len(), 65, "uncompressed P-256 point is 65 bytes");
    let x = public[1..33].to_vec();
    let y = public[33..65].to_vec();
    (key_pair, x, y)
}

#[test]
fn rs256_token_against_a_real_rsa_jwks_verifies() {
    let header = r#"{"alg":"RS256","kid":"rsa-key-1"}"#;
    let payload = payload_json("alice");
    let key = rsa_signing_key();
    let token = sign_jwt_rsa(header, &payload, &key);
    let jwks = format!(
        r#"{{"keys":[{{"kid":"rsa-key-1","kty":"RSA","n":"{RSA_N_B64URL}","e":"{RSA_E_B64URL}"}}]}}"#
    );
    assert_eq!(validate(&token, &jwks), "ok");
}

#[test]
fn rs256_token_with_tampered_signature_is_rejected() {
    let header = r#"{"alg":"RS256","kid":"rsa-key-1"}"#;
    let payload = payload_json("alice");
    let key = rsa_signing_key();
    let token = sign_jwt_rsa(header, &payload, &key);
    let mut parts: Vec<&str> = token.split('.').collect();
    let mut sig = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
    sig[0] ^= 0xFF;
    let tampered_sig = b64u(&sig);
    parts[2] = &tampered_sig;
    let tampered = parts.join(".");
    let jwks = format!(
        r#"{{"keys":[{{"kid":"rsa-key-1","kty":"RSA","n":"{RSA_N_B64URL}","e":"{RSA_E_B64URL}"}}]}}"#
    );
    assert_eq!(validate(&tampered, &jwks), "invalid JWT signature");
}

#[test]
fn es256_token_against_a_real_ec_p256_jwks_verifies() {
    let (key, x, y) = ec_signing_key();
    let header = r#"{"alg":"ES256","kid":"ec-key-1"}"#;
    let payload = payload_json("bob");
    let token = sign_jwt_ec(header, &payload, &key);
    let jwks = format!(
        r#"{{"keys":[{{"kid":"ec-key-1","kty":"EC","crv":"P-256","x":"{}","y":"{}"}}]}}"#,
        b64u(&x),
        b64u(&y)
    );
    assert_eq!(validate(&token, &jwks), "ok");
}

#[test]
fn es256_token_with_tampered_signature_is_rejected() {
    let (key, x, y) = ec_signing_key();
    let header = r#"{"alg":"ES256","kid":"ec-key-1"}"#;
    let payload = payload_json("bob");
    let token = sign_jwt_ec(header, &payload, &key);
    let mut parts: Vec<&str> = token.split('.').collect();
    let mut sig = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
    sig[0] ^= 0xFF;
    let tampered_sig = b64u(&sig);
    parts[2] = &tampered_sig;
    let tampered = parts.join(".");
    let jwks = format!(
        r#"{{"keys":[{{"kid":"ec-key-1","kty":"EC","crv":"P-256","x":"{}","y":"{}"}}]}}"#,
        b64u(&x),
        b64u(&y)
    );
    assert_eq!(validate(&tampered, &jwks), "invalid JWT signature");
}

/// The algorithm-confusion attack this fix has to close: an attacker who
/// knows the IdP's real RSA *public* key (JWKS documents are public by
/// design) claims `alg: "HS256"` and uses those public `n`/`e` bytes as
/// an HMAC secret, hoping the server naively HMACs against whatever `k`-
/// shaped bytes it finds. Before this fix there was no `kty` check at
/// all — `jwks_key` read `k` unconditionally. Now `kty: "RSA"` never
/// produces a `Symmetric` key, so the `HS256`/`Rsa` combination has no
/// matching verification arm and is rejected outright.
#[test]
fn hs256_header_against_an_rsa_jwks_key_is_rejected_not_hmac_verified() {
    let header = r#"{"alg":"HS256","kid":"rsa-key-1"}"#;
    let payload = payload_json("mallory");
    let header_b64 = b64u(header.as_bytes());
    let payload_b64 = b64u(payload.as_bytes());
    // Forge an HMAC using the RSA key's public `n` bytes as the secret —
    // exactly what an attacker who only has the (public) JWKS document
    // could do.
    let n_bytes = URL_SAFE_NO_PAD.decode(RSA_N_B64URL).unwrap();
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&n_bytes).unwrap();
    use hmac::Mac;
    mac.update(format!("{header_b64}.{payload_b64}").as_bytes());
    let forged_sig = b64u(&mac.finalize().into_bytes());
    let forged_token = format!("{header_b64}.{payload_b64}.{forged_sig}");
    let jwks = format!(
        r#"{{"keys":[{{"kid":"rsa-key-1","kty":"RSA","n":"{RSA_N_B64URL}","e":"{RSA_E_B64URL}"}}]}}"#
    );
    let result = validate(&forged_token, &jwks);
    assert_ne!(result, "ok", "algorithm-confusion forgery must not verify");
    assert!(result.contains("alg") || result.contains("signature"), "unexpected rejection reason: {result}");
}
