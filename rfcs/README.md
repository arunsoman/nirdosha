# RFCs

A lightweight, PR-based process for a decision that's cross-cutting,
breaking, or shapes the language surface / a public interface — see
[`GOVERNANCE.md`](../GOVERNANCE.md#how-day-to-day-decisions-get-made)
for exactly which decisions need this versus ordinary review. This
process didn't exist before 2026-09-04; the `str`-in-signatures ban
(`docs/adr/0002-ban-str-in-fn-signatures.md`) shipped in one session
with none of this, which is the concrete gap it closes.

## Process

1. Copy [`0000-template.md`](./0000-template.md) to
   `rfcs/NNNN-short-title.md`, `NNNN` = next unused 4-digit number.
2. Open a PR adding the file. State `draft` in its front matter.
3. One existing maintainer volunteers (or is asked) as **shepherd** —
   noted in the RFC's front matter — responsible for driving it to a
   state change, not for agreeing with it.
4. Discussion happens in the PR's comments (or a linked GitHub
   Discussion thread for anything long-form). State moves to
   `discussion` once there's been at least one substantive comment.
5. The shepherd calls it: `accepted` (merge as-is, implementation can
   start/continue), `postponed` (real idea, wrong time — merged with
   that state so the discussion isn't lost), or `rejected` (merged with
   reasoning recorded, so the same proposal doesn't get re-litigated
   from scratch later).
6. Once the design it describes is actually built and shipped, a
   follow-up PR flips the state to `implemented` and links the PR(s)
   that did it. An `accepted` RFC whose implementation never lands
   isn't a failure of this process — it just stays `accepted`,
   honestly, until someone picks it up.

## States

`draft` → `discussion` → **`accepted`** → `implemented`
`draft` → `discussion` → **`postponed`**
`draft` → `discussion` → **`rejected`**

## Current RFCs

| # | Title | State | Shepherd |
|---|---|---|---|
| [0001](./0001-package-manifest-format.md) | Package manifest format (Cargo-based package manager) | draft | *unassigned* |
| [0002](./0002-editor-tooling-lsp-tree-sitter.md) | Editor/tooling ecosystem: tree-sitter grammar + minimal LSP | draft | *unassigned* |
| [0003](./0003-plugin-abi-v2.md) | Plugin ABI v2 — effect declarations, async/sync policy, versioning | draft | *unassigned* |
| [0004](./0004-native-plugin-sandboxing.md) | Trust model for native (Kind A) plugins | draft | *unassigned* |
| [0005](./0005-plugin-boundary-safety-and-performance.md) | The Nirdosha↔Rust plugin boundary — safety and performance | draft | *unassigned* |
| [0006](./0006-structured-concurrency.md) | Structured concurrency for native threads — Pillars 1-4 | draft | *unassigned* |

## What doesn't need an RFC

A decision made in the course of implementing something, not designed
up front — a judgment call, a workaround, a "why does this file vendor
X instead of using the system one" — doesn't need this process. Record
it as an [ADR](../docs/adr/README.md) instead, after the fact.
