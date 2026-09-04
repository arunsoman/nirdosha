# Governance

How decisions get made in Nirdosha, and who can make them. Written
2026-09-04 as the project moved from purely solo-maintained to having
real (if not yet active) maintainer access — see
[`docs/ECOSYSTEM.md` §G5](./docs/ECOSYSTEM.md) for the gap analysis
this closes, and [`MAINTAINERS.md`](./MAINTAINERS.md) for who currently
holds which role.

## Roles

| Role | Grants | Who |
|---|---|---|
| **Owner** | Repo admin: branch protection, releases, secrets, adding/removing maintainers. Final say when consensus doesn't form. | [`MAINTAINERS.md`](./MAINTAINERS.md) |
| **Maintainer** | Push access, reviews and merges PRs (branch protection requires 1 approval — a maintainer can approve someone else's PR, not their own), triages issues, can shepherd an RFC. Scoped in practice to the area(s) listed for them in [`AREAS.md`](./AREAS.md), though nothing in GitHub enforces that boundary technically. | [`MAINTAINERS.md`](./MAINTAINERS.md) |
| **Triager** | Labels, closes duplicates/invalid issues, requests changes on PRs, cannot merge. A step for someone building toward Maintainer, not a separate track. | none appointed yet — see [Contributor funnel](#contributor-funnel) |

Granting or revoking any of these is an Owner action, done by adding/
removing the person from [`MAINTAINERS.md`](./MAINTAINERS.md) (or the
triager list) and the corresponding GitHub permission, in the same PR.

## How day-to-day decisions get made

- **A bug fix, doc fix, or small feature inside one area:** the
  maintainer for that area (or the Owner, if unassigned) reviews and
  merges. No separate process.
- **Anything cross-cutting, breaking, or that changes the language
  surface, grammar, or a public interface (CLI flags, the manifest
  format, the plugin ABI):** goes through the [RFC process](./rfcs/README.md)
  first. This is the direct fix for the gap `docs/ECOSYSTEM.md` §G5
  named: the `str`-in-signatures ban shipped in one session with no
  proposal or review window — a real breaking-change precedent this
  process exists to not repeat. See
  [`docs/adr/0002-ban-str-in-fn-signatures.md`](./docs/adr/0002-ban-str-in-fn-signatures.md)
  for that decision recorded after the fact.
- **A decision made outside the RFC process anyway** (a judgment call
  during implementation, not a designed-up-front change) gets recorded
  as an [ADR](./docs/adr/README.md) so the reasoning survives — see
  `docs/adr/` for the pattern, mirrored from
  [`crates/compiler/src/INDEX.md`](./crates/compiler/src/INDEX.md)'s
  own "durable name, not durable line number" approach to documenting
  decisions that would otherwise only live in a commit message.
- **Disagreement that doesn't resolve in review:** the Owner decides.
  This is a small project — there is no vote, no quorum, just an
  escalation path so a stuck PR doesn't stay stuck.

## Branch protection

`main` is protected (configured 2026-09-04): merging requires a green
`build` + `build-windows` CI run and at least one approving review;
force-pushes and branch deletion are blocked; conversation threads
must be resolved before merge. The repo Owner can bypass in a genuine
emergency (`enforce_admins` is off) — this is a deliberate, disclosed
exception for a project with real bus-factor risk, not an oversight;
using it outside an actual emergency defeats the point of having the
rule and should get called out in the next PR, not left silent.

## Releases

- Tags matching `v*` trigger [`release.yml`](./.github/workflows/release.yml)
  (prebuilt binaries → GitHub Release) and
  [`docker.yml`](./.github/workflows/docker.yml) (container images,
  cosign-signed and SBOM'd, all via GitHub OIDC — no long-lived
  registry credentials are stored as repo secrets today; verified
  2026-09-04 via `gh secret list`, which returned none, and both
  workflows authenticate with the ephemeral `GITHUB_TOKEN`/OIDC, not a
  personal access token. If a future workflow needs a third-party
  registry or crates.io, it should use that same OIDC/trusted-publishing
  pattern — a GitHub Environment with an OIDC trust relationship, not a
  PAT pasted into repo secrets — so a release doesn't depend on any one
  person's account.
- **Signed tags — policy, not yet enforced.** A release tag should be
  a GPG- or SSH-signed `git tag -s`, not a plain tag, so a release's
  provenance doesn't rest solely on whoever had push access at the
  time. This isn't yet enforced by a GitHub tag-protection rule because
  that requires each maintainer's signing key to be registered with
  GitHub first — a per-person setup step, not something to switch on
  as a side effect of writing this document. Tracked as a follow-up;
  whoever cuts the next release should sign the tag by hand in the
  meantime.
- Only the Owner cuts releases today (single point of failure,
  disclosed — see [`MAINTAINERS.md`](./MAINTAINERS.md)). Delegating
  this needs at minimum GitHub Environments with required reviewers,
  not a shared credential; not yet set up.

## Contributor funnel

- Issues are triaged weekly and labeled from the set in
  [`.github/labels.yml`](./.github/labels.yml) (the file is the
  documented source of truth; `gh label list` on the live repo should
  match it — reconcile the file, not the repo, if they drift).
  `good first issue`/`help wanted` are kept current on real open
  issues, not aspirational.
- **Triage SLA: 48 hours** to a first response (label, question, or
  "seen, will look") on a new issue or PR — not to a full resolution.
  See [`CONTRIBUTING.md`](./CONTRIBUTING.md#response-time) for how this
  relates to the "about a week" full-response expectation.
- [GitHub Discussions](https://github.com/arunsoman/nirdosha/discussions)
  is enabled for design chatter that isn't yet a formal RFC.
- The [Public Roadmap](./docs/PUBLIC_ROADMAP.md) is linked from the
  README so a prospective contributor can see what's open without
  reading the full internal roadmap.

## Documents this governance model depends on

- [`MAINTAINERS.md`](./MAINTAINERS.md) — who holds which role, today.
- [`AREAS.md`](./AREAS.md) — which subsystem each maintainer is
  responsible for.
- [`.github/CODEOWNERS`](./.github/CODEOWNERS) — GitHub's own
  mechanical mirror of `AREAS.md`, so review requests route
  automatically.
- [`rfcs/`](./rfcs/README.md) — the RFC process and its current
  proposals.
- [`docs/adr/`](./docs/adr/README.md) — decisions made outside the RFC
  process, recorded after the fact.
- [`CODE_OF_CONDUCT.md`](./CODE_OF_CONDUCT.md) / [`SECURITY.md`](./SECURITY.md) —
  unchanged by this document, still the standing policies for conduct
  and vulnerability reports.
