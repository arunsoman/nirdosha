# Nirdosha — agent instructions

Nirdosha is a security/systems-focused DSL (structs/enums, affine
`box`/`db`/`tcp`/`file`/`mq`/`sandbox` handles, Row 12 identity) with a
Rust compiler (interpreter + LLVM codegen) at `crates/compiler/`. This
file is the minimum needed to orient — everything else loads on demand
from the table below. Don't read `docs/GRAMMAR.md`/`docs/LANGUAGE.md`/etc.
up front; load the one row you actually need.

## Build / test / run

```
cd crates/compiler
cargo build                              # build the nirdosha CLI
cargo test                                # full suite (unit + crates/compiler/tests/*.rs)
cargo test --test <name>                  # one integration test file
cargo run -- <file.nir>                   # interpret
cargo run -- build <file.nir> -o <out>    # compile to a native binary (LLVM, -O2)
cargo run -- emit-ast <file.nir>          # parsed AST as JSON -- lex+parse ONLY, does NOT typecheck (by design: an ill-typed program is still inspectable)
cargo run -- emit-ui <file.nir> -o out.html   # derive a web UI from struct/fn conventions -- full typecheck+ownership pass, no side effects; the actual fastest typecheck-only smoke test
cargo run -- serve <file.nir> --port 8080     # run as a real HTTP service
```

## Facts that will cost you real time to rediscover — read these once

- **No `::` token exists anywhere in the lexer.** Enum variants are
  flat, unqualified calls — `Some(5)`, `None()`, `Circle(r)` in a
  `match` arm — never `EnumName::Variant`. Writing the Rust-style
  qualified form is a hard parse error, not a style issue. A
  zero-payload variant still needs `()` at the call site (`None()`,
  not bare `None`).
- **`str` cannot be a user `fn`'s parameter or return type**, checked
  recursively through `Result`/`Option`/generics/`box`/`&`/`thread`/
  `chan`/`Vector`/`Matrix`/`fn` types (`TypeErrorKind::
  StrInFnSignature`, `typeck.rs::check_fn`). Use a real `enum` for
  categorical data, or `struct Text { value: str }` for free text. Full
  rule + exemptions (builtins, constructors, `transact`'s `txn_id`):
  `docs/LANGUAGE.md` §6b.
- **`str` has zero concatenation, zero formatting.** Every string a
  program produces is a literal from source, or comes back from a
  builtin (`json_get_str`, `db_query`, ...) — there is no way to build
  a string at runtime from parts.
- **No statement separator** (no semicolons, no significant
  newlines) — wherever a token could extend the current expression or
  start a new statement, the parser always extends. `return x` then
  `-y` on the next line parses as one statement, `return (x - y)`. See
  `docs/GRAMMAR.md`'s disambiguation-rule section before writing multi-line
  `.nir` source.
- **Docs get updated in the same step as code, never deferred.** Every
  `.md` file in this repo is treated as load-bearing, not aspirational
  — see the next section for why some of them can't just be deleted
  when superseded.
- Current project status/priorities: **`docs/ROADMAP.md`** — read this
  before starting any nontrivial task, so you're not duplicating
  something already done or already scoped elsewhere.

## Load on demand — design/spec docs

All of the following live under `docs/`.

| Need | Load |
|---|---|
| Current status of everything, what's shipped, what's pending, sequencing | `docs/ROADMAP.md` |
| EBNF grammar, LL(1)/parser disambiguation rules | `docs/GRAMMAR.md` |
| Type system, operators, builtins list, what's compiled vs. interpreter-only (verify against `codegen.rs::check_supported` directly, not just this doc) | `docs/LANGUAGE.md` |
| Design philosophy / the "rows" (no-GC, no-races, determinism, LLM-friendliness, ...) | `docs/goal.md` |
| Phase-by-phase build plan (0.5 → 5) | `docs/Nirdosha_Unified_Plan.md` |
| `transact` durability protocol (WAL, crash replay, `txn_id`, retry/timeout) | `docs/TRANSACT.md` |
| `workflow` state machines, `on_entry`/`on_exit` notification actions, presence bridge | `docs/WORKFLOW.md` |
| `sandbox`/`stop` process-isolation design | `docs/SANDBOXING.md` |
| Native iOS/Android app generation (manifest-driven codegen, Standard/Rich profiles) — **design only, not built**, see `docs/ROADMAP.md` Track D | `docs/MOBILE.md` |
| Multi-renderer UI manifest (web/TUI/mobile), real module/package system — **discussion only, not built**; `validate <fn_name> { pre/post }` Hoare contracts (F3) — **built**, see `docs/ROADMAP.md` Track F | `docs/NEXT_GEN.md` |
| Row 11 (`struct`/`enum`/generics/`match`) design rationale | `docs/nirdosha_row11_amendment.md` |
| Row 12 (identity/`VerifiedIdentity`/sessions/API keys) design | `docs/nirdosha_row12_functions_identity.md` |
| Historical build journal (early phases, numbered "updates") | `docs/PHASE0.md` |
| Where Nirdosha's std-lib design departs from the two earlier aspirational specs | `docs/PROTOLANG_PORT.md` (itself responds to `docs/protolang_reference_specification.md`/`docs/protolang_std_io_specification.md`) |
| LLM-agent-facing HTTP API spec (constrained gen, validation, sandboxed exec, provenance) — **spec only, server not built**, see `docs/ROADMAP.md` Track C | `docs/nirdosha-agent-api.md` |
| Remaining UI-DSL doc debt | `crates/compiler/UI_DSL_TODO.md` |
| The trade-finance demo app's own module-by-module build tracker | `examples/trade-finance/todo.md` |
| Browsable concatenation of every `.nir` example (generated locally, not in git) | `all_examples.md` |

**Not this project's work, don't load for Nirdosha tasks**:
`docs/llm-ops-api-spec.md`/`docs/llm-ops-api-spec-v2.md` are generic multi-backend
LLM training/serving specs with zero Nirdosha-specific content.
`benchmarks/julia/*.jl` are 6 standalone perf-comparison scripts, not
packages or tooling.

## Load on demand — large source files

`crates/compiler/src/INDEX.md` maps every file over ~500 lines
(`codegen.rs`/`interpreter.rs`/`typeck.rs`/`ast.rs`/`parser.rs`/
`ui_gen.rs`/`serve.rs`/`ownership.rs`/`runtime_kernels.rs`/`smt.rs`/
`refine.rs`/`main.rs`) to its structural pieces by name, with an
approximate line number as a fast-path hint — check that before reading
one of these files in full. The names are the durable part; a line
number will drift as the file is edited, `grep -n` the name if it looks
off rather than trusting a stale offset.
