# Architecture Decision Records

A decision made **outside** the [RFC process](../../rfcs/README.md) —
a judgment call made while implementing something, not a design
proposed and discussed up front — gets recorded here so the reasoning
survives past the commit that made it. See
[`GOVERNANCE.md`](../../GOVERNANCE.md#how-day-to-day-decisions-get-made)
for when something needs an RFC instead of an ADR.

Same pattern as [`crates/compiler/src/INDEX.md`](../../crates/compiler/src/INDEX.md):
a durable name (`NNNN-title.md`), not a durable line number or a
promise the decision is still current — an ADR records that a
decision *was made* and why, not that it's still the right call
forever. A later ADR can supersede an earlier one; it doesn't edit or
delete it.

## Format

`docs/adr/NNNN-title.md`, `NNNN` = next unused 4-digit number:

```markdown
# NNNN: Title

Date: YYYY-MM-DD
Status: accepted | superseded by NNNN

## Context
What situation forced a decision.

## Decision
What was actually decided.

## Consequences
What this makes easier, harder, or newly possible — including the
honest downside, not just the win.
```

## Index

| # | Title | Status |
|---|---|---|
| [0001](./0001-vendor-z3-except-macos.md) | Vendor Z3 for release builds, except macOS (system Z3 there) | accepted |
| [0002](./0002-ban-str-in-fn-signatures.md) | Ban `str` as a function argument/return type | accepted |
| [0003](./0003-runtime-kernels-cargo-dependency.md) | Split the compiled-path runtime kernels into their own Cargo-dependency-aware crate | accepted |
| [0004](./0004-external-data-service-boundary.md) | External Data & Service Boundary — plugin-backed `db`/`mq` connections by URL scheme | accepted |
