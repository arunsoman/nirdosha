# Nirdosha Agent-Facing API

Functional APIs that wrap the Nirdosha compiler's real capabilities
to make the hardest parts of AI-generated code work.

Every endpoint here is grounded in something that already exists in
the codebase or is explicitly planned in Nirdosha_Unified_Plan.md.
Nothing is aspirational hand-waving -- if it's not built yet, it
says so and references the phase that delivers it.

------------------------------------------------------------------------
WHAT PROBLEM THESE APIs SOLVE
------------------------------------------------------------------------

The hardest things in the LLM-code-generation world today:

  1. An LLM generates code that looks right but is syntactically
     invalid -- you only find out after running it, wasting a turn.
  2. An LLM generates code that parses but has type errors -- the
     error message is English prose, not machine-parseable, so
     self-repair is guesswork.
  3. An LLM generates code that type-checks but has subtle safety
     bugs (overflow, race, bounds) -- no runtime catches them,
     no proof system flags them.
  4. Running LLM-generated code safely requires sandboxing, which
     is usually an afterthought bolted on with Docker, not a
     language-level primitive.
  5. Repeated runs of the same LLM-generated simulation give
     different results -- nondeterminism makes debugging and
     auditing impossible.
  6. There is no way to incrementally improve an LLM's output --
     you either accept the whole generation or throw it away.
  7. There is no way to measure whether an LLM is actually
     getting better at writing code for a specific domain --
     pass@1 on HumanEval doesn't tell you about sensor fusion.

Nirdosha's existing architecture addresses each of these:

  1. LL(1) grammar + planned GBNF export → constrained decoding
  2. --format=json → structured diagnostics (already shipped)
  3. refine.rs + smt.rs → SMT-discharged bounds proofs (shipped)
  4. sandbox/stop → real OS process isolation (shipped)
  5. rand_seed → deterministic RNG (shipped)
  6. validate_fragment → type-check expression fragments in
     context (shipped)
  7. bench/ corpus → pass@1 + self-repair rate (Phase 5, scaffolded)

These APIs wrap those capabilities into callable endpoints.

------------------------------------------------------------------------
API INDEX
------------------------------------------------------------------------

  A. Code Generation & Validation
     A1. POST /v1/generate         -- constrained generation + validation
     A2. POST /v1/validate         -- validate a complete program
     A3. POST /v1/validate-fragment-- validate an expression in context
     A4. POST /v1/repair           -- self-repair loop with diagnostics
     A5. POST /v1/splice           -- splice a validated fragment into a program

  B. Execution & Simulation
     B1. POST /v1/run               -- run a program (interpreted)
     B2. POST /v1/run-sandboxed     -- run in an isolated OS process
     B3. POST /v1/run-deterministic -- run with enforced seed + hash check
     B4. POST /v1/build             -- compile to native binary

  C. Compiler Introspection
     C1. GET  /v1/grammar           -- get the GBNF grammar artifact
     C2. GET  /v1/types             -- list all types + their shapes
     C3. GET  /v1/builtins          -- list all builtins + signatures
     C4. POST /v1/emit-ast          -- serialize a program's AST as JSON

  D. Benchmarking & Evaluation
     D1. POST /v1/bench/run         -- run pass@1 on a corpus
     D2. POST /v1/bench/repair-rate -- measure self-repair within N retries
     D3. GET  /v1/bench/corpus      -- list benchmark corpus
     D4. POST /v1/bench/corpus      -- add a task to the corpus

  E. Provenance & Reproducibility
     E1. POST /v1/provenance/hash   -- content-hash a source file
     E2. POST /v1/provenance/verify -- verify binary matches attested source
     E3. GET  /v1/provenance/audit  -- get audit trail for a run

------------------------------------------------------------------------
BASE
------------------------------------------------------------------------

Base URL:    http://localhost:7878  (nirdosha-compiler-server)
Content-Type: application/json
Auth:        none (local tool -- not a multi-tenant platform)

The server is a thin HTTP wrapper around the compiler crate
(`compiler/src/lib.rs`). It links the compiler as a library and
exposes its existing functions (run, run_diagnostic,
validate_fragment, typecheck, check_ownership) as HTTP endpoints.

------------------------------------------------------------------------
A. CODE GENERATION & VALIDATION
------------------------------------------------------------------------

================ A1. POST /v1/generate ====================

Constrained generation: the server loads the GBNF grammar artifact
(Phase 2 deliverable) and feeds it to a grammar-constrained decoding
backend (llama.cpp's grammar sampler, outlines, or xgrammar). The
LLM can only produce syntactically valid Nirdosha.

The generated code is then immediately validated by the compiler
(typecheck + ownership check). If validation fails, the structured
diagnostics are returned alongside the code -- the caller can feed
them back to the LLM for self-repair without a separate round-trip.

THIS IS THE HARD THING IT MAKES EASY:
  Today, getting an LLM to generate syntactically valid code in a
  niche language requires either (a) few-shot prompting and praying,
  or (b) building a custom grammar-constrained decoder integration
  from scratch. This endpoint does both in one call: grammar
  constrains the tokens, compiler validates the result, diagnostics
  come back structured.

```
POST /v1/generate
{
  "prompt": "Write a Nirdosha function that computes the dot product
   of two 3-vectors using the dot() builtin.",
  "context?: {
    "preceding_code": "fn main() { ... }",  // code already in the file
    "in_scope": [                              // what the LLM should know
      { "name": "v", "type": "Vector(f64, 3)" },
      { "name": "w", "type": "Vector(f64, 3)" }
    ]
  },
  "constraint": {
    "grammar": "gbnf",               // use the shipped nirdosha.gbnf
    "backend": "llamacpp"|"outlines"|"xgrammar",
    "model_endpoint": "http://localhost:8080/v1",  // OpenAI-compatible
    "model_name": "qwen2.5-coder-7b",
    "temperature": 0.0,
    "max_tokens": 512
  },
  "validate": true,                  // run typecheck + ownership after gen
  "repair_attempts": 3               // if validation fails, feed
                                     // diagnostics back and retry
}
```

Response (200 OK):
```
{
  "code": "fn dot3(a: Vector(f64, 3), b: Vector(f64, 3)) -> f64 {\n
             return dot(a, b)\n}",
  "valid": true,
  "diagnostics": [],
  "repair_trace": [                  // empty if valid on first try
    {
      "attempt": 1,
      "code": "fn dot3(a, b) -> f64 { return dot(a, b) }",
      "diagnostics": [
        { "stage": "type", "diagnostic": {
            "kind": "MissingTypeAnnotation",
            "span": { "line": 1, "col": 11 },
            "message": "parameter 'a' needs a type annotation"
        }}
      ]
    }
  ],
  "tokens_generated": 28,
  "latency_ms": 340
}
```

If repair_attempts is exhausted without producing valid code:
```
{
  "code": "<last attempt>",
  "valid": false,
  "diagnostics": [ ... last diagnostics ... ],
  "repair_trace": [ ... all attempts ... ],
  "error": "REPAIR_EXHAUSTED"
}
```

DEPENDENCY: GBNF grammar artifact (Phase 2, §4.2.3). Until that
ships, this endpoint falls back to unconstrained generation +
post-hoc validation (still useful -- you get structured diagnostics
either way, just no grammar enforcement at decode time).

================ A2. POST /v1/validate ====================

Validate a complete Nirdosha program without running it.
Wraps: lex → parse → typecheck → ownership check.
This is the compiler's existing pipeline, exposed as a single call.

```
POST /v1/validate
{
  "source": "fn main() {\n  let v: Vector(f64, 3) = [1.0, 2.0, 3.0]\n
              print(dot(v, v))\n}",
  "checks": ["lex", "parse", "typecheck", "ownership"]
}
```

Response (200 OK):
```
{
  "valid": true,
  "diagnostics": [],
  "ast_hash": "sha256:a1b2..."    // content-addressed source hash
                                   // (goal.md row 10 foundation)
}
```

Response (422 Unprocessable):
```
{
  "valid": false,
  "diagnostics": [
    {
      "stage": "type",
      "diagnostic": {
        "kind": "ShapeMismatch",
        "span": { "line": 3, "col": 10 },
        "message": "Vector(f64, 3) dot Vector(f64, 4): inner dims
                    don't match (3 vs 4)"
      }
    }
  ],
  "ast_hash": null
}
```

WHY THIS MATTERS: the diagnostics are structured JSON (already
shipped via --format=json). An LLM can parse them programmatically
and attempt a targeted fix, not a blind retry. The `kind` field is
a stable enum variant (TypeErrorKind, OwnershipErrorKind) -- the
LLM can learn "ShapeMismatch means I got the dimensions wrong" and
fix exactly that.

================ A3. POST /v1/validate-fragment ===========

Validate a single expression against an expected type and a
caller-supplied variable environment. This is the load-bearing
piece for row 9 (AI as first-class citizen): agents emit typed AST
fragments that the compiler validates before splicing.

Wraps: `typeck::validate_fragment(json, expected_ty, env)`.

```
POST /v1/validate-fragment
{
  "fragment_json": "{\"Float\": [3.14, {\"line\":1,\"col\":1}]}",
  "expected_type": "f64",
  "environment": [
    { "name": "v", "type": "Vector(f64, 3)" },
    { "name": "n", "type": "i64" }
  ]
}
```

Response (200 OK):
```
{
  "valid": true,
  "inferred_type": "f64",
  "diagnostics": []
}
```

Response (422):
```
{
  "valid": false,
  "inferred_type": null,
  "diagnostics": [
    { "stage": "type", "diagnostic": {
        "kind": "TypeMismatch",
        "span": { "line": 1, "col": 1 },
        "message": "expected f64, got Vector(f64, 3)"
    }}
  ]
}
```

USE CASE: An agent is editing a function body. It has:
  let result: f64 = <HOLE>
The agent generates a candidate for <HOLE>, sends it here with
expected_type=f64 and the variables in scope. If it type-checks,
the agent splices it in. If not, it gets a structured error telling
it exactly what went wrong -- "you returned a Vector where an f64
was expected" -- and can fix just that.

SCOPE LIMIT (stated honestly): validate_fragment checks types only,
not ownership. The move-checker needs a whole function's control
flow, which a fragment in isolation doesn't have. This is documented
in the compiler source (typeck.rs:1676) and is not a bug.

================ A4. POST /v1/repair =====================

The self-repair loop, packaged as one call.

Given a natural-language task description and a generation backend,
this endpoint:
  1. Generates Nirdosha code from the prompt
  2. Validates it (typecheck + ownership)
  3. If invalid, feeds the structured diagnostics back to the LLM
     as a repair prompt
  4. Repeats up to max_attempts times
  5. Returns the final code + the full repair trace

THIS IS THE HARD THING IT MAKES EASY:
  Self-repair loops are the single biggest quality lever in
  LLM code generation (measured by every benchmark from HumanEval
  to SWE-bench). But building one requires: a structured error
  format, a retry loop, a prompt-construction step, and a validation
  step -- all custom code. This endpoint does all of it.

```
POST /v1/repair
{
  "task": "Write a Kalman filter predict step for a 3-state system
   using kf_predict_state and kf_predict_cov. The state vector x
   is Vector(f64, 3), covariance P is Matrix(f64, 3, 3), state
   transition F is Matrix(f64, 3, 3), process noise Q is
   Matrix(f64, 3, 3).",

  "generation": {
    "model_endpoint": "http://localhost:8080/v1",
    "model_name": "qwen2.5-coder-7b",
    "temperature": 0.0,
    "max_tokens": 512,
    "grammar_constraint": true     // use GBNF if available
  },

  "max_attempts": 5,
  "repair_strategy": "diagnostic_feed",  // diagnostic_feed | resample
  "validate": true,                       // typecheck + ownership
  "execute": false,                       // also run it?
  "expected_output": "..."                // if execute=true, check output
}
```

Response (200 OK):
```
{
  "success": true,
  "attempts_used": 2,
  "code": "fn kf_predict_step(x: Vector(f64, 3), P: Matrix(f64, 3, 3),
              F: Matrix(f64, 3, 3), Q: Matrix(f64, 3, 3)) -> ...",
  "repair_trace": [
    {
      "attempt": 1,
      "code": "fn kf_predict_step(x, P, F, Q) { ... }",
      "diagnostics": [
        { "stage": "type", "diagnostic": {
            "kind": "MissingTypeAnnotation",
            "message": "parameter 'x' needs a type"
        }}
      ],
      "repair_prompt": "Your previous code had these errors:
        [MissingTypeAnnotation at 1:11: parameter 'x' needs a type]
        Fix them and regenerate."
    },
    {
      "attempt": 2,
      "code": "fn kf_predict_step(x: Vector(f64, 3), ...) { ... }",
      "diagnostics": [],
      "repair_prompt": null
    }
  ],
  "execution_result": null,
  "total_latency_ms": 2100
}
```

If all attempts fail:
```
{
  "success": false,
  "attempts_used": 5,
  "code": "<last attempt>",
  "repair_trace": [ ... all 5 attempts ... ],
  "error": "REPAIR_EXHAUSTED",
  "final_diagnostics": [ ... ]
}
```

repair_strategy options:
  "diagnostic_feed" -- feed structured diagnostics as context for
    the next attempt (default, most effective)
  "resample" -- just regenerate with temperature > 0, no diagnostic
    context (baseline for comparison)

================ A5. POST /v1/splice ======================

Take a validated fragment and splice it into a program at a
marked location. Returns the modified program source.

```
POST /v1/splice
{
  "program_source": "fn main() {\n  let v: Vector(f64, 3) = [1.0, 2.0, 3.0]\n  let result: f64 = <HOLE>\n  print(result)\n}",
  "hole_marker": "<HOLE>",
  "fragment_json": "{\"Call\": [\"dot\", [{\"Var\": \"v\"}, {\"Var\": \"v\"}]]}",
  "expected_type": "f64",
  "validate_after_splice": true
}
```

Response (200 OK):
```
{
  "spliced_source": "fn main() {\n  let v: Vector(f64, 3) = [1.0, 2.0, 3.0]\n  let result: f64 = dot(v, v)\n  print(result)\n}",
  "valid": true,
  "diagnostics": []
}
```

This is the "incremental improvement" primitive: an agent doesn't
need to regenerate an entire function to fix one expression -- it
generates just the fragment, validates it in context, and splices.
This is cheaper (fewer tokens), safer (only the changed part is
untrusted), and more auditable (the diff is one expression).

------------------------------------------------------------------------
B. EXECUTION & SIMULATION
------------------------------------------------------------------------

================ B1. POST /v1/run ========================

Run a Nirdosha program via the tree-walking interpreter.
Wraps: `nirdosha::run(src)` or `nirdosha::run_diagnostic(src)`.

```
POST /v1/run
{
  "source": "fn main() {\n  print(42)\n}",
  "format": "json"|"text",      // json = structured diagnostics on error
  "timeout_ms": 5000
}
```

Response (200 OK):
```
{
  "output": "42\n",
  "exit_code": 0,
  "diagnostics": []
}
```

Response (200 OK, runtime error with format=json):
```
{
  "output": "",
  "exit_code": 1,
  "diagnostics": [
    { "stage": "runtime", "diagnostic": {
        "kind": "SingularMatrix",
        "span": { "line": 3, "col": 12 },
        "message": "matrix is singular; cannot invert"
    }}
  ]
}
```

================ B2. POST /v1/run-sandboxed ===============

Run a Nirdosha program inside an isolated OS process.
Uses the existing `sandbox`/`stop` primitives (shipped in commit
579c1bc, SANDBOXING.md layer 1) to fork a child process that
re-execs the nirdosha binary.

THIS IS THE HARD THING IT MAKES EASY:
  Running LLM-generated code safely is the #1 blocker for autonomous
  coding agents in production. Docker is heavy and doesn't prevent
  resource exhaustion. Nirdosha's sandbox is a real OS process with
  the language's ownership guarantees preventing shared-state races.

```
POST /v1/run-sandboxed
{
  "source": "fn main() {\n  rand_seed(42)\n  let x: f64 = rand_gaussian(0.0, 1.0)\n  print(x)\n}",
  "timeout_ms": 10000,
  "memory_limit_mb": 256,        // kill if exceeded
  "cpu_limit_seconds": 5,        // kill if exceeded
  "network_enabled": false,      // sandbox has no network by default
  "format": "json"
}
```

Response (200 OK):
```
{
  "output": "0.123456789\n",
  "exit_code": 0,
  "sandbox_pid": 12345,          // (already terminated)
  "diagnostics": [],
  "resource_usage": {
    "max_memory_mb": 12,
    "cpu_time_ms": 340,
    "wall_time_ms": 350
  }
}
```

If the sandbox was killed:
```
{
  "output": "",
  "exit_code": 137,              // SIGKILL
  "diagnostics": [],
  "killed_reason": "timeout"|"oom"|"cpu_limit",
  "resource_usage": { ... }
}
```

================ B3. POST /v1/run-deterministic ===========

Run a Nirdosha program with enforced determinism guarantees.
This is the mission-critical execution path (goal.md row 10,
Phase 3 §4.3.1).

```
POST /v1/run-deterministic
{
  "source": "fn main() {\n  rand_seed(42)\n  let x: f64 = rand_gaussian(0.0, 1.0)\n  print(x)\n}",
  "seed": 42,                    // required -- must match rand_seed in source
  "expected_output_hash": "sha256:...",  // if provided, verify output matches
  "timeout_ms": 10000
}
```

Response (200 OK):
```
{
  "output": "0.123456789\n",
  "output_hash": "sha256:abcdef...",
  "deterministic": true,         // confirmed: no nondeterminism detected
  "matches_expected": true,      // if expected_output_hash was provided
  "exit_code": 0
}
```

If nondeterminism is detected (e.g. the program uses an unseeded
spawn that reads OS entropy -- currently an honest gap per
LANGUAGE.md §9):
```
{
  "output": "...",
  "output_hash": "sha256:...",
  "deterministic": false,
  "nondeterminism_sources": [
    { "source": "unseeded_spawn", "location": { "line": 5, "col": 3 },
      "message": "spawned function gets an independent unseeded RNG;
                  its output is not reproducible from the seed alone." }
  ],
  "exit_code": 0
}
```

THIS IS THE HARD THING IT MAKES EASY:
  Defense simulations, Monte Carlo runs, and any auditable numerical
  computation require byte-for-byte reproducibility. Today, achieving
  that means fighting every source of nondeterminism in your runtime
  (thread scheduling, OS entropy, floating-point reduction order).
  Nirdosha's runtime has exactly one nondeterminism source (unseeded
  spawn), and it's documented. This endpoint makes the guarantee
  checkable, not just hoped-for.

================ B4. POST /v1/build =======================

Compile a Nirdosha program to a native binary via LLVM.
Wraps: `nirdosha::codegen::build(program, output_path)`.

```
POST /v1/build
{
  "source": "fn fib(n: i64) -> i64 { ... } fn main() { print(fib(30)) }",
  "optimization": "O0"|"O1"|"O2",   // default O2
  "output_format": "binary"|"llvm-ir",  // binary = .o, llvm-ir = .ll
  "binary_hash": true                  // return sha256 of the output
}
```

Response (200 OK):
```
{
  "binary_path": "/tmp/nirdosha-build-abc123",
  "binary_hash": "sha256:deadbeef...",
  "llvm_ir_path": "/tmp/nirdosha-build-abc123.ll",
  "compile_time_ms": 1200,
  "unsupported_features": []          // empty if all compiled
}
```

If the source uses interpreter-only features (Vector, Matrix, etc.):
```
{
  "binary_path": null,
  "error": "UNSUPPORTED_FEATURES",
  "unsupported_features": [
    { "feature": "Vector", "location": { "line": 2, "col": 10 },
      "reason": "Vector/Matrix codegen is Phase 4, not yet implemented" }
  ]
}
```

NOTE: this is the honest behavior. codegen.rs already rejects
unsupported features via `check_supported` with a specific reason,
never silently mis-compiling. This endpoint surfaces that as
structured JSON.

------------------------------------------------------------------------
C. COMPILER INTROSPECTION
------------------------------------------------------------------------

These endpoints expose the compiler's internal knowledge so an
LLM agent can query "what types exist?", "what builtins can I
call?", "what's the grammar?" without reading the source code.

================ C1. GET /v1/grammar =====================

Returns the GBNF grammar artifact for constrained decoding.

```
GET /v1/grammar?format=gbnf
```

Response (200 OK, text/plain):
```
root ::= stmt*
stmt ::= "fn" ident "(" params ")" ("->" type)? block
        | "let" ident ":" type "=" expr
        | "return" expr?
        | "if" expr block ("else" block)?
        | "while" expr block
        | "audited" string block
        ...
```

DEPENDENCY: Phase 2 §4.2.3 (GBNF export). Until that ships, this
returns 501 with a message pointing to the phase. The hand-written
LL(1) parser is already verified unambiguous (GRAMMAR.md row 7),
so the export is a translation, not a research problem.

================ C2. GET /v1/types ========================

Returns all types the compiler knows about, with their properties.
An LLM agent uses this to know what it can declare.

```
GET /v1/types
```

Response (200 OK):
```
{
  "types": [
    { "name": "i8", "category": "integer", "affine": false,
      "compiled": true, "bits": 8 },
    { "name": "i64", "category": "integer", "affine": false,
      "compiled": true, "bits": 64 },
    { "name": "f64", "category": "float", "affine": false,
      "compiled": true, "bits": 64,
      "notes": "IEEE 754 double. Saturates to inf/NaN, never traps." },
    { "name": "bool", "category": "boolean", "affine": false,
      "compiled": true },
    { "name": "str", "category": "string", "affine": false,
      "compiled": false,
      "notes": "UTF-8, Arc<str>-backed. Literals + escapes only." },
    { "name": "box T", "category": "heap", "affine": true,
      "compiled": false },
    { "name": "Vector(T, N)", "category": "array", "affine": false,
      "compiled": false,
      "notes": "Fixed-length. N is a compile-time literal." },
    { "name": "Matrix(T, R, C)", "category": "array", "affine": false,
      "compiled": false,
      "notes": "Fixed-shape, row-major. R/C are compile-time literals." },
    { "name": "chan T", "category": "channel", "affine": false,
      "compiled": false,
      "notes": "Unbounded MPMC. Handle is copyable, payload moves." },
    { "name": "sandbox", "category": "process", "affine": true,
      "compiled": false },
    { "name": "tcp", "category": "network", "affine": true,
      "compiled": false },
    ...
  ]
}
```

================ C3. GET /v1/builtins =====================

Returns all registered builtins with their type signatures.
An LLM agent uses this to know what functions it can call and
what arguments they expect.

```
GET /v1/builtins
```

Response (200 OK):
```
{
  "builtins": [
    { "name": "print", "signature": "print(..args) -> unit",
      "category": "io", "compiled": "partial",
      "notes": "bool literal can't be printed when compiled" },

    { "name": "dot", "signature": "dot(a: Vector(T, N), b: Vector(T, N)) -> T",
      "category": "linalg", "compiled": false,
      "constraint": "same length, numeric element" },

    { "name": "det", "signature": "det(m: Matrix(f64, N, N)) -> f64",
      "category": "linalg", "compiled": false,
      "constraint": "square matrix only",
      "errors": ["SingularMatrix"] },

    { "name": "solve", "signature": "solve(a: Matrix(f64, N, N), b: Vector(f64, N)) -> Vector(f64, N)",
      "category": "linalg", "compiled": false,
      "errors": ["SingularMatrix"] },

    { "name": "rand_seed", "signature": "rand_seed(seed: <int>) -> unit",
      "category": "simulation", "compiled": false,
      "notes": "Resets interpreter RNG. Required before any draw." },

    { "name": "rand_gaussian",
      "signature": "rand_gaussian(mean: f64, stddev: f64) -> f64",
      "category": "simulation", "compiled": false,
      "notes": "Box-Muller. Deterministic from seed." },

    { "name": "kf_predict_state",
      "signature": "kf_predict_state(x: Vector, P: Matrix, F: Matrix, Q: Matrix) -> Vector",
      "category": "simulation", "compiled": false,
      "notes": "Linear Kalman filter predict step." },

    { "name": "lla_to_ecef",
      "signature": "lla_to_ecef(v: Vector(f64, 3)) -> Vector(f64, 3)",
      "category": "geometry", "compiled": false,
      "notes": "WGS84 lat/lon/alt to Earth-centered Earth-fixed." },

    ...
  ]
}
```

WHY THIS MATTERS: an LLM writing Nirdosha code needs to know the
exact builtin names, argument types, and constraints. Today, the
only way to learn this is reading LANGUAGE.md. This endpoint makes
it machine-queryable -- an agent can call this before generating
code, or a tool can inject the relevant builtins into the prompt
automatically.

================ C4. POST /v1/emit-ast ====================

Serialize a program's AST as JSON. Wraps the existing `--emit-ast`
CLI flag.

```
POST /v1/emit-ast
{
  "source": "fn main() { print(42) }"
}
```

Response (200 OK):
```
{
  "ast": {
    "Function": {
      "name": "main",
      "params": [],
      "ret_ty": null,
      "body": [
        { "Expr": {
            "Call": ["print", [{ "Int": [42, {"line":1,"col":15}] }]]
        }}
      ],
      "span": { "line": 1, "col": 1 }
    }
  }
}
```

The AST JSON is the same Serialize-derived shape that
validate_fragment's input expects -- so an agent can:
  1. emit-ast on an existing program
  2. modify a node in the JSON
  3. validate-fragment the modified node
  4. splice it back in

This is the full round-trip for agent-driven code editing, using
only the compiler's existing serialization.

------------------------------------------------------------------------
D. BENCHMARKING & EVALUATION
------------------------------------------------------------------------

================ D1. POST /v1/bench/run ==================

Run pass@1 evaluation: for each task in the corpus, generate
Nirdosha code, validate it, run it, check the output.

```
POST /v1/bench/run
{
  "corpus_id": "default",         // or a custom corpus name
  "model_endpoint": "http://localhost:8080/v1",
  "model_name": "qwen2.5-coder-7b",
  "grammar_constraint": true,     // use GBNF if available
  "tasks": null,                  // null = all tasks; or ["fib", "matmul3"]
  "pass_k": 1,                    // pass@1 (k=1), pass@5 (k=5, generate 5x)
  "temperature": 0.0,             // for pass@1; 0.7 for pass@5
  "timeout_per_task_ms": 10000
}
```

Response (200 OK):
```
{
  "results": [
    {
      "task_id": "fib",
      "prompt": "Write a recursive Fibonacci function in Nirdosha...",
      "generated_code": "fn fib(n: i64) -> i64 { ... }",
      "passed": true,
      "parse_ok": true,
      "typecheck_ok": true,
      "ownership_ok": true,
      "execution_ok": true,
      "output_matches": true,
      "latency_ms": 1200,
      "tokens_generated": 45
    },
    {
      "task_id": "matmul3",
      "prompt": "Write a function that multiplies two 3x3 matrices...",
      "generated_code": "fn matmul3(a: Matrix(f64,3,3), b: Matrix(f64,3,3)) -> ...",
      "passed": false,
      "parse_ok": true,
      "typecheck_ok": false,
      "ownership_ok": null,       // not reached
      "execution_ok": null,
      "output_matches": null,
      "diagnostics": [
        { "stage": "type", "diagnostic": {
            "kind": "ShapeMismatch",
            "message": "Matrix(f64,3,3) * Matrix(f64,4,4): inner dims
                        don't match (3 vs 4)"
        }}
      ],
      "latency_ms": 800,
      "tokens_generated": 38
    }
  ],
  "summary": {
    "total": 20,
    "passed": 14,
    "pass_at_1": 0.70,
    "parse_rate": 1.0,            // 100% parsed (grammar helps!)
    "typecheck_rate": 0.85,
    "execution_rate": 0.75,
    "avg_latency_ms": 950
  }
}
```

DEPENDENCY: bench/corpus.json already exists (shipped). The harness
plumbing (corpus format, scoring script) is Phase 5 §4.5.2. This
endpoint wraps that harness as an HTTP call.

================ D2. POST /v1/bench/repair-rate ============

Measure self-repair rate: for each failing task from a bench/run,
feed structured diagnostics back and retry up to N times.

```
POST /v1/bench/repair-rate
{
  "corpus_id": "default",
  "model_endpoint": "http://localhost:8080/v1",
  "model_name": "qwen2.5-coder-7b",
  "max_repair_attempts": 3,
  "tasks": null,
  "temperature": 0.0
}
```

Response (200 OK):
```
{
  "results": [
    {
      "task_id": "matmul3",
      "attempts": [
        { "attempt": 1, "passed": false,
          "diagnostics": [{"ShapeMismatch": "..."}] },
        { "attempt": 2, "passed": false,
          "diagnostics": [{"ShapeMismatch": "..."}] },
        { "attempt": 3, "passed": true,
          "diagnostics": [] }
      ],
      "final_passed": true,
      "attempts_to_success": 3
    }
  ],
  "summary": {
    "initial_pass_rate": 0.70,      // pass@1 before repair
    "final_pass_rate": 0.90,        // after up to 3 repair attempts
    "repair_rate": 0.667,           // (final - initial) / (1 - initial)
                                    // 67% of failures were repairable
    "avg_attempts_to_success": 1.8,
    "unrepairable_tasks": ["sensor_fusion_kf"]
  }
}
```

WHY THIS MATTERS: self-repair rate is the metric that tells you
whether your structured diagnostics are actually helping the LLM
fix its own mistakes, vs. just resampling blindly. A repair_rate
of 0.67 with diagnostic_feed vs. 0.20 with resample proves the
structured diagnostics are doing real work.

================ D3. GET /v1/bench/corpus ================

List the benchmark corpus.

```
GET /v1/bench/corpus
```

Response (200 OK):
```
{
  "corpus": [
    {
      "task_id": "fib",
      "category": "recursion",
      "prompt": "Write a recursive Fibonacci function...",
      "expected_output": "55\n",
      "difficulty": "easy",
      "features_tested": ["recursion", "i64", "if-else"]
    },
    {
      "task_id": "matmul3",
      "category": "linalg",
      "prompt": "Write a function that multiplies two 3x3 matrices...",
      "expected_output": "...",
      "difficulty": "medium",
      "features_tested": ["Matrix", "operators", "shape-checking"]
    },
    {
      "task_id": "kf_predict",
      "category": "simulation",
      "prompt": "Write a Kalman filter predict step...",
      "expected_output": "...",
      "difficulty": "hard",
      "features_tested": ["kf_predict_state", "kf_predict_cov",
                          "Vector", "Matrix"]
    }
  ]
}
```

================ D4. POST /v1/bench/corpus ===============

Add a task to the benchmark corpus.

```
POST /v1/bench/corpus
{
  "task_id": "ecef_to_enu",
  "category": "geometry",
  "prompt": "Write a function that converts ECEF coordinates to ENU
   relative to a reference point. Use the ecef_to_enu builtin.",
  "expected_output": "...",
  "difficulty": "medium",
  "features_tested": ["ecef_to_enu", "Vector(f64,3)"],
  "source_code": null,          // if the task includes starter code
  "seed": 42                     // for deterministic tasks
}
```

------------------------------------------------------------------------
E. PROVENANCE & REPRODIBILITY
------------------------------------------------------------------------

These endpoints wrap the reproducibility infrastructure that
goal.md row 10 describes. The honest status (from goal.md's own
correction): capability.rs/ledger.rs do not exist yet. The
concrete, checkable foundation is seed-based determinism (Phase 3)
and content-addressed source hashing.

================ E1. POST /v1/provenance/hash ============

Content-hash a Nirdosha source file. This is the foundation for
tamper-evidence (goal.md row 10): if the source changes, the hash
changes, and a binary built from a different hash is detectable.

```
POST /v1/provenance/hash
{
  "source": "fn main() { print(42) }",
  "hash_algorithm": "sha256"
}
```

Response (200 OK):
```
{
  "source_hash": "sha256:abcdef...",
  "ast_hash": "sha256:123456...",   // hash of the parsed AST
                                     // (stable across whitespace changes)
  "byte_count": 24,
  "line_count": 1
}
```

Two hashes because they catch different things:
  - source_hash: catches any byte change (including comments, whitespace)
  - ast_hash: catches semantic changes only (whitespace-insensitive)

A binary's provenance chain is: source_hash → ast_hash → compile flags →
binary_hash. If any link changes, the binary is not from the attested
source.

================ E2. POST /v1/provenance/verify ==========

Verify that a binary was produced from attested source.

```
POST /v1/provenance/verify
{
  "binary_hash": "sha256:deadbeef...",
  "expected_source_hash": "sha256:abcdef...",
  "compile_flags": {
    "optimization": "O2"
  },
  "expected_binary_hash": "sha256:deadbeef..."
}
```

Response (200 OK):
```
{
  "verified": true,
  "source_hash": "sha256:abcdef...",
  "binary_hash": "sha256:deadbeef...",
  "compile_flags": { "optimization": "O2" },
  "matches_expected": true
}
```

Response (200 OK, mismatch):
```
{
  "verified": false,
  "source_hash": "sha256:abcdef...",
  "binary_hash": "sha256:cafebabe...",
  "expected_binary_hash": "sha256:deadbeef...",
  "matches_expected": false,
  "message": "Binary hash does not match expected. Either the source
    changed, the compile flags changed, or the binary was not produced
    by the attested compiler."
}
```

DEPENDENCY: This requires reproducible builds (deterministic compiler
output). Nirdosha's codegen is LLVM-based, and LLVM's determinism is
well-studied but not guaranteed without specific flags. This endpoint
will be fully functional after Phase 5 §4.5.3. Until then, it returns
the hashes but marks verified=false with a note that reproducible
build verification is not yet implemented.

================ E3. GET /v1/provenance/audit =============

Get the audit trail for a deterministic run.

```
GET /v1/provenance/audit?run_id=run-abc123
```

Response (200 OK):
```
{
  "run_id": "run-abc123",
  "source_hash": "sha256:abcdef...",
  "ast_hash": "sha256:123456...",
  "binary_hash": "sha256:deadbeef...",  // if compiled
  "seed": 42,
  "output_hash": "sha256:fedcba...",
  "started_at": "2026-08-21T12:00:00Z",
  "ended_at": "2026-08-21T12:00:05Z",
  "nondeterminism_sources": [],
  "sandboxed": true,
  "resource_usage": {
    "max_memory_mb": 12,
    "cpu_time_ms": 340
  }
}
```

The audit trail is the reproducibility guarantee made checkable:
  same source_hash + same seed + same binary_hash
    → same output_hash
  If output_hash doesn't match, something in the chain broke.

------------------------------------------------------------------------
ERROR CODES
------------------------------------------------------------------------

  ERR_PARSE_FAILURE         -- source doesn't parse
  ERR_TYPE_CHECK_FAILURE    -- typecheck errors (diagnostics included)
  ERR_OWNERSHIP_FAILURE     -- move-checker errors (diagnostics included)
  ERR_RUNTIME_FAILURE       -- runtime error (diagnostics included)
  ERR_UNSUPPORTED_FEATURE   -- codegen doesn't support this feature yet
  ERR_GRAMMAR_NOT_AVAILABLE -- GBNF export not yet shipped (Phase 2)
  ERR_TIMEOUT               -- execution exceeded timeout
  ERR_SANDBOX_KILLED        -- sandbox was killed (oom/cpu/timeout)
  ERR_REPAIR_EXHAUSTED      -- all repair attempts failed
  ERR_MODEL_ENDPOINT_UNREACHABLE
  ERR_CORPUS_TASK_NOT_FOUND
  ERR_PROVENANCE_MISMATCH   -- binary doesn't match attested source
  ERR_NONDETERMINISM        -- run produced nondeterministic output

All errors include the structured diagnostics from the compiler
where applicable -- the same Diagnostic JSON shape that
--format=json produces.

------------------------------------------------------------------------
WHAT THIS IS NOT
------------------------------------------------------------------------

This is not a training API. Nirdosha is a language, not a model
training platform. The APIs here make the things that are hard
about USING LLMs easier -- specifically, using LLMs to generate,
validate, repair, and run verified numerical code in a
mission-critical context.

The APIs that would make TRAINING and DISTILLATION easier are a
different project. What this API set does is close the loop
between an LLM and a compiler that can actually prove things about
the code the LLM writes -- which is the gap nobody has shipped yet.

------------------------------------------------------------------------
IMPLEMENTATION NOTES
------------------------------------------------------------------------

The server is a thin HTTP wrapper. It links the compiler crate
as a library (compiler/src/lib.rs already exposes run, run_diagnostic,
validate_fragment, Diagnostic, RunFailure). The additional work:

  1. HTTP server (axum or actix-web, ~200 lines of route handlers)
  2. Grammar-constrained decoding integration (calls an external
     OpenAI-compatible endpoint with the GBNF grammar -- Phase 2 dep)
  3. Benchmark harness scoring loop (Phase 5 scaffold exists in bench/)
  4. Provenance hashing (sha256 of source + AST, straightforward)

No compiler internals change. The compiler already has every
function these APIs need. The server just calls them.

========================================================================