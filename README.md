# Nirdosha

Nirdosha is a programming language project (working name "Nirdosha") whose source
files use the `.nir` extension. The repository contains a **compiler** together
with a broad set of **examples** and a set of **benchmarks** written in the
language itself.

> Note: This README was generated from the repository's directory layout and
> the names/topics of the example `.nir` files. It is intended as a high-level
> orientation guide, not a formal specification.

---

## Repository Layout

```
nirdosha/
├── compiler/
│   └── examples/         # Illustrative .nir programs (the language's "tour")
│       ├── rev-assurence/
│       └── trade-finance/
├── benchmarks/
│   └── nirdosha/         # Performance micro-benchmarks written in .nir
└── README.md            # This file
```

---

## What the Language Seems to Cover

Based on the example files present, Nirdosha appears to be a systems-level
language with first-class support for memory-safety primitives, concurrency,
effects, and richer domain modeling. The example programs suggest the
following feature areas:

### Core Language
- **`hello.nir`** — Basic program structure / entry point
- **`factorial.nir`** — Functions and recursion
- **`loop.nir`** — Control flow / iteration
- **`floats.nir`** — Floating-point arithmetic
- **`strings.nir`** — String handling
- **`structs_enums.nir`** — Algebraic data types (structs and enums)
- **`generics.nir`** — Generic / parametric types

### Memory & Ownership Model
- **`ownership.nir`** — Ownership semantics (move discipline)
- **`borrow.nir`** — Borrowing / references

### Concurrency
- **`threads.nir`** — Native threads
- **`channels.nir`** — Channel-based message passing
- **`sandbox.nir`** / **`sandbox_channels.nir`** — Sandboxed execution and
  isolated communication

### Numerics & Linear Algebra
- **`matrices.nir`** — Matrix types/operations
- **`linalg.nir`** — Linear-algebra routines
- **`matmul.nir`**, **`det.nir`**, **`dot.nir`** — Benchmarked matrix ops
- **`kalman.nir`**, **`sensor_fusion.nir`** — Filtering / sensor fusion
  algorithms

### I/O, Networking & Effects
- **`file_io.nir`** — File input/output
- **`effects.nir`** — Effect system / tracked side effects
- **`tcp_client.nir`** — TCP networking
- **`http.nir`** / **`https.nir`** — HTTP(S) client usage
- **`json.nir`** — JSON serialization/deserialization
- **`db.nir`** — Database access

### Security, Capabilities & Privilege
- **`privileged_fn.nir`** — Privileged / capability-gated functions
- **`identity_mock.nir`**, **`row12_identity.nir`** — Identity-related
  examples

### Application-Scale Examples
- **`store.nir`** — A store / inventory style application
- **`transact.nir`**, **`transact_cross_process.nir`** — Transactional
  semantics, including across processes
- **`ui_todo.nir`** — A small UI / todo application
- **`observability.nir`** — Tracing / metrics / observability
- **`payments_mock.nir`** — A mock payments flow
- **`wargame_agents.nir`** — Multi-agent simulation (wargaming)
- **`rev-assurence/rev_assurance.nir`** — Reinsurance / risk-transfer domain
  model (large example)
- **`trade-finance/trade_finance.nir`** — Trade-finance domain model
  (large example)

---

## Benchmarks

The `benchmarks/nirdosha/` directory contains self-contained `.nir` programs
used to measure performance:

| Benchmark | Focus |
|-----------|-------|
| `fib.nir` | Recursive function call overhead |
| `floatloop.nir` | Floating-point loop throughput |
| `dot.nir` | Vector dot product |
| `matmul.nir` | Matrix multiplication |
| `det.nir` | Matrix determinant |
| `kalman.nir` | Kalman-filter style computation |

---

## Compiled Examples Bundle

A combined, browsable view of every example is available in
[`all_examples.md`](./all_examples.md), and a zipped archive of all `.nir`
files is available in [`nir_files.zip`](./nir_files.zip).

---

## Getting Started (sketch)

Because the compiler sources are the authoritative reference, the recommended
way to explore the language is:

1. Read [`all_examples.md`](./all_examples.md) to skim the full tour of
   features by topic.
2. Start with the small programs (`hello.nir`, `factorial.nir`, `loop.nir`,
   `ownership.nir`, `borrow.nir`).
3. Progress to concurrency (`threads.nir`, `channels.nir`, `sandbox.nir`) and
   then the domain-scale examples (`store.nir`, `transact.nir`,
   `rev-assurence/`, `trade-finance/`).
4. Use the `benchmarks/` programs to understand the language's performance
   characteristics for numerics and recursion.

---

## Status

This README is a snapshot inferred from the file set at generation time. For
precise syntax, semantics, and tooling commands, consult the compiler
sources under `compiler/`.