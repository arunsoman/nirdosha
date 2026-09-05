# Agent instructions — writing Nirdosha code

This file follows the `AGENTS.md` convention several agentic coding
tools read automatically (OpenAI Codex CLI, Amp, and others). Drop it
at the root of a project that uses Nirdosha and any tool that respects
this convention will pick it up without further setup.



# Writing Nirdosha (`.nir`) code

Nirdosha is a research-stage systems language with no garbage collector,
no data races, no deadlocks, and no integer/buffer overflow — a smaller,
LL(1) grammar designed to be easy for an LLM to write correctly, not
just easy for a human to read. This guide has the rules you need to
generate *valid* Nirdosha on the first try.

## The rules that will break your output if you get them wrong

1. **No `::` token exists anywhere in the lexer.** Enum variants are
   flat, unqualified calls: `Some(5)`, `None()`, `Circle(r)` — never
   `EnumName::Variant`. A zero-payload variant still needs `()` at the
   call site: `None()`, not bare `None`.
2. **`str` cannot be a function's parameter or return type** — checked
   recursively through `Result`/`Option`/generics/`box`/`&`/`thread`/
   `chan`/`Vector`/`Matrix`/`fn` types. Use a real `enum` for
   categorical data (a status, a currency code), or wrap free text in a
   one-field struct (`struct Text { value: str }`) if it must cross a
   function boundary. `str` is completely fine as a `struct` field, a
   local `let` binding, or a literal — the restriction is *only* at
   `fn` parameter/return position.
3. **`str` has zero concatenation, zero formatting, zero slicing.**
   Every string a program produces is either a literal from source or
   comes back from a builtin (`json_get_str`, `db_query`, `http_get`,
   ...). There is no `+` for strings and no f-string/format equivalent.
4. **No statement separator** — no semicolons, no significant
   newlines. Wherever a token could extend the current expression *or*
   start a new statement, the parser always extends. So:
   ```nirdosha
   return x
   -y
   ```
   parses as one statement, `return (x - y)`, **not** two statements.
   Put unrelated statements on lines that can't be read as a
   continuation of the previous one (this bites unary `-` and calls
   most often).
5. **No `for` loops, no closures/lambdas, no tuples.** Use `while` for
   iteration. Use a real `struct`/`enum` instead of a tuple. Plain
   first-class functions exist (`let f: fn(i64) -> i64 = double`) but
   capture nothing — there's no enclosing-scope capture at all.
6. **No implicit conversions, ever**, between two already-typed values
   — not even `i32` + `i64`. An integer *literal* flexes to fit its
   declared width (`let n: i8 = 100` needs no cast), but two typed
   variables never coerce. There is no int↔float conversion operator.
7. **Construction is an ordinary call.** A `struct`'s name is its own
   positional constructor: `Product(1, "Widget", 999)`, not
   `Product { id: 1, name: "Widget", price: 999 }`.
8. **`match` is exhaustive, no wildcard binding patterns for
   variants.** Every enum variant needs its own arm. A separate
   literal-pattern form exists for `str`/`i64`/`bool` scrutinees only,
   and *that* form requires a trailing `_ =>` wildcard arm (variant
   arms never use `_`, since coverage is checked by variant, not by
   value).
9. **A `match` arm's body must be a single expression — never a
   `{ statement; statement }` block.** This is the single most common
   mistake an LLM makes writing Nirdosha (it's valid in Rust, which is
   why the instinct is strong). `Ok(conn) => { let x = f(conn) stop(conn) x }`
   is a parse error (`expected an expression, found LBrace`), full
   stop, even though it looks completely reasonable. If an arm needs
   more than one step, do what every real Nirdosha program in this
   repo does: **extract a small helper function and call it as the
   arm's single expression** —
   ```nirdosha
   fn list_product_inner(conn: db) -> Result(json, ErrorCode) {
       let created: i64 = db_execute(conn, "CREATE TABLE IF NOT EXISTS product (...)")
       let rows: Result(json, ErrorCode) = match db_query(conn, "SELECT ...") {
           Ok(r) => Ok(r),
           Err(e) => Err(DbError(e)),
       }
       stop(conn)
       return rows
   }
   fn list_product() -> Result(json, ErrorCode) {
       return match db_connect("shop.db") {
           Ok(conn) => list_product_inner(conn),   // single expression: a call
           Err(e) => Err(DbError(e)),
       }
   }
   ```
   (`if`/`else` bodies are a genuine exception — those *are* real
   multi-statement blocks, and the whole `if`/`else` construct counts
   as one expression, so `Ok(x) => if cond { let a = 1 "yes" } else { "no" }`
   is valid. But reach for the helper-function split above by default
   — it's what every shipped example does, and it stays readable as
   the branch grows.)

   **The same rule rules out `return` inside a match arm too** — not
   just `{ }` blocks. `return` is a *statement* in Nirdosha's grammar
   (`return_stmt`), completely separate from `expr` — it can never
   appear anywhere an expression is required, match arms included. The
   early-return-on-error idiom that's completely normal in most
   languages —
   ```nirdosha
   // WRONG — "found Return" parse error, every time:
   let x: i64 = match may_fail(n) {
       Ok(v) => v,
       Err(e) => return Err(e),
   }
   ```
   has to become a plain value in the arm, with the match itself
   *becoming* the function's return (or the caller's `return match`,
   same helper-function pattern as above):
   ```nirdosha
   // RIGHT — the arm is a plain value; the match is the whole return
   enum MyError {
       Bad(str),
   }
   fn may_fail(n: i64) -> Result(i64, MyError) {
       return if n > 0 { Ok(n) } else { Err(Bad("bad")) }
   }
   fn f(n: i64) -> Result(i64, MyError) {
       return match may_fail(n) {
           Ok(v) => Ok(v),
           Err(e) => Err(e),
       }
   }
   ```
   (Note `Result(i64, MyError)`, not `Result(i64, str)` — a bare `str`
   in a `Result`'s error slot would itself violate rule 2, since the
   ban applies recursively through `Result`/`Option`. Use a real error
   `enum` there too, exactly like every builtin's own `Result(_, str)`
   signatures do *not* have to — that exemption is for builtins only,
   never for a `fn` you write yourself.)

   **A third idiom, for "run N independent steps against the same
   `conn`, fail if any of them failed"** (e.g. creating several tables
   during setup) — don't try to early-`return` out of the sequence
   (rule 9 above already rules that out); collect each step's result
   as a sentinel value, then do ONE combined check at the end:
   ```nirdosha
   let r1: i64 = match db_execute(conn, "CREATE TABLE IF NOT EXISTS a (...)") {
       Ok(n) => n,
       Err(e) => -1,
   }
   let r2: i64 = match db_execute(conn, "CREATE TABLE IF NOT EXISTS b (...)") {
       Ok(n) => n,
       Err(e) => -1,
   }
   let all_ok: bool = r1 >= 0 && r2 >= 0
   let result: Result(i64, ErrorCode) = if all_ok {
       Ok(r2)
   } else {
       Err(DbError("one or more setup statements failed"))
   }
   stop(conn)
   return result
   ```
   This runs every step even if an earlier one failed (true early-exit
   isn't expressible here), which is fine for idempotent DDL like
   `CREATE TABLE IF NOT EXISTS` — for steps with real side effects that
   must not run after a prior failure, guard each one with its own
   `if previous_ok { ... } else { -1 }` instead of a flat sequence.
10. **No compound assignment operators.** `total += i` is a parse
    error (`expected an expression, found Assign`) — there is no `+=`,
    `-=`, `*=`, `/=` at all. Write it out: `total = total + i`.
11. **Only `//` line comments exist — no `/* ... */` block comments at
    all.** The lexer doesn't recognize `/*` as the start of anything;
    a leading `/*` produces `parse error: expected 'fn', found Slash`
    (or similar, wherever it appears) because the parser just sees a
    stray `/` where a top-level item or expression was expected. Use
    `//` for every comment, including multi-line ones (one `//` per
    line — there's no multi-line comment syntax at all).

## Quick reference: wrong vs. right

A fast-scan companion to the rules above — every pair below is
verified against the real compiler, not hypothetical.

**Enum variant construction (rule 1)**
```nirdosha
// WRONG -- parse error: expected an expression, found Colon
let s: Shape = Shape::Circle(1.0)
// RIGHT
let s: Shape = Circle(1.0)
```

**`str` at a function boundary (rule 2)**
```nirdosha
// WRONG -- type error: parameter is (or contains) `str`
fn greet(name: str) -> str { return name }
// RIGHT -- wrap free text; use enum for closed vocabularies
struct Text { value: str }
fn greet(name: Text) -> i64 { return 1 }
```

**String concatenation (rule 3)**
```nirdosha
// WRONG -- no such operator; `str` has no `+`
let full: str = "Hello, " + name
// RIGHT -- there is no runtime string-building at all; only literals
// and builtin return values exist. Pass the pieces separately instead
// of trying to assemble one string, e.g. two bind params to a query
// rather than one concatenated SQL fragment.
```

**Statement continuation (rule 4)**
```nirdosha
// WRONG (probably not what you meant) -- parses as ONE statement:
// return (x - y), not two statements
return x
-y
// RIGHT -- make the second statement unambiguously not a continuation
return x
print(y)
```

**`for` loops / tuples (rule 5)**
```nirdosha
// WRONG -- no `for` keyword exists at all
for item in items { print(item) }
// RIGHT
let i: i64 = 0
while i < len(items) {
    print(i)
    i = i + 1
}
```

**Implicit conversion (rule 6)**
```nirdosha
// WRONG -- type error: expected `i32`, found `i64`
let a: i32 = 1
let b: i64 = 2
let c: i64 = a + b
// RIGHT -- match the declared widths; there is no cast operator either,
// so pick one width and use it consistently
let a: i64 = 1
let b: i64 = 2
let c: i64 = a + b
```

**Struct construction (rule 7)**
```nirdosha
// WRONG -- no struct-literal syntax exists
let p: Point = Point { x: 1, y: 2 }
// RIGHT -- construction is an ordinary positional call
let p: Point = Point(1, 2)
```

**`match` arm bodies (rule 9 — the single most common mistake)**
```nirdosha
// WRONG -- parse error: expected an expression, found LBrace
let x: i64 = match r {
    Ok(conn) => { let a = f(conn) stop(conn) a },
    Err(e) => -1,
}
// WRONG -- parse error: expected an expression, found Return
let x: i64 = match r {
    Ok(v) => v,
    Err(e) => return -1,
}
// RIGHT -- single expression per arm; factor multi-step logic into a
// helper function and call it as the arm's value
fn handle(conn: db) -> i64 {
    let a: i64 = f(conn)
    stop(conn)
    return a
}
let x: i64 = match r {
    Ok(conn) => handle(conn),
    Err(e) => -1,
}
```

**Compound assignment (rule 10)**
```nirdosha
// WRONG -- parse error: expected an expression, found Assign
total += i
// RIGHT
total = total + i
```

**Comments (rule 11)**
```nirdosha
// WRONG -- parse error: expected `fn`, found Slash
/* a block comment */
// RIGHT -- // is the only comment syntax, one per line
// a comment
```

**`db_query`'s array result (a silent *runtime* bug, not a compile
error — see "Common builtins" below for the full example)**
```nirdosha
// WRONG -- compiles fine, always returns Err (wrong shape)
json_get_i64(db_query_result, "price_cents")
// RIGHT -- unwrap row 0 first
json_array_get(db_query_result, 0)   // then json_get_i64 on THAT
```

## Types

| Type | Spelling | Notes |
|---|---|---|
| Signed/unsigned ints | `i8` `i16` `i32` `i64` `u8` `u16` `u32` `u64` `usize` | Range-checked at every `let`/return/assign. |
| Float | `f64` | Only width. No scientific-notation literals. |
| Boolean | `bool` | |
| Unit | `unit` | No literal syntax — only a function's implicit return. |
| String | `str` | UTF-8. See rules 2–3 above. |
| Heap cell | `box T` | Single-owner (affine) — using it by name moves it. `*expr` dereferences. |
| Borrow | `&T` | Read-only borrow of a plain identifier only (`&x`, never `&(x+1)`). |
| Thread handle | `thread T` | Affine. `spawn f(args)` returns it; `join(t)` consumes it once. |
| Channel | `chan T` | Handle is freely copyable; the *payload* moves through `send`. |
| Sandbox | `sandbox` | Affine. A real separate OS process; `stop` consumes it once. |
| `tcp` / `tcp_listener` | | Affine. Real sockets. |
| `file` | | Affine. `open(path, mode)`; `mode` is `"r"`, `"w"`, or `"a"`. |
| `db` | | Affine. `db_connect(conn_str) -> Result(db, str)`; `stop(conn)` closes it once. See the Ownership section below for a real sharp edge here. |
| `Vector(T, N)` / `Matrix(T, R, C)` | | `N`/`R`/`C` are compile-time literal ints. `Vector(f64,3) ≠ Vector(f64,4)` — different types. |
| `Option(T)` | `Some(x)` / `None()` | Prelude generic enum, always available. |
| `Result(T, E)` | `Ok(x)` / `Err(e)` | Prelude generic enum, always available. |

## Declarations

```nirdosha
fn name(param: Ty, ...) -> RetTy { ... }   // RetTy omitted => unit
let x: Ty = expr
x = expr                                    // reassignment, not a new binding
return expr

struct Product {
    id: i64,
    name: str,
    price_cents: i64,
}

enum Status {
    Pending,
    Approved,
    Rejected(str),        // a variant can carry payload types
}

fn describe(s: Status) -> Status {
    return match s {
        Pending => Approved(),         // still needs () even with no payload
        Approved => Approved(),
        Rejected(reason) => Rejected(reason),
    }
}

if cond { ... } else { ... }     // also usable as an expression: let x = if c {1} else {2}
while cond { ... }
```

Type parameters are concrete-per-instantiation, not monomorphized —
`Pair(i64, str)` and `Pair(f64, bool)` are different, unrelated types.
`struct`/`enum` field lists allow (but don't require) a trailing comma.

## Ownership

`box`/`thread`/`sandbox`/`tcp`/`tcp_listener`/`file`/`db` are
**affine** — using the binding by name moves it; a later use on the
same path is a compile error (checked statically, not at runtime).
`&x` borrows without moving. Everything else (`i64`, `str`, `bool`,
`struct`s of non-affine fields, etc.) is freely copyable.

**A sharp, non-obvious edge with `db` (and the other handle types):**
calling a *builtin* that takes a `conn: db` (`db_query`, `db_execute`)
does **not** consume it — you can call `db_query`/`db_execute` on the
same `conn` as many times as you need within one function, exactly
like every real example in this repo does (open once, run several
statements, `stop` once at the end). But passing `conn` into a
function *you define* **does** move it, ordinary affine-argument
rules — after `let x = my_helper(conn)`, that function's own `conn`
binding is gone, and a later `stop(conn)` in the *original* function is
a "use after moved" error.

Practical consequence: **don't split multi-step DB logic that shares
one connection across several of your own helper functions** — you'll
end up with no single place left that legitimately owns `conn` to
`stop` it. Keep every `db_query`/`db_execute` call for one connection
in the *same* function, and reach for `if`/`else` (whose blocks
support real multi-statement sequences — see rule 9) to express
branching logic inline, rather than factoring a branch out into a new
function just to dodge match's single-expression rule. Extracting a
helper is still fine and encouraged when that helper *doesn't* need
the connection (e.g. a function that only shapes a `json` result you
already fetched) — the danger is specifically in handing the `db`
handle itself across a function boundary more than once.

If you do need to hand a connection to a helper and get a value back
without immediately closing it, you can pass a borrow (`&db`) into a
function that only reads through it *indirectly* — but `db_query`/
`db_execute` themselves require an owned `db`, not `&db`, so a borrow
doesn't help for the common case of "call a builtin from inside a
helper." Default to keeping the whole sequence in one function.

**A second sharp edge: the ownership checker is not path-sensitive
across early `return` branches.** If you `stop(conn)` inside an `if`
guard that returns early, the checker treats `conn` as moved for
*every* lexically later use in the function — including code that only
runs on the branch where you *didn't* stop it. Verified minimal repro
that fails to compile:

```nirdosha
// WRONG — "ownership error: use of `conn` after it was moved",
// even though the fall-through path never took the `if bad` branch.
fn f(conn: db, bad: bool) -> i64 {
    if bad {
        stop(conn)
        return -1
    }
    let r: Result(i64, str) = db_execute(conn, "CREATE TABLE IF NOT EXISTS t (id INTEGER)")
    stop(conn)
    return 1
}
```

This shows up in practice as validation guards written as "if the
input is bad, close the connection and bail out early" — a pattern
that's natural to reach for but doesn't typecheck here. The fix: do
input validation that doesn't need `conn` at all *before* you ever call
`db_connect`, in its own function, so there's no `conn` in scope to
poison on the early-return path:

```nirdosha
// RIGHT — validation runs first, has no `conn`, so there's nothing
// for the checker to worry about on its own early-return path.
fn validate_order(o: Order) -> Result(i64, ErrorCode) {
    return if o.quantity <= 0 {
        Err(InvalidOrder("quantity must be positive"))
    } else {
        Ok(1)
    }
}

fn place_order_inner(conn: db, o: Order) -> Result(i64, ErrorCode) {
    // ... every db_query/db_execute call for this connection, one
    // `stop(conn)` right before the final `return` ...
}

fn place_order(o: Order) -> Result(i64, ErrorCode) {
    return match validate_order(o) {
        Ok(_) => match db_connect("app.db") {
            Ok(conn) => place_order_inner(conn, o),
            Err(e) => Err(DbError(e)),
        },
        Err(e) => Err(e),
    }
}
```

More generally: never write `stop(conn)` inside a conditional branch
unless that branch is the *only* remaining use of `conn` in the
function. If a function has any logic that can reject *without*
touching the database, push that logic out to a separate, `conn`-free
function and call it before `db_connect` — don't try to bail out of
the middle of a connection's lifetime.

## Concurrency & I/O

```nirdosha
let t: thread i64 = spawn compute(x)     // real OS thread
let result: i64 = join(t)                 // blocks, consumes the handle

let c: chan i64 = chan
send(c, 42)                                // never blocks
let v: i64 = recv(c)                       // blocks until a value arrives

let s: sandbox = sandbox worker(args)      // real separate OS process
let code: i64 = stop(s)                    // kills if still running

let conn: tcp = connect("host", 8080)
let l: tcp_listener = listen(8080)
let incoming: tcp = accept(l)              // blocks for next client
stop(conn)                                  // closes tcp or tcp_listener

let f: file = open("path.txt", "w")        // mode: "r" | "w" | "a"
send(f, "text")                             // write (reuses tcp's keyword)
let contents: str = recv(f)                 // read all available; "" at EOF
stop(f)
```

## Common builtins

**Database** (interpreter-only): `db_connect(conn_str: str) -> Result(db, str)`
(a bare path/`:memory:` = SQLite; `postgres://...` = real Postgres) ·
`db_query(conn, sql, ...up to 8 bind values) -> Result(json, str)` ·
`db_execute(conn, sql, ...binds) -> Result(i64, str)` (returns affected
rows) · `stop(conn)`. `?` placeholders only — `str` has no
concatenation, so binding is the *only* way to parameterize a query.

**`db_query`'s result is always a JSON *array* of row objects — even
when you expect exactly one row.** This compiles fine and fails
silently at runtime if you get it wrong: `json_get_i64(db_query_result,
"price_cents")` looks reasonable but is wrong, because
`db_query_result` is an array, not the row object itself — the call
just returns `Err` (not found), which usually surfaces as a mystery
sentinel-value failure two steps later, not a clear error at the call
site. Always unwrap the first row via `json_array_get(rows, 0)` before
reading a field with `json_get_*`, exactly like every `list_<struct>`
CRUD function in this guide does when it hands the raw array to a
caller, versus every "look up a single row's field" case, which needs
this extra step:
```nirdosha
// WRONG — silently always "not found," never a compile error
let price: i64 = match db_query(conn, "SELECT price_cents FROM item WHERE id = ?", id) {
    Ok(rows) => match json_get_i64(rows, "price_cents") {
        Ok(p) => p,
        Err(e) => -1,
    },
    Err(e) => -1,
}

// RIGHT — unwrap row 0 first
let price: i64 = match db_query(conn, "SELECT price_cents FROM item WHERE id = ?", id) {
    Ok(rows) => match json_array_get(rows, 0) {
        Ok(row) => match json_get_i64(row, "price_cents") {
            Ok(p) => p,
            Err(e) => -1,
        },
        Err(e) => -1,
    },
    Err(e) => -1,
}
```

**JSON**: `json_parse(s: str) -> Result(json, str)` ·
`json_get_str/i64/f64/bool(j: json, key: str) -> Result(T, str)` ·
`json_array_len(j: json) -> i64` · `json_array_get(j: json, i: i64) -> Result(json, str)`.

**HTTP** (client-only): `http_get(host: str, port: i64, path: str) -> Result(HttpResponse, str)` ·
`http_post(host: str, port: i64, path: str, body: str) -> Result(HttpResponse, str)` ·
`https_get`/`https_post` same shapes.

**Identity** (Row 12): `oidc_validate_token(token, expected_issuer, expected_audience, jwks_json: str) -> Result(VerifiedIdentity, str)` ·
`check_role(identity: VerifiedIdentity, role: str) -> Result(RoleView, str)` ·
`extract_claim(identity, name: str) -> Result(ClaimView, str)`.

**Privileged functions**: `fn transfer(amount: i64) -> i64 requires(role: "admin") { ... }`
gates the function's *value*, not just its behavior — you cannot call
it or take its value directly. The only way to get a callable value is
`acquire transfer(proof)` where `proof` is a `RoleView`/`ClaimView`
from `check_role`/`extract_claim`.

**Print**: `print(x)` — any number of args, any scalar type.

## A complete worked example

```nirdosha
enum ErrorCode {
    DbError(str),
}

struct Product {
    id: i64,
    name: str,
    price_cents: i64,
    stock: i64,
}

fn list_product_inner(conn: db) -> Result(json, ErrorCode) {
    let created: i64 = match db_execute(conn, "CREATE TABLE IF NOT EXISTS product (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, price_cents INTEGER, stock INTEGER)") {
        Ok(n) => n,
        Err(e) => -1,
    }
    let rows: Result(json, ErrorCode) = match db_query(conn, "SELECT id, name, price_cents, stock FROM product ORDER BY id") {
        Ok(r) => Ok(r),
        Err(e) => Err(DbError(e)),
    }
    stop(conn)
    return rows
}

fn list_product() -> Result(json, ErrorCode) {
    return match db_connect("store.db") {
        Ok(conn) => list_product_inner(conn),
        Err(e) => Err(DbError(e)),
    }
}

fn create_product(p: Product) -> Result(i64, ErrorCode) requires(role: "admin") {
    return match db_connect("store.db") {
        Ok(conn) => match db_execute(conn, "INSERT INTO product (name, price_cents, stock) VALUES (?, ?, ?)", p.name, p.price_cents, p.stock) {
            Ok(n) => Ok(n),
            Err(e) => Err(DbError(e)),
        },
        Err(e) => Err(DbError(e)),
    }
}

fn main() {
    print("ready")
}
```

Naming `list_<struct>`/`create_<struct>`/`update_<struct>`/
`delete_<struct>` functions like this is also what `nirdosha emit-ui`/
`nirdosha serve` use to auto-generate a full CRUD web UI with zero
extra syntax — see the `screen`/`dashboard` DSL in `docs/LANGUAGE.md` §11 if
you need to customize that generated UI (custom labels, field
validation, role-gated visibility, dashboard tiles/charts).

Multi-step approval / state-machine flows (KYC onboarding, purchase
approvals, maker-checker) have their own construct: `workflow Name {
data { field: Ty, ... } state Name { on_entry { ... } on Event ->
Target } ... }` — durable, named states with `on <Event> -> <Target>`
transitions, desugared into ordinary `fn`s (`start_<name>`,
`advance_<name>`, etc.), so it needs no new runtime. `state { owner:
role("...") }` names who may fire that state's outgoing events —
checked live, per instance, not statically — and `nirdosha serve`/
`emit-ui` generate a "Workflows" queue screen from it automatically
(each role sees only what's waiting on them, plus a "my requests" tab
for whoever started an instance and an audit-trail "history" view), no
extra syntax needed. See `docs/WORKFLOW.md` for the full construct.

## How to verify what you wrote

**If you're pasting this into a file by hand (the paste-anywhere-prompt
workflow): strip the surrounding ` ```nirdosha ` / ` ``` ` markdown
fence before saving.** This is an extremely common, extremely basic
mistake — the fence isn't Nirdosha syntax, and leaving it in produces
`lex error at 1:1: unexpected character` (or similar) on the very
first line, which can look confusingly unrelated to "I forgot to
delete two lines." Save only what's between the fences as the `.nir`
file's actual content.

If you have shell access to a machine with `nirdosha` installed
(`curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/kannamma-labs/nirdosha/main/scripts/install.sh | sh`),
verify before presenting code as final:

```sh
nirdosha emit-ui file.nir -o /tmp/out.html   # full typecheck + ownership check, no side effects (doesn't run main())
nirdosha file.nir --format=json              # actually runs it; structured Diagnostic JSON on any failure
```

**`nirdosha emit-ast file.nir` does *not* typecheck** — it only
lexes/parses, by deliberate design (so a program that doesn't yet
typecheck can still be inspected). A file that passes `emit-ast` can
still be full of type errors — don't treat a clean `emit-ast` as "this
compiles." Use `emit-ui` for a real typecheck-only pass, or just run it
with `--format=json` if side effects (a real DB write, a real HTTP
call) are acceptable for this check.

A `Diagnostic` (from `--format=json`) or a `type error: ...` line (from
`emit-ui`) names the exact rule violated — read it and fix the named
issue rather than guessing. If you don't have shell access (a plain
chat interface), self-check your output line-by-line against the 9
rules above before presenting it, and say plainly that it hasn't been
run through the real compiler.

## Where to go deeper

Full type/builtin reference: `docs/LANGUAGE.md`. Full EBNF grammar:
`docs/GRAMMAR.md`. `workflow`/state-ownership construct: `docs/WORKFLOW.md`.
Worked examples: `examples/*.nir` in the main repo
(https://github.com/kannamma-labs/nirdosha).

## Verification is not optional

Before telling the user code is finished, actually run it through the
compiler (`nirdosha emit-ui file.nir -o /tmp/out.html` for a
typecheck-only pass, then `nirdosha file.nir --format=json` to run it)
if you have shell access. A `Diagnostic`/`type error` names the exact
rule violated — fix that, don't guess. Do not rely on `nirdosha
emit-ast` as a typecheck signal — it only parses (see above).
