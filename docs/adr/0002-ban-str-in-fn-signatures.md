# 0002: Ban `str` as a function argument/return type

Date: 2026-08-23
Status: accepted

## Context

Nirdosha's core value proposition is that an LLM-written program is
provably free of a set of whole bug classes — memory safety, data
races, deadlocks, integer/buffer overflow (README). Stringly-typed
control flow (`if status == "PENDING"`, `match currency { "USD" =>
..., "EUR" => ... }`) sits outside every one of those proofs: a typo'd
literal, an unhandled case, or a silently-accepted new value are all
compile-clean and only fail at runtime, or never fail loudly at all —
exactly the class of defect the rest of the type system exists to rule
out statically. Real `enum`s already get exhaustive `match` checking
and already render as searchable dropdowns in `emit-ui`
(`docs/LANGUAGE.md` §11) for free; nothing was pushing authors (human
or LLM) toward using them at a function boundary instead of a bare
`str`.

This decision shipped in a single implementation session (2026-08-23),
with no RFC, no discussion window, and no advance notice — a real
breaking change to what typechecks, made unilaterally. `docs/ECOSYSTEM.md`
§G5 names this specific event as the concrete precedent motivating
[`GOVERNANCE.md`](../../GOVERNANCE.md)'s RFC requirement for future
breaking changes; this ADR is that decision recorded after the fact,
not before.

## Decision

A user-defined `fn`'s parameter or return type may not be, or contain
(recursively, through `Result`/`Option`/generics/`box`/`&`/`thread`/
`chan`/`Vector`/`Matrix`/`fn(...) -> ...`), a bare `str`
(`TypeErrorKind::StrInFnSignature`, `typeck.rs::check_fn`). Two
conventions replace what a bare `str` parameter/return used to carry:

- A closed, categorical vocabulary becomes a small zero-payload `enum`.
- Genuine free text gets wrapped in a one-field carrier struct,
  conventionally named `Text { value: str }`.

Three things are exempt **by construction**, not by special-casing,
because none of them is a `fn_decl`: builtins (the language's real
external-I/O boundary — an HTTP body, SQL text, a JWT — is
irreducibly `str`), struct/enum constructors (a struct can freely keep
a `str` field), and `transact`'s synthesized `txn_id` parameter (must
stay a plain scalar for WAL durability). An enum variant's own payload
(`External(str)`) is likewise unaffected — the check inspects a
signature's declared type expression, not a named type's internal
declaration. Full detail: `docs/LANGUAGE.md` §6b.

## Consequences

- Every existing `.nir` program with a `str`-typed `fn` parameter or
  return became a compile error in this one session — a real breaking
  change to the language surface, not additive.
- The `==`/`!=` interpreter gap this migration surfaced and fixed in
  the same pass (`Value::Struct`/`Value::Enum` had no binary-operator
  arm, despite typechecking cleanly) is documented in `docs/LANGUAGE.md`
  §6b rather than repeated here — a reminder that "push people toward
  enums" is only a real improvement if comparing the resulting enums
  then actually works at runtime, which it didn't until this same pass
  fixed it.
- Established the precedent this repo's process gap is now fixed
  against: see [`GOVERNANCE.md`](../../GOVERNANCE.md#how-day-to-day-decisions-get-made)
  and [`rfcs/README.md`](../../rfcs/README.md) — a change of this shape
  going forward gets an RFC and a review window before it ships, not
  after.
