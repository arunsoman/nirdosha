# Security Policy

## Reporting a vulnerability

Please report security issues privately, not as a public GitHub issue —
use [GitHub's private vulnerability reporting](https://github.com/kannamma-labs/nirdosha/security/advisories/new)
for this repository (the "Security" tab → "Report a vulnerability").
This opens a private advisory only the maintainer can see until it's
resolved.

Please include:

- What you found and why it's a security issue, not just a bug
- A minimal `.nir` file or request that reproduces it
- The affected area if you know it (parser/typeck/ownership, `smt.rs`
  refinement checking, `serve.rs`'s HTTP/auth boundary, `sandbox`
  process isolation, `transact`/`workflow` durability, or the
  generated UI client)

You should get an initial response within a week. This is a small team
(see [`MAINTAINERS.md`](./MAINTAINERS.md)), not a funded security team —
response time is best-effort, not SLA-backed.

## Scope

Nirdosha is under active development. Some safety properties are proven
today (ownership/affine types, SMT-discharged overflow bounds, the
concurrency model); others are explicitly aspirational and documented
as such — see [Honest Scope & Roadmap](https://github.com/kannamma-labs/nirdosha/wiki/Honest-Scope-and-Roadmap).
A report that a *documented, disclosed* limitation is exploitable is
still useful — please file it — but it's triaged differently from a
violation of a claim the project actually makes.

Areas most worth scrutiny, since they're the actual security
boundary in a real deployment:

- `serve.rs` — the HTTP/auth boundary: token validation
  (`oidc_validate_token`), role/claim gate enforcement, field-level
  RBAC redaction, request-size/rate limits
- `sandbox`/`stop` — real OS-process isolation (`docs/SANDBOXING.md`)
- `ownership.rs`/`typeck.rs` — the static guarantees the whole project
  is built around; a real counterexample to "the type checker accepts
  it, therefore it's memory/race-safe" is a serious finding
- Anything that would let untrusted `.nir` source (e.g. LLM-generated
  code fed through the agent-facing tooling) escape a declared
  `sandbox` or bypass a `requires(role: ...)` gate

Out of scope: findings that require local code execution with the same
privileges as the `nirdosha` process itself, or that only affect a
locally-run `nirdosha serve` instance with no authentication configured
(that's a documented development mode, not a deployment posture).

## Supported versions

This project is pre-1.0 and does not yet have a formal support-window
policy — see Track A ("Compatibility/versioning policy") in
[`docs/PUBLIC_ROADMAP.md`](./docs/PUBLIC_ROADMAP.md). Until that lands, the only
supported version is the latest commit on `main`.
