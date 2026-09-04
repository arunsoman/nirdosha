---
state: draft
shepherd: (unassigned)
---

# RFC NNNN: Title

## Motivation

What problem is this solving, for whom, and why now. Link the issue(s)
or discussion that prompted it if any exist.

## Design

The actual proposal. Concrete enough that someone could start
implementing it from this section alone — grammar changes, new
`TypeErrorKind`/`ErrorKind` variants, CLI surface, file formats, all
belong here, not hand-waved.

## Effect on the permission model

Does this change what `requires(role/claim: ...)`, `acquire`, a
`screen`'s view/edit gates, or `serve.rs`'s server-side enforcement can
express or must check? If genuinely none, say so explicitly — don't
omit the section.

## Compatibility

Does an existing `.nir` program's behavior change? A grammar addition
that only introduces new syntax at a spot nothing valid could occupy
before is backward-compatible by construction — say why, the same way
`docs/LANGUAGE.md` §17 does for real-namespace `module`. A breaking
change needs a migration note, not just a warning that it's breaking.

## Rejected alternatives

What else was considered and why it lost — this is what lets a
`rejected`/`postponed` RFC actually prevent the same idea from being
re-proposed from zero later.

## Open questions

Anything left genuinely undecided at merge time. Not a place to hide
scope that should actually block acceptance.
