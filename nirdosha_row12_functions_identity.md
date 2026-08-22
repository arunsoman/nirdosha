Nirdosha Amendment: Row 12 — Corrected 
First-Class Functions & User Identity as a Relying Party 
Document: Corrected Amendment to goal.md and Row 11 Amendment
Date: 21 Aug 2026
Status: High-level design — addressing the relying-party critique 
 Executive Summary 
The initial Row 12 draft incorrectly positioned the Nirdosha runtime as an identity provider (minting signed tokens, holding signing keys, re-attesting identity
data). This document corrects that: Nirdosha is strictly a relying party / consumer of third-party identity providers. The runtime validates external tokens; it
does not issue them. 
What Nirdosha does NOT do: - Issue JWTs, SAML assertions, or any identity tokens - Store signing keys, passwords, or user credentials - Run an OIDC server,
OAuth authorization server, or IdP - Mint runtime-signed capability tokens 
What Nirdosha DOES do: - Validate tokens issued by external IdPs (Azure AD, Okta, LDAP, etc.) - Make the validation result unforgeable in the type system -
Enforce authorization via types derived from verified claims - Provide protocol-agnostic abstractions for identity consumption 
 1. The Core Correction: Validate, Don’t Mint 
1.1 What Was Wrong 
The initial draft had: 
-- WRONG: Runtime signs its own token
record User: Linear is
subject: String,
attestation: Bytes, -- Runtime signature (makes Nirdosha an IdP)
...
end;
 This creates a second trust anchor (the runtime’s signing key) that doesn’t exist in real relying-party architectures. A consumer of a third-party IdP validates the
IdP’s signature and then uses the verified claims. It does not re-sign the data. 
1.2 The Correct Model 
┌─────────────────────────────────────────────────────────────────┐
│ REAL-WORLD ARCHITECTURE │
├─────────────────────────────────────────────────────────────────┤
│ │
│ ┌─────────────┐ JWT/SAML ┌─────────────────────┐ │
│ │ Azure AD │ ─────────────────> │ Nirdosha Runtime │ │
│ │ (IdP) │ (signed by IdP) │ (Relying Party) │ │
│ └─────────────┘ │ │ │
│ │ 1. Validate signature│ │
│ ┌─────────────┐ JWT/SAML │ against JWKS │ │
│ │ Okta │ ─────────────────> │ 2. Verify claims │ │
│ │ (IdP) │ (signed by IdP) │ (issuer, aud, │ │
│ └─────────────┘ │ exp, etc.) │ │
│ │ 3. Extract claims │ │
│ ┌─────────────┐ LDAP bind │ 4. Produce typed │ │
│ │ LDAP │ ─────────────────> │ VerifiedIdentity│ │
│ │ (IdP) │ │ (NO re-signing) │ │
│ └─────────────┘ └─────────────────────┘ │
│ │
│ Nirdosha NEVER holds signing keys. It only holds: │
│ - Verification keys (JWKS, SAML certs, LDAP CA) │
│ - Client secrets (for token exchange, stored in secure enclave)│
│ - Validation policies (trusted issuers, allowed audiences) │
│ │
└─────────────────────────────────────────────────────────────────┘

2. Protocol-Agnostic Identity Types 
2.1 VerifiedIdentity: The Common Abstraction 
All protocol adapters (OIDC, SAML, LDAP, API key) produce the same type. The language does not hardcode OIDC fields. 
-- ============================================================
-- 2.1.1 VerifiedIdentity: produced ONLY by protocol adapters
-- ============================================================
 -- Module-private constructor. Only protocol adapters can create this.
-- User code CANNOT forge a VerifiedIdentity.
record VerifiedIdentity: Free is
-- Who this identity represents (from IdP)
subject: String,
  -- Who issued it (URL or identifier)
issuer: String,
  -- Who this token was intended for (client_id, audience)
audience: String,
  -- When it expires
expiresAt: DateTime,
  -- When it was issued
issuedAt: DateTime,
  -- Verified claims from the IdP (already cryptographically validated)
claims: Map[String, Claim],
  -- Raw token reference (for audit/logging, not for re-validation)
tokenReference: TokenReference
end;
 -- TokenReference is an opaque handle to the validated token in runtime storage.
-- It is NOT a bearer token and cannot be used to authenticate elsewhere.
record TokenReference: Linear is
handle: Int64,
hash: String -- SHA-256 of the raw token, for audit correlation
end;
 -- Claim types (same as before, but now purely derived from IdP data)
union Claim: Free is
case StringClaim is value: String;
case IntClaim is value: Int64;
case BoolClaim is value: Bool;
case ListClaim is values: List[Claim];
case NullClaim;
end;
 Key differences from the wrong draft: - No attestation field. Trust comes from the fact that only protocol adapters can construct VerifiedIdentity.
The compiler enforces this via module-private constructors. - No runtime signature. The cryptographic validation happened in the adapter. The type system’s
unforgeability replaces the need for a runtime signature. - TokenReference is Linear. It represents a slot in the runtime’s validated-token cache. It must be
dropped to release the cache slot. 
2.2 Borrowing VerifiedIdentity 
The initial draft made User strictly Linear, which is awkward for a relying party that needs to reference the same identity multiple times during a request
(logging, audit, multiple auth checks). The corrected design makes VerifiedIdentity Free (copyable, shareable) but with a Linear TokenReference that
manages the runtime cache lifecycle. 
-- VerifiedIdentity is Free (can be borrowed, passed around freely)
-- But it contains a Linear TokenReference that must be managed
 function handleRequest(identity: &VerifiedIdentity): Response is
-- Borrow identity for read-only access
let subject: String := identity.subject;
let roleResult: Result[RoleView[Role.Physician], AuthError> := 
checkRole(identity, Role.Physician); -- borrows identity
let deptResult: Result<ClaimView["department", String], AuthError> :=extractClaim(identity, "department", TypeTag[String]); -- also borrows
-- identity is still valid here; it was only borrowed
end;
 -- When the request ends, the TokenReference is dropped, releasing the cache slot
 Why Free + Linear TokenReference? - VerifiedIdentity itself is just data (claims, subject, etc.). Copying it is safe. - TokenReference represents a runtime
resource (cache slot, memory buffer). It must be consumed exactly once. - This separation lets the same identity be referenced for logging, audit, and multiple
auth checks without threading linear ownership through every call. 
 3. Protocol Adapters 
3.1 The Adapter Pattern 
Each protocol has an adapter that validates raw tokens and produces VerifiedIdentity. The language types are protocol-agnostic; only the adapters know
about JWTs, SAML, LDAP, etc. 
-- ============================================================
-- 3.1.1 Protocol Adapter Interface
-- ============================================================
 -- All adapters implement this interface
interface IdentityAdapter: Linear is
function validateToken(token: String): Result<VerifiedIdentity, AuthError>
effect(authorize, network);
function validateTokenWithPolicy(
token: String,
policy: ValidationPolicy
): Result<VerifiedIdentity, AuthError>
effect(authorize, network);
function introspectToken(token: String): Result<TokenIntrospectionResult, AuthError>
effect(authorize, network);
end;
 -- ============================================================
-- 3.1.2 OIDC Adapter
-- ============================================================
 record OidcAdapter: Linear is
config: OidcConfig,
jwksCache: JwksCache,
httpClient: HttpClient
end;
 record OidcConfig: Free is
issuerUrl: String,
clientId: String,
-- clientSecret is NOT here. Stored in runtime secure enclave.
discoveryUrl: String, -- e.g., https://issuer/.well-known/openid-configuration
scopes: List[String],
usePkce: Bool := true,
useNonce: Bool := true
end;
 -- OIDC Discovery: fetch endpoints dynamically
function OidcAdapter.discoverEndpoints(
adapter: OidcAdapter
): Result<OidcEndpoints, HttpError>
effect(network)
is
let discoveryUrl: String := adapter.config.discoveryUrl;
let response: Result<Response, HttpError> := 
adapter.httpClient.get(discoveryUrl);
case response of
when Ok(resp: Response) =>
let endpoints: Result<OidcEndpoints, JsonError> := resp.bodyJson();
return endpoints;
when Err(e: HttpError) =>
return Result[OidcEndpoints, HttpError].Err(e);
end case;end;
 -- Validate a JWT ID token or access token
function OidcAdapter.validateToken(
adapter: OidcAdapter,
token: String
): Result<VerifiedIdentity, AuthError>
effect(authorize, network)
is
-- Step 1: Parse JWT header to find key ID (kid)
let header: Result<JwtHeader, AuthError> := parseJwtHeader(token);
case header of
when Ok(h: JwtHeader) =>
-- Step 2: Fetch signing key from JWKS cache (or refresh if missing)
let keyResult: Result<PublicKey, AuthError> := 
adapter.jwksCache.getKey(h.kid);
case keyResult of
when Ok(key: PublicKey) =>
-- Step 3: Validate signature
let sigValid: Bool := crypto.verifyJwtSignature(token, key);
if not sigValid then
return Result[VerifiedIdentity, AuthError].Err(AuthError.InvalidSignature());
end if;
  -- Step 4: Parse payload and validate claims
let payload: Result<JwtPayload, AuthError> := parseJwtPayload(token);
case payload of
when Ok(p: JwtPayload) =>
-- Step 5: Validate issuer matches configured issuer
if p.iss != adapter.config.issuerUrl then
return Result[...].Err(AuthError.UntrustedIssuer(
expected := adapter.config.issuerUrl,
actual := p.iss
));
end if;
  -- Step 6: Validate audience matches this application
if p.aud != adapter.config.clientId then
return Result[...].Err(AuthError.WrongAudience(
expected := adapter.config.clientId,
actual := p.aud
));
end if;
  -- Step 7: Validate expiration and clock skew
let now: DateTime := DateTime.now();
let skew: Duration := ValidationPolicy.default().clockSkew;
if p.exp < now.subtract(skew) then
return Result[...].Err(AuthError.TokenExpired(p.sub));
end if;
if p.nbf != None and p.nbf.unwrap() > now.add(skew) then
return Result[...].Err(AuthError.TokenNotYetValid());
end if;
  -- Step 8: Validate nonce (if PKCE/nonce was used)
-- (nonce stored in session cache, checked here)
  -- Step 9: Construct VerifiedIdentity
let identity: VerifiedIdentity := VerifiedIdentity(
subject := p.sub,
issuer := p.iss,
audience := p.aud,
expiresAt := p.exp,
issuedAt := p.iat,
claims := jwtClaimsToMap(p.customClaims),
tokenReference := TokenReference(
handle := adapter.runtime.cacheToken(token),
hash := crypto.sha256(token)
)
);
return Result[VerifiedIdentity, AuthError].Ok(identity);  when Err(e: AuthError) =>
return Result[VerifiedIdentity, AuthError].Err(e);
end case;
when Err(e: AuthError) =>
return Result[VerifiedIdentity, AuthError].Err(e);
end case;
when Err(e: AuthError) =>
return Result[VerifiedIdentity, AuthError].Err(e);
end case;
end;

3.2 SAML Adapter 
record SamlAdapter: Linear is
config: SamlConfig,
httpClient: HttpClient
end;
 record SamlConfig: Free is
idpMetadataUrl: String,
spEntityId: String,
assertionConsumerServiceUrl: String,
wantAssertionsSigned: Bool := true,
wantMessagesSigned: Bool := false
end;
 function SamlAdapter.validateAssertion(
adapter: SamlAdapter,
samlResponse: String
): Result<VerifiedIdentity, AuthError>
effect(authorize, network)
is
-- Step 1: Parse SAML Response
-- Step 2: Validate XML signature against IdP metadata
-- Step 3: Validate Assertion conditions (AudienceRestriction, NotOnOrAfter)
-- Step 4: Extract NameID (subject) and AttributeStatement (claims)
-- Step 5: Construct VerifiedIdentity
end;

3.3 LDAP Adapter 
record LdapAdapter: Linear is
config: LdapConfig,
connection: LdapConnection
end;
 record LdapConfig: Free is
serverUrl: String,
bindDn: String,
searchBase: String,
userFilter: String, -- e.g., "(uid={username})"
groupFilter: String -- e.g., "(member={dn})"
end;
 function LdapAdapter.authenticate(
adapter: LdapAdapter,
username: String,
password: String
): Result<VerifiedIdentity, AuthError>
effect(authorize, network)
is
-- Step 1: Bind to LDAP server with service account
-- Step 2: Search for user by username
-- Step 3: Attempt user bind with provided password
-- Step 4: If successful, fetch user attributes and group memberships
-- Step 5: Construct VerifiedIdentity with claims from LDAP attributes
end;
3.4 API Key / Service Account Adapter 
record ApiKeyAdapter: Linear is
keyStore: ApiKeyStore,
rateLimiter: RateLimiter
end;
 function ApiKeyAdapter.validateKey(
adapter: ApiKeyAdapter,
apiKey: String,
clientCert: Option<Certificate>
): Result<VerifiedIdentity, AuthError>
effect(authorize)
is
-- Step 1: Lookup key in secure store
-- Step 2: Validate key hash (constant-time comparison)
-- Step 3: If mTLS is required, validate client certificate
-- Step 4: Check rate limits
-- Step 5: Construct VerifiedIdentity with service account claims
end;

3.5 Token Introspection Adapter (for opaque OAuth2 tokens) 
function OidcAdapter.introspectToken(
adapter: OidcAdapter,
token: String
): Result<TokenIntrospectionResult, AuthError>
effect(authorize, network)
is
-- Step 1: Call POST /introspect endpoint with client_id + client_secret
-- Step 2: Parse introspection response
-- Step 3: If active=true, construct VerifiedIdentity from response claims
-- Step 4: If active=false, return TokenExpired or TokenRevoked
end;
 record TokenIntrospectionResult: Free is
active: Bool,
identity: Option<VerifiedIdentity>,
expiresAt: Option<DateTime>,
scope: Option<String>
end;

 4. Authorization: Derivative, Not Minted 
4.1 RoleView: A Read-Only Projection 
Authorization checks return views derived from VerifiedIdentity claims, not runtime-signed tokens. The type system guarantees the view was produced
by a real check; the cryptographic trust comes from the original IdP validation. 
-- ============================================================
-- 4.1.1 RoleView: proof that a role check was performed
-- ============================================================
 -- Module-private constructor. Only checkRole() can create this.
-- It is a ZERO-SIZE TYPE at runtime (just a compile-time proof).
record RoleView[role: Role]: Free is
-- Empty at runtime. The type IS the proof.
-- The compiler tracks that this value was produced by checkRole().
end;
 -- The checkRole function validates claims against the role definition
-- and returns a view if successful.
function checkRole(
identity: &VerifiedIdentity,
role: Role
): Result<RoleView[role], AuthError>
effect(authorize)
is-- Look up role definition (e.g., "Physician requires claim 'roles' contains 'physician'")
let definition: RoleDefinition := roleDefinitions.get(role);
  -- Check if identity's claims satisfy the definition
let satisfied: Bool := evaluateRoleDefinition(identity.claims, definition);
if satisfied then
return Result[RoleView[role], AuthError].Ok(RoleView[role]());
else
return Result[RoleView[role], AuthError].Err(AuthError.InsufficientRole(
required := roleToString(role)
));
end if;
end;
 -- ============================================================
-- 4.1.2 ClaimView: proof that a claim was extracted and validated
-- ============================================================
 -- Module-private constructor. Only extractClaim() can create this.
record ClaimView[claimName: String, T: Free]: Free is
value: T
-- The type encodes: "this value came from claim 'claimName' of type T"
end;
 function extractClaim[T: Free](
identity: &VerifiedIdentity,
claimName: String,
expectedType: TypeTag[T]
): Result<ClaimView[claimName, T], AuthError>
effect(authorize)
is
let claim: Option<Claim> := identity.claims.get(claimName);
case claim of
when Some(c: Claim) =>
let typed: Result<T, AuthError> := claimAsType(c, expectedType);
case typed of
when Ok(value: T) =>
return Result[ClaimView[claimName, T], AuthError].Ok(
ClaimView[claimName, T](value := value)
);
when Err(_) =>
return Result[...].Err(AuthError.ClaimTypeMismatch(
claim := claimName,
expected := typeTagToString(expectedType)
));
end case;
when None =>
return Result[ClaimView[claimName, T], AuthError].Err(
AuthError.MissingClaim(claimName)
);
end case;
end;
 Key differences from the wrong draft: - No attestation field. RoleView and ClaimView are compile-time proofs, not cryptographic tokens. - No runtime
signature. The trust chain is: IdP signs token → adapter validates signature → type system prevents forgery. - Zero runtime cost. RoleView is a zero-size type
(ZST). It exists only in the type system; it compiles away completely. 
4.2 ReBAC: Resource-Based Access 
-- PatientSelfView: proof that the identity matches the patient
record PatientSelfView: Free is
patientId: PatientId
-- Zero-size proof that identity.subject == patient record owner
end;
 function authorizePatientSelfAccess(
identity: &VerifiedIdentity,
targetPatientId: PatientId,
dbCap: &DatabaseCapability
): Result<PatientSelfView, MediCloudError>
effect(db, authorize)is
-- Look up the patient record to find the linked user subject
let patientResult: Result<Patient, MediCloudError> := fetchPatient(dbCap, targetPatientId);
case patientResult of
when Ok(patient: Patient) =>
-- Check if identity's subject matches the patient's linked account
let linkedSubject: Result<String, DbError> := 
query MediCloudDB {
SELECT user_subject FROM patient_accounts 
WHERE patient_id = :targetPatientId
}.rows.first();
case linkedSubject of
when Ok(subject: String) =>
if identity.subject == subject then
return Result[PatientSelfView, MediCloudError].Ok(
PatientSelfView(patientId := targetPatientId)
);
else
return Result[...].Err(MediCloudError.Forbidden(
"patient_record_" + patientIdToString(targetPatientId),
"self_access_only"
));
end if;
when Err(_) =>
return Result[...].Err(MediCloudError.PatientNotFound(targetPatientId));
end case;
when Err(e: MediCloudError) =>
return Result[...].Err(e);
end case;
end;

 5. Validation Policy 
-- ============================================================
-- 5.1 ValidationPolicy: First-Class Config
-- ============================================================
 record ValidationPolicy: Free is
-- Who we trust
trustedIssuers: List[String], -- e.g., ["https://login.microsoftonline.com/v2.0"]
allowedAudiences: List[String>, -- e.g., ["medicloud-api-client-id"]
  -- What we require
requiredClaims: List[String>, -- e.g., ["sub", "iss", "aud", "exp"]
forbiddenClaims: List[String>, -- Claims that must NOT be present
  -- Time tolerances
clockSkew: Duration := Duration.fromSeconds(60),
maxTokenAge: Option<Duration> := None, -- Reject tokens older than this
  -- Security policies
requireNonce: Bool := true,
requirePkce: Bool := true,
requireMtls: Bool := false,
  -- Token lifecycle
enableRevocationCheck: Bool := false, -- Check revocation lists / introspection
revocationCacheDuration: Duration := Duration.fromMinutes(5)
end;
 -- The policy is part of the source and content-addressed (Row 10)
-- Changing the policy changes the attested source hash

 6. Federation: Route by Issuer, Don’t Brute-Force-- ============================================================
-- 6.1 Safe Federation: Route by Claim, Not Trial
-- ============================================================
 -- WRONG (from initial draft): try every provider until one works
-- This is vulnerable to token substitution attacks
 -- CORRECT: inspect the token to determine the correct provider
function routeTokenToAdapter(
federation: IdentityFederation,
token: String
): Result<IdentityAdapter, AuthError>
effect(authorize)
is
-- Step 1: Parse token header (unsecured) to find issuer hint
let issuerHint: Result<String, AuthError> := extractIssuerFromToken(token);
case issuerHint of
when Ok(issuer: String) =>
-- Step 2: Look up adapter by issuer URL
let adapter: Option<IdentityAdapter> := federation.adapters.get(issuer);
case adapter of
when Some(a: IdentityAdapter) =>
return Result[IdentityAdapter, AuthError].Ok(a);
when None =>
return Result[...].Err(AuthError.UntrustedIssuer(
expected := "known configured issuer",
actual := issuer
));
end case;
when Err(_) =>
-- For opaque tokens (no parseable issuer), use configured default
-- or return error if no default is configured
return Result[...].Err(AuthError.UnroutableToken());
end case;
end;
 -- JWT-specific issuer extraction (no signature validation, just header parsing)
function extractIssuerFromToken(token: String): Result<String, AuthError> is
let parts: List[String> := token.split(".");
if parts.length != 3 then
return Result[String, AuthError].Err(AuthError.MalformedToken());
end if;
let payload: Result<Bytes, Base64Error> := base64UrlDecode(parts[1]);
case payload of
when Ok(data: Bytes) =>
let json: Result<JsonValue, JsonError> := json.parse(data);
case json of
when Ok(j: JsonValue) =>
let iss: Option<String> := j.getString("iss");
case iss of
when Some(issuer: String) =>
return Result[String, AuthError].Ok(issuer);
when None =>
return Result[...].Err(AuthError.MissingIssuer());
end case;
when Err(_) =>
return Result[...].Err(AuthError.MalformedToken());
end case;
when Err(_) =>
return Result[...].Err(AuthError.MalformedToken());
end case;
end;

 7. Token Lifecycle Concerns 
7.1 Refresh Tokens 
-- The runtime manages refresh tokens separately from access tokensrecord RefreshTokenHandle: Linear is
handle: Int64,
expiresAt: DateTime
end;
 function exchangeRefreshToken(
adapter: OidcAdapter,
refreshToken: RefreshTokenHandle
): Result<Pair<VerifiedIdentity, RefreshTokenHandle>, AuthError>
effect(authorize, network)
is
-- Call POST /token with grant_type=refresh_token
-- Return new access token (as VerifiedIdentity) + new refresh token handle
end;

7.2 Revocation 
function checkRevocation(
adapter: OidcAdapter,
identity: &VerifiedIdentity
): Result<Bool, AuthError>
effect(authorize, network)
is
-- Call POST /revoke or check CRL/OCSP
-- Return true if token is still valid, false if revoked
end;
 -- Usage: middleware checks revocation on sensitive endpoints
function sensitiveHandler(identity: &VerifiedIdentity): Response is
let revoked: Result<Bool, AuthError> := checkRevocation(oidcAdapter, identity);
case revoked of
when Ok(false) =>
return Response.unauthorized(Body.String("Token has been revoked"));
when Ok(true) =>
-- Continue with request
...
when Err(e: AuthError) =>
return Response.internalError(Body.String("Revocation check failed"));
end case;
end;

7.3 Logout 
-- Back-channel logout: IdP sends logout token to Nirdosha
function handleBackChannelLogout(
logoutToken: String,
adapter: OidcAdapter
): Result<Unit, AuthError>
effect(authorize)
is
-- Validate logout token (same as ID token but with event=logout)
-- Invalidate session in runtime cache
-- Return 200 OK to IdP
end;
 -- Front-channel logout: redirect through browser
function handleFrontChannelLogout(
sid: String,
issuer: String
): Response is
-- Clear browser session cookie
-- Invalidate session in runtime cache
return Response.ok(Body.String("Logged out"));
end;

 8. Proof of Possession8.1 mTLS Token Binding 
function validateMtlsBoundToken(
adapter: ApiKeyAdapter,
token: String,
clientCert: Certificate
): Result<VerifiedIdentity, AuthError>
effect(authorize)
is
-- Validate that the token's cnf (confirmation) claim matches the client certificate thumbprint
let cnf: Option<String> := extractCnfClaim(token);
case cnf of
when Some(expectedThumbprint: String) =>
let actualThumbprint: String := clientCert.sha256Thumbprint();
if expectedThumbprint == actualThumbprint then
return adapter.validateKey(token, Some(clientCert));
else
return Result[...].Err(AuthError.CertificateMismatch());
end if;
when None =>
return Result[...].Err(AuthError.MissingCertificateBinding());
end case;
end;

8.2 DPoP (Demonstrating Proof-of-Possession) 
function validateDpopToken(
adapter: OidcAdapter,
accessToken: String,
dpopProof: String,
requestMethod: HttpMethod,
requestUrl: Url
): Result<VerifiedIdentity, AuthError>
effect(authorize)
is
-- Validate DPoP proof JWT (signature, jti uniqueness, htu/match)
-- Extract public key from DPoP proof
-- Validate access token's cnf claim matches DPoP public key thumbprint
-- Return VerifiedIdentity only if all checks pass
end;

 9. Complete OIDC Callback Handler 
-- ============================================================
-- 9.1 Initial Authorization Redirect
-- ============================================================
 function initiateOidcLogin(
adapter: OidcAdapter,
redirectUri: String,
sessionStore: SessionStore
): Result<Url, AuthError>
effect(authorize, network)
is
-- Generate state (CSRF protection)
let state: String := crypto.randomString(32);
-- Generate nonce (replay protection)
let nonce: String := crypto.randomString(32);
-- Generate PKCE code_verifier and code_challenge
let codeVerifier: String := crypto.randomString(128);
let codeChallenge: String := crypto.sha256Base64Url(codeVerifier);
  -- Store state, nonce, code_verifier in session store (linked to state)
sessionStore.store(state, SessionData(
nonce := nonce,
codeVerifier := codeVerifier,
redirectUri := redirectUri,
createdAt := DateTime.now()));
  -- Build authorization URL
let authUrl: Url := Url.parse(adapter.config.discoveryUrl)
.join("/authorize")
.withQueryParams(Map.from([
("client_id", adapter.config.clientId),
("response_type", "code"),
("scope", adapter.config.scopes.join(" ")),
("redirect_uri", redirectUri),
("state", state),
("nonce", nonce),
("code_challenge", codeChallenge),
("code_challenge_method", "S256")
]));
  return Result[Url, AuthError].Ok(authUrl);
end;
 -- ============================================================
-- 9.2 Callback Handler (complete)
-- ============================================================
 function oauthCallbackHandler(
req: Request,
ctx: RouteContext,
adapter: OidcAdapter,
sessionStore: SessionStore
): Response
effect(authorize, network)
is
-- Extract query parameters
let code: Result<String, ParseError> := ctx.queryParam("code");
let state: Result<String, ParseError> := ctx.queryParam("state");
let errorParam: Option<String> := ctx.queryParam("error").ok();
let errorDescription: Option<String> := ctx.queryParam("error_description").ok();
  -- Handle IdP error response
case errorParam of
when Some(err: String) =>
return Response.badRequest(Body.String(
"IdP error: " + err + " - " + errorDescription ?? "no description"
));
when None =>
-- Continue
end case;
  -- Validate code and state are present
case (code, state) of
when (Ok(c: String), Ok(s: String)) =>
-- Step 1: Validate state against session store (CSRF protection)
let sessionResult: Result<SessionData, SessionError> := sessionStore.retrieve(s);
case sessionResult of
when Ok(session: SessionData) =>
-- Verify session not expired (5 minute timeout)
if DateTime.now().subtract(session.createdAt) > Duration.fromMinutes(5) then
sessionStore.delete(s);
return Response.badRequest(Body.String("Session expired"));
end if;
  -- Step 2: Exchange code for tokens
let tokenResult: Result<TokenResponse, AuthError> := 
adapter.exchangeCode(
code := c,
codeVerifier := session.codeVerifier,
redirectUri := session.redirectUri
);
case tokenResult of
when Ok(tokens: TokenResponse) =>
-- Step 3: Validate ID token
let identityResult: Result<VerifiedIdentity, AuthError> :=    endend;

adapter.validateToken(tokens.idToken);
case identityResult of
when Ok(identity: VerifiedIdentity) =>
-- Step 4: Validate nonce matches
let nonceClaim: Result<ClaimView["nonce", String], AuthError> :=
extractClaim[String](identity, "nonce", TypeTag[String]);
case nonceClaim of
when Ok(nonceView: ClaimView["nonce", String]) =>
if nonceView.value != session.nonce then
return Response.unauthorized(Body.String("Nonce mismatch"));
end if;
-- Step 5: Clean up session data
sessionStore.delete(s);
-- Step 6: Create application session (separate from identity)
let appSession: ApplicationSession := createApplicationSession(identity);
-- Step 7: Return success
return Response.redirect("/dashboard")
.withCookie(sessionCookie(appSession));
when Err(_) =>
return Response.unauthorized(Body.String("Missing nonce claim"));
end case;
when Err(e: AuthError) =>
return Response.unauthorized(Body.String("Invalid ID token: " + e.message));
end case;
when Err(e: AuthError) =>
return Response.unauthorized(Body.String("Token exchange failed: " + e.message));
end case;
when Err(SessionNotFound) =>
return Response.badRequest(Body.String("Invalid or expired state"));
end case;
when _ =>
return Response.badRequest(Body.String("Missing code or state"));
case;
 10. Session Management (Separated from Identity) 
-- ApplicationSession is separate from VerifiedIdentity.
-- It manages the browser session, not the IdP token.
record ApplicationSession: Linear is
sessionId: String,
identitySubject: String, -- Reference to who is logged in
identityIssuer: String, -- Which IdP authenticated them
createdAt: DateTime,
expiresAt: DateTime,
lastAccessedAt: DateTime
end;
 function createApplicationSession(identity: VerifiedIdentity): ApplicationSession is
return ApplicationSession(
sessionId := crypto.randomString(64),
identitySubject := identity.subject,
identityIssuer := identity.issuer,
createdAt := DateTime.now(),
expiresAt := DateTime.now().add(Duration.fromHours(8)),
lastAccessedAt := DateTime.now()
);
end;
 -- Session cookie (HTTP-only, Secure, SameSite)
function sessionCookie(session: ApplicationSession): Cookie is
return Cookie(
name := "medic loud_session",
value := session.sessionId,httpOnly := true,
secure := true,
sameSite := SameSite.Strict,
maxAge := Duration.fromHours(8)
);
end;

 11. What Nirdosha Does NOT Do (Explicitly Documented) 
-- ============================================================
-- 11.1 Out of Scope for Nirdosha Language & Runtime
-- ============================================================
 -- Nirdosha does NOT:
-- - Issue JWTs, SAML assertions, or any identity tokens
-- - Store user passwords or credentials
-- - Run an OIDC Provider, OAuth Authorization Server, or IdP
-- - Manage user registration, password reset, or account recovery
-- - Store signing keys (only verification keys)
-- - Act as a certificate authority
-- - Provide a user interface for login, consent, or account management
 -- Nirdosha DOES:
-- - Validate tokens issued by external IdPs
-- - Make validation results unforgeable in the type system
-- - Enforce authorization via compile-time types
-- - Provide protocol adapters for common identity protocols
-- - Manage application sessions (separate from identity tokens)
-- - Support token lifecycle operations (refresh, introspection, revocation)
 -- The boundary: Nirdosha is a relying party. IdPs are external.

 12. Integration with Rows 1–11Row How Corrected Identity Model Honors It1 (No GC) VerifiedIdentity is Free (copyable). TokenReference is Linear (manages cache slot). No heap allocation for identity data.2 (No races) VerifiedIdentity is immutable (val). Shareable across actors. Adapters are actor-local.3 (No deadlocks) Token validation is async message-passing to adapter actors. No blocking.4 (No overflow) Clock skew bounds are refinement types. Token age checks are SMT-proved.5 (Native speed) RoleView is a zero-size type. Compiles away completely. No runtime cost for authorization proofs.6 (Easy to learn) One pattern: validate external token → get VerifiedIdentity → check roles/claims. No framework wiring.7 (LLM-friendly) Fixed pattern: adapter.validateToken() → checkRole() → extractClaim(). Predictable structure.8 (Compositional) checkRole(identity, role) is a pure function of verified claims. Compositional semantics.9 (AI-native) LLM generates checkRole calls. Compiler verifies token type matches required role. Structured diagnostics.10 (Tamper-evidence) Validation policies are content-addressed. Changing trusted issuers changes the source hash.11 (Type foundation) VerifiedIdentity is a record. RoleView[Role] is a generic record. ClaimView[name, T] is a generic record. 
 13. Summary of CorrectionsIssue in Initial Draft CorrectionRuntime signs its own tokens Removed. Runtime only validates external tokens.User.attestation: Bytes Removed. Trust comes from module-private constructors, not runtime signatures.RoleToken with runtime signature Replaced with RoleView — a zero-size compile-time proof.ClaimProof with runtime signature Replaced with ClaimView — a zero-size compile-time proof.IdentityCapability grants signing keys Corrected: holds verification keys and client secrets only.Brute-force federation Replaced with issuer-based routing.Hardcoded OIDC endpoints Replaced with OIDC Discovery (.well-known/openid-configuration).Missing token introspection Added introspectToken() for opaque OAuth2 tokens.Missing audience/issuer validation Added explicit validation with structured errors.Missing PKCE, nonce, state Added complete authorization redirect + callback flow.Missing refresh tokens Added exchangeRefreshToken().Missing revocationMissing clock skew, nbf, iat Added to ValidationPolicy and validation logic.Missing mTLS/DPoP Added validateMtlsBoundToken() and validateDpopToken().Session management conflated Separated ApplicationSession from VerifiedIdentity.User strictly Linear Changed to VerifiedIdentity (Free) + TokenReference (Linear). 
 Corrected amendment to Nirdosha Row 12. Addresses the relying-party critique by removing runtime token minting, adding protocol-agnostic abstractions, and
documenting the explicit boundary between language/runtime and external IdPsAdded checkRevocation() and back-channel/front-channel logout.