//! JWT verification for inbound WebSocket clients.
//!
//! Deliberately does **not** reuse `compiler/src/interpreter.rs`'s own
//! `validate_oidc_token` — that function is `pub(crate)`, private to the
//! interpreter, and pulling in the whole `nirdosha` crate as a real
//! (non-dev) dependency just to reach it would drag z3/postgres/rusqlite
//! into a process meant to stay a small, easily-deployed sidecar (see
//! `Cargo.toml`'s own doc comment on that choice). Uses the standard
//! `jsonwebtoken` crate instead of hand-rolling a second JWT verifier —
//! but keeps the one hardening detail from `interpreter.rs` that actually
//! matters, ported over deliberately: `JwksKeyMaterial`'s "a key's `kty`
//! locks which `alg` it may verify under" pairing, which closes the
//! classic algorithm-confusion attack (an RSA public key replayed as an
//! HMAC secret). `jsonwebtoken::Validation` only restricts which
//! algorithms a *decode call* will accept — it does not itself derive an
//! algorithm from a JWK's `kty` — so building one single-algorithm
//! `DecodingKey` per JWK, keyed by `kid`, and never letting the token's
//! own header pick a different one, is deliberately this module's job.

use std::collections::HashMap;
use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct RawJwks {
    keys: Vec<RawJwk>,
}

#[derive(Debug, Deserialize)]
struct RawJwk {
    kid: String,
    kty: String,
    // RSA (`kty: "RSA"`)
    n: Option<String>,
    e: Option<String>,
    // EC (`kty: "EC"`, `crv: "P-256"` only — ES256)
    crv: Option<String>,
    x: Option<String>,
    y: Option<String>,
    // oct (`kty: "oct"`, symmetric — HS256 only). Real IdPs never publish
    // one of these in a public JWKS (it would leak the shared secret) —
    // this exists so a locally-mocked JWKS (`mock_issue_token`, this
    // crate's own integration tests, `tests/serve.rs`'s own `JWKS` const)
    // verifies exactly the same way a real RSA/EC IdP's would.
    k: Option<String>,
}

struct VerifiableKey {
    decoding_key: DecodingKey,
    algorithm: Algorithm,
}

/// A parsed, ready-to-verify-against JWKS — loaded once at startup from
/// `--jwks-file`, the same "static file, no live rotation" posture
/// `nirdosha serve`'s own `--jwks-file`/`--issuer`/`--audience` trio
/// already takes (`main.rs`'s `cmd_serve` doc comment); disclosed, not
/// hidden, the same way that limitation already is there.
pub struct KeySet {
    keys: HashMap<String, VerifiableKey>,
}

#[derive(Debug)]
pub enum JwksError {
    Parse(String),
    UnsupportedKty(String),
    UnsupportedCurve(String),
    MissingField { kid: String, field: &'static str },
    InvalidKeyMaterial { kid: String, reason: String },
    InvalidBase64 { kid: String, field: &'static str },
}

impl fmt::Display for JwksError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JwksError::Parse(e) => write!(f, "malformed JWKS: {e}"),
            JwksError::UnsupportedKty(kty) => write!(f, "unsupported JWK `kty`: `{kty}` (only RSA, EC/P-256, and oct are supported)"),
            JwksError::UnsupportedCurve(crv) => write!(f, "unsupported EC curve: `{crv}` (only P-256/ES256 is supported)"),
            JwksError::MissingField { kid, field } => write!(f, "JWK `{kid}` is missing required field `{field}`"),
            JwksError::InvalidKeyMaterial { kid, reason } => write!(f, "JWK `{kid}` has invalid key material: {reason}"),
            JwksError::InvalidBase64 { kid, field } => write!(f, "JWK `{kid}` field `{field}` is not valid base64url"),
        }
    }
}

impl std::error::Error for JwksError {}

impl KeySet {
    pub fn from_json(jwks_json: &str) -> Result<Self, JwksError> {
        let raw: RawJwks = serde_json::from_str(jwks_json).map_err(|e| JwksError::Parse(e.to_string()))?;
        let mut keys = HashMap::with_capacity(raw.keys.len());
        for jwk in raw.keys {
            let verifiable = match jwk.kty.as_str() {
                "RSA" => {
                    let n = jwk.n.as_deref().ok_or_else(|| JwksError::MissingField { kid: jwk.kid.clone(), field: "n" })?;
                    let e = jwk.e.as_deref().ok_or_else(|| JwksError::MissingField { kid: jwk.kid.clone(), field: "e" })?;
                    let decoding_key = DecodingKey::from_rsa_components(n, e)
                        .map_err(|err| JwksError::InvalidKeyMaterial { kid: jwk.kid.clone(), reason: err.to_string() })?;
                    VerifiableKey { decoding_key, algorithm: Algorithm::RS256 }
                }
                "EC" => {
                    let crv = jwk.crv.clone().unwrap_or_default();
                    if crv != "P-256" {
                        return Err(JwksError::UnsupportedCurve(crv));
                    }
                    let x = jwk.x.as_deref().ok_or_else(|| JwksError::MissingField { kid: jwk.kid.clone(), field: "x" })?;
                    let y = jwk.y.as_deref().ok_or_else(|| JwksError::MissingField { kid: jwk.kid.clone(), field: "y" })?;
                    let decoding_key = DecodingKey::from_ec_components(x, y)
                        .map_err(|err| JwksError::InvalidKeyMaterial { kid: jwk.kid.clone(), reason: err.to_string() })?;
                    VerifiableKey { decoding_key, algorithm: Algorithm::ES256 }
                }
                "oct" => {
                    let k = jwk.k.as_deref().ok_or_else(|| JwksError::MissingField { kid: jwk.kid.clone(), field: "k" })?;
                    // A JWK's `k` is base64url per RFC 7518 §6.4.1 —
                    // `DecodingKey::from_secret` wants raw bytes, unlike
                    // `from_rsa_components`/`from_ec_components` (which
                    // take the base64url strings directly), so this one
                    // arm has to decode first — same decode
                    // `interpreter.rs::JwksKeyMaterial`'s own `oct` arm
                    // and `mock_issue_token`'s minting side both do.
                    let raw_secret = URL_SAFE_NO_PAD
                        .decode(k)
                        .map_err(|_| JwksError::InvalidBase64 { kid: jwk.kid.clone(), field: "k" })?;
                    VerifiableKey { decoding_key: DecodingKey::from_secret(&raw_secret), algorithm: Algorithm::HS256 }
                }
                other => return Err(JwksError::UnsupportedKty(other.to_string())),
            };
            keys.insert(jwk.kid.clone(), verifiable);
        }
        Ok(KeySet { keys })
    }
}

#[derive(Debug, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: i64,
}

#[derive(Debug)]
pub enum VerifyError {
    Malformed(String),
    UnknownKid(String),
    Verification(String),
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VerifyError::Malformed(e) => write!(f, "malformed token: {e}"),
            VerifyError::UnknownKid(kid) => write!(f, "token references unknown key id `{kid}`"),
            VerifyError::Verification(e) => write!(f, "token verification failed: {e}"),
        }
    }
}

impl std::error::Error for VerifyError {}

/// Verifies `token`'s signature against `keys`, plus `iss`/`aud`/`exp` —
/// checked against a **real wall clock**, deliberately, unlike
/// `interpreter.rs::validate_oidc_token` (which stays a pure function of
/// its inputs for `.nir`-source determinism, `LANGUAGE.md` §9, and
/// explicitly leaves the real-clock `exp` check to "the actual network
/// boundary" — see that function's own doc comment). This gateway *is*
/// that boundary for its own WebSocket clients, the same role
/// `serve.rs::dispatch`'s bearer-token path already plays for ordinary
/// API calls — so checking `exp` for real, right here, is the correct
/// split, not a shortcut. `jsonwebtoken::Validation`'s default
/// (`validate_exp: true`) already does this.
pub fn verify(token: &str, keys: &KeySet, issuer: &str, audience: &str) -> Result<Claims, VerifyError> {
    let header = jsonwebtoken::decode_header(token).map_err(|e| VerifyError::Malformed(e.to_string()))?;
    let kid = header.kid.ok_or_else(|| VerifyError::Malformed("token header has no `kid`".to_string()))?;
    let key = keys.keys.get(&kid).ok_or_else(|| VerifyError::UnknownKid(kid.clone()))?;

    // Locked to exactly the one algorithm this specific `kid`'s key
    // material is valid for (never the token's own `alg` claim) — this is
    // the actual algorithm-confusion guard; `Validation::new` here is not
    // an arbitrary choice, it's `key.algorithm`, decided entirely by
    // `KeySet::from_json`'s `kty` match above.
    let mut validation = Validation::new(key.algorithm);
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[audience]);
    let data = jsonwebtoken::decode::<Claims>(token, &key.decoding_key, &validation).map_err(|e| VerifyError::Verification(e.to_string()))?;
    Ok(data.claims)
}
