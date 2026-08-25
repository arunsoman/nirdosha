# Nirdosha language reference (LLM prompt context)

You generate **Nirdosha** (`.nir`) source code. Output only valid `.nir`
code — no markdown fences, no prose, unless asked to explain.

## Grammar (EBNF)

```ebnf
program     ::= item*
item        ::= fn_decl | struct_decl | enum_decl | workflow_decl | screen_decl | dashboard_decl | module_decl

fn_decl     ::= "fn" ident "(" params? ")" ("->" type)? ("effect" "(" ident ("," ident)* ")")? ("requires" "(" "role" ":" str_lit | "claim" ":" str_lit "," str_lit ")")? block
params      ::= param ("," param)*        // NO trailing comma allowed
param       ::= ident ":" type

struct_decl ::= "struct" ident type_param_list? "{" field ("," field)* ","? "}"   // trailing comma OK here
field       ::= ident ":" type
type_param_list ::= "(" ident ("," ident)* ")"

enum_decl   ::= "enum" ident type_param_list? "{" variant ("," variant)* ","? "}"
variant     ::= ident ("(" type ("," type)* ")")?     // zero-payload variant: just `Name`, called as `Name()`

workflow_decl  ::= "workflow" ident "{" data_block? state_decl+ "}"
data_block     ::= "data" "{" field ("," field)* ","? "}"
state_decl     ::= "state" ident "terminal"? "{" on_entry_block? on_exit_block? transition* "}"
on_entry_block ::= "on_entry" "{" action_call* "}"
on_exit_block  ::= "on_exit" "{" action_call* "}"
action_call    ::= ident "(" (expr ("," expr)*)? ")"
transition     ::= "on" "link"? ident "->" ident

// UI DSL (`emit-ui`/`serve` only) -- additive over pure naming-convention
// inference; a `struct` with no matching `screen` needs none of this.
screen_decl    ::= "screen" ident "{" screen_item* "}"
screen_item    ::= paginate_block | field_override | action_decl | kv_entry
paginate_block ::= "paginate" "{" kv_entry* "}"
field_override ::= "field" ident "{" kv_entry* "}"
action_decl    ::= "action" str_lit "->" ident ("{" kv_entry* "}")?
kv_entry       ::= ident ":" expr     // value is ANY expr: str/int/bare-ident-fn-ref/role(..)/claim(..)

dashboard_decl ::= "dashboard" "{" dashboard_item* "}"
dashboard_item ::= ("tile" | "chart") str_lit "->" ident

// Pure nav-grouping sugar for `emit-ui` -- everything inside still
// registers into the ordinary flat top-level namespace. Single-level
// only: a `module` inside a `module`, or a `screen`/`dashboard` inside
// one, is a parse error.
module_decl ::= "module" str_lit "{" (fn_decl | struct_decl | enum_decl)* "}"

type        ::= "&" type | "box" type | "thread" type | "chan" type | "sandbox"
              | "Vector" "(" type "," int_lit ")" | "Matrix" "(" type "," int_lit "," int_lit ")"
              | "fn" "(" (type ("," type)*)? ")" ("->" type)?    // first-class function value's type
              | "i8"|"i16"|"i32"|"i64"|"u8"|"u16"|"u32"|"u64"|"usize"|"f64"
              | "bool" | "unit" | "str" | "tcp" | "tcp_listener" | "file" | "json" | "db" | "mq"
              | ident ("(" type ("," type)* ")")?      // struct/enum name, optionally with type args

block       ::= "{" stmt* "}"
stmt        ::= let_stmt | return_stmt | while_stmt | audited_stmt | expr_stmt
let_stmt    ::= "let" ident ":" type "=" expr
return_stmt ::= "return" expr?
while_stmt  ::= "while" expr block
audited_stmt::= "audited" str_lit "{" stmt* "}"
expr_stmt   ::= expr

expr        ::= if_expr | transact_expr | match_expr | assignment
if_expr     ::= "if" expr block ("else" (block | if_expr))?
match_expr  ::= "match" expr "{" match_arm ("," match_arm)* ","? "}"
match_arm   ::= (ident ("(" ident ("," ident)* ")")? "=>" expr)     // enum-variant arm
              | ((str_lit|int_lit|"true"|"false"|"_") "=>" expr)   // literal arm; needs trailing `_ => ..` arm
transact_expr ::= "transact" "{"
                     ("precheck" ":" call)?
                     "network" ":" call ("retry" int_lit)? ("timeout" int_lit)?
                     "verify" ":" call
                     "commit" ":" call
                     ("compensate" ":" call)?
                     ("log" ":" call)?
                   "}"
assignment  ::= ident "=" assignment | logic_or
logic_or    ::= logic_and ("||" logic_and)*
logic_and   ::= equality ("&&" equality)*
equality    ::= comparison (("=="|"!=") comparison)*
comparison  ::= additive (("<"|">"|"<="|">=") additive)*
additive    ::= multiplicative (("+"|"-") multiplicative)*
multiplicative ::= unary (("*"|"/"|".*"|"./") unary)*
unary       ::= ("!"|"-"|"*"|"box"|"&") unary
              | "spawn" call | "join" unary | "chan"
              | "send" "(" expr "," expr ")" | "recv" "(" expr ")"
              | "sandbox" call | "stop" unary
              | "connect" "(" expr "," expr ")" | "listen" "(" expr ")" | "accept" "(" expr ")"
              | call
call        ::= postfix ("(" args? ")")?      // AT MOST one call — f()() is a parse error
args        ::= expr ("," expr)*              // NO trailing comma allowed
postfix     ::= primary (("[" expr ("," expr)* "]") | ("." ident))*
primary     ::= int_lit | float_lit | str_lit | "true" | "false" | ident | "(" expr ")" | array_lit
array_lit   ::= "[" expr ("," expr)* "]"       // all-scalar => Vector; all-same-shape-Vector => Matrix
str_lit     ::= '"' (char | "\\" ["\\ntr])* '"'   // only \" \\ \n \t \r escapes; no concatenation
int_lit     ::= digit+                          // no 0x/0b, no _ separators
float_lit   ::= digit+ "." digit+                // required digits both sides; no 1e10, no .5, no 1.
ident       ::= alpha (alpha|digit|"_")*
```

**No semicolons, no statement separator.** A block's statements run
together with no delimiter, so the parser **always extends the current
expression instead of ending the statement** when a token is ambiguous:
```
let x: i64 = 1
-2
```
is ONE statement: `x = 1 - 2` (`x` is `-1`), never two statements. Same
for `return x` / `-y` on the next line → `return (x - y)`. **Always put
unrelated statements on lines that can't be read as continuing the
previous expression** (e.g. start the next line with `let`, `if`,
`return`, a bare call, etc., not with `-`/`+`/`*`/`(`/`[`).

## Semantics cheat-sheet

- **No `for`, no closures, no tuples, no `%`, no bitwise ops, no
  `f()()`, no chained `[]`/`.field` after a call's `()`.**
- **Block value**: a block's value is its last statement's expression,
  *if* that last statement is a bare expression (no `let`/`return`/
  `while`). Powers `if`-as-expression: `let x: i64 = if c { 1 } else { 2 }`.
- **`unit`**: a type only, no literal. Implicit return of a function with
  no `-> T` and no `return`.
- **Affine (move-once) types**: `box T`, `thread T`, `sandbox`, `tcp`,
  `tcp_listener`, `file`, `db`, `mq`. Using the binding again after it
  moves (passed by name into a call/spawn/send, or consumed by
  `join`/`stop`) is a static use-after-move error. `&T` borrows without
  moving. `json` is a plain, freely-copyable value type — NOT affine.
- **`str` is BANNED as a user `fn`'s parameter or return type** (recursively,
  through `Result`/`Option`/generics/`box`/`&`/`Vector`/etc). Fix:
  - closed vocabulary (status/currency/decision) → a zero-payload `enum`
  - free text crossing a function boundary → wrap in a one-field struct,
    conventionally named `Text`: `struct Text { value: str }`
  `str` is fine everywhere else: struct fields, local `let`s, literals,
  builtin calls (`db_query`, `http_get`, etc. are exempt — not `fn_decl`s).
- **Struct construction is a call**: `Point(3.0, 4.0)`, not a `{ .. }`
  literal. Field access: `p.x`.
- **Enum variants are calls**: `Circle(2.0)`. A zero-payload variant is
  declared with no `()` (`enum Opt { None, Some(i64) }`) but still needs
  `()` at the *call* site: `None()`.
- **`match` exhaustiveness**: enum arms must cover every variant exactly
  once (no wildcard). Literal arms (`str`/`int`/`bool` scrutinee only,
  never `f64`) must end with exactly one `_ =>` arm.
- **Generics**: `struct Pair(A, B) { a: A, b: B }`, used as `Pair(i64, str)`
  — structural per-instantiation, no monomorphization concerns to reason
  about. Built-in prelude: `Option(T)` (`Some(T)`/`None`), `Result(T, E)`
  (`Ok(T)`/`Err(E)`) — already available, don't redeclare.
- **`requires`/`acquire`** (RBAC): `fn f(...) -> T requires(role: "admin") { .. }`
  makes `f`'s *name* uncallable directly — `f(x)` and `let g = f` are
  compile errors. Only path to a callable value:
  `acquire f(proof) -> Result(fn(...)->T, str)`, where `proof` is a
  `RoleView` (from `check_role(identity, "admin")`) or `ClaimView` (from
  `extract_claim`). `identity` comes from
  `oidc_validate_token(token, issuer, audience, jwks_json) -> Result(VerifiedIdentity, str)`.
- **`effect(pure)` / `effect(rng, io, concurrent, network)`**: optional,
  omit unless the story specifically calls for declaring effects.
- **`transact { .. }`** (durable multi-step operation, an `expr` producing
  `bool`): fixed slot order `precheck?`, `network`, `verify`, `commit`,
  `compensate?`, `log?`. `network`'s call must pass `txn_id` (implicit,
  in scope inside `transact { }` only) as one of its arguments. `verify`'s
  call may only take `network`/`txn_id` as arguments.
- **`workflow Name { data { .. }? state ... }`** (durable, named state
  machine — a top-level `item`, sits alongside `fn`/`struct`/`enum`, not
  inside one). Pure desugaring: the compiler synthesizes, from the block
  alone, an enum `<Name>Event` (one variant per distinct transition
  event), a struct `<Name>Data` (one field per `data` block field, empty
  struct if no `data` block), and functions `start_<name_snake>(data:
  <Name>Data) -> Result(i64, ...)` (creates an instance, runs the start
  state's `on_entry`, returns the new instance id) and
  `advance_<name_snake>(instance_id: i64, event: <Name>Event, payload:
  json) -> Result(bool, ...)` (fires a named `on <Event> -> <Target>`
  transition: runs the current state's `on_exit`, moves state, runs the
  target's `on_entry`). A `link`-marked transition (`on link E -> T`)
  additionally gets `<e>_via_link(instance_id: i64, token: <Name>
  LinkToken, payload: json) -> Result(bool, ...)`, an unauthenticated,
  single-use trigger (no `requires`/identity needed) — for "click this
  emailed link to verify" flows. `terminal` marks a state with no
  outgoing transitions expected. Inside an `on_entry`/`on_exit` action
  call's own arguments only: `instance_id: i64` is always in scope;
  `data.<field>` (read-only) for each declared `data` field; and, only
  inside the `on_entry` of a state with an outgoing `link`-marked
  transition named `E`, `link_E` (the freshly minted link token). An
  action call must be a bare `name(args)` — builtins allowed (unlike
  `transact`'s slots), including the notification builtins `send_email`/
  `send_sms`/`send_push`/`notify` (`WORKFLOW.md`). Interpreter-only —
  `nirdosha build`/`emit-llvm` reject a program containing `workflow`.
- **`screen`/`dashboard`** (`emit-ui`/`serve` only — a full CRUD+dashboard
  UI is already inferred from `struct` + `list_/create_/update_/delete_/
  get_<struct>` and `stat_/chart_<name>` naming conventions with **zero**
  syntax; `screen`/`dashboard` are optional, additive overrides for what
  a naming convention can't express). Complete key vocabulary — do not
  invent other keys, they will parse (any `ident: expr` is syntactically
  legal) but silently do nothing:
  | Block | Key | Value shape | Effect |
  |---|---|---|---|
  | `screen X { ... }` top level | `title` | `str` | overrides the nav/heading label (default: struct name) |
  | | `list` / `create` / `update` / `delete` | bare fn ident | overrides which fn backs that slot (default: `<kind>_<snake_struct>`); **checked to exist** |
  | `paginate { ... }` | `page_size` | `int` | parsed only — **not yet wired into the generated UI** |
  | | `total` | any expr | parsed only — **not yet wired** |
  | `field <name> { ... }` | `label` | `str` | overrides that field's displayed label |
  | | `view` | `role("r1","r2",..)` or `claim("key","value")` | **enforced**: `nirdosha serve` nulls this field out in every JSON response to an identity that doesn't satisfy it |
  | | `edit` | `role(...)` / `claim(...)` | **enforced**: `update_<Struct>` returns `403` if the request changes this field's stored value and the caller doesn't satisfy it |
  | | `pattern` | `str` (a regex) | **enforced** on `str` fields only: `create_<Struct>`/`update_<Struct>` return `400` if the value doesn't match; also set as the form input's native HTML5 `pattern` attribute |
  | | `format` | `"email"` \| `"phone"` \| `"date"` \| `"url"` \| `"uuid"` | sugar for a built-in `pattern`, same enforcement; str fields only; **not** combinable with `pattern` on the same field |
  | | `min` / `max` | `int` or `float` | **enforced** on numeric fields only: `create_<Struct>`/`update_<Struct>` return `400` if the value is out of range; also set as the input's native HTML5 `min`/`max` |
  | | `searchable` / `sortable` | any | parsed only — **not yet wired**, documented placeholder names |
  | `action "<label>" -> <fn> { ... }` | `style` | `"filled"` \| `"outlined"` | button visual style |
  | | `confirm` | `str` | `window.confirm(...)`-gates the call |
  | `dashboard { ... }` | `tile "<label>" -> <fn>` / `chart "<label>" -> <fn>` | — | metric tile / chart card; `<fn>` must exist |
  `view`/`edit` are the real **table-level read/write access control**
  primitive (field-granular, not whole-table) — combine with `requires`
  on the CRUD `fn`s themselves for **function-level** access control
  (who may call `create_x`/`update_x`/`delete_x` at all vs. who may
  read/change one specific column of it) — see the worked example below,
  which uses both together.
- **`module "Name" { fn_decl | struct_decl | enum_decl }`**: pure
  nav-grouping sugar for `emit-ui`'s sidebar — every declaration inside
  still registers in the ordinary flat global namespace (`typeck.rs`
  doesn't even look at `module`, only `ui_gen.rs` does). One level only:
  nesting a `module`/`screen`/`dashboard` inside a `module` is a parse
  error.
- **`audited "<non-empty justification>" { stmt* }`**: suppresses
  `codegen.rs`'s Tier-1/2 runtime guard *emission* (e.g. the div-by-zero
  trap) inside `body` for compiled builds only — the interpreter always
  runs its own checks regardless. Has no reachable value of its own
  (bare `stmt*`, not a `block`) — put a `return` inside if the enclosing
  `fn` needs one.
- **Concurrency**: `spawn f(args) -> thread T` (real OS thread; `T` = `f`'s
  return type), `join(h) -> T` (consumes handle). `chan`: `let c: chan T = chan`,
  `send(c, v)` (never blocks), `recv(c) -> T` (blocks).
- **Sandbox** (real OS process): `sandbox f(args) -> sandbox` where `f`
  must be `-> unit` with only scalar/bool params. `stop(s) -> i64` (exit code).
- **TCP**: `connect(host: str, port: i64) -> tcp`, `listen(port: i64) -> tcp_listener`,
  `accept(l) -> tcp`, `send(conn, s: str)`, `recv(conn) -> str`, `stop(conn)`.
- **File**: `open(path: str, mode: str) -> file` (`mode`: `"r"`/`"w"`/`"a"`),
  same `send`/`recv`/`stop` as tcp.
- Ints default to `i64` if unannotated but flex to a narrower declared
  width; floats are always `f64`, no implicit int↔float conversion ever.
  Int `/` traps on zero; float `/` saturates to `inf`/`NaN`.

## Key builtins (name(args) -> ret)

`print(...)` any args · `len(v)` `sum(v)` `dot(a,b)` `cross(a,b)`
`transpose(m)` `zeros(n)`/`zeros(r,c)` `ones(n)`/`ones(r,c)` `identity(n)`
`norm(v)` `det(m)` `inv(m)` `solve(a,b)` `trace(m)` `rank(m)` ·
`rand_seed(n)` `rand_f64()` `rand_gaussian(mean,sd)` ·
`db_connect(conn_str) -> Result(db,str)` `db_query(conn,sql,...binds) -> Result(json,str)`
`db_execute(conn,sql,...binds) -> Result(i64,str)` ·
`oidc_validate_token(token,issuer,aud,jwks) -> Result(VerifiedIdentity,str)`
`check_role(identity,role) -> Result(RoleView,str)`
`extract_claim(identity,name) -> Result(ClaimView,str)` ·
`json_parse(s: str) -> Result(json,str)` `json_get(j: json, key: str) -> Result(json,str)`
`json_array_get(j: json, idx: i64) -> Result(json,str)` `json_array_len(j: json) -> Result(i64,str)`
`json_get_str(j: json, key: str) -> Result(str,str)` `json_get_i64(j: json, key: str) -> Result(i64,str)`
`json_get_f64(j: json, key: str) -> Result(f64,str)` `json_get_bool(j: json, key: str) -> Result(bool,str)`
`json_set_str(doc: json, key: str, value: str) -> Result(json,str)`

## Examples (verified against the real compiler)

**Function + arithmetic**
```
fn add(a: i32, b: i32) -> i32 {
    return a + b
}

fn main() {
    let x: i32 = 5
    let y: i32 = add(x, 3)
    print(y)
}
```

**struct / enum / match / field access**
```
struct Point {
    x: f64,
    y: f64,
}

enum Shape {
    Circle(f64),
    Rectangle(f64, f64),
}

fn area(s: Shape) -> f64 {
    return match s {
        Circle(r) => 3.14159 * r * r,
        Rectangle(w, h) => w * h,
    }
}

fn main() -> f64 {
    let p: Point = Point(3.0, 4.0)
    let c: Shape = Circle(2.0)
    return p.x + p.y + area(c)
}
```

**Generics + Option/Result prelude**
```
struct Duo(A, B) {
    first: A,
    second: B,
}

fn sum_pair(p: Duo(i64, i64)) -> i64 {
    return p.first + p.second
}

fn unwrap_option(o: Option(i64), default: i64) -> i64 {
    return match o {
        Some(n) => n,
        None => default,
    }
}
```

**`str` ban / "enum favoring" pattern**
```
struct Text {
    value: str,
}

fn greet(name: Text) -> Text {   // NOT (name: str) -> str -- illegal
    return name
}

enum Status {
    Pending,
    Approved,
    Rejected,
}

fn describe(s: Status) -> Text {
    return match s {
        Pending => Text("waiting"),
        Approved => Text("done"),
        Rejected => Text("denied"),
    }
}
```

**First-class functions** (`fn(T1,T2) -> R` type; no closures — a plain
function name is the only value this type holds, nothing captured)
```
fn double(x: i64) -> i64 {
    return x * 2
}

fn apply(f: fn(i64) -> i64, x: i64) -> i64 {
    return f(x)
}

fn main() -> i64 {
    let g: fn(i64) -> i64 = double
    return apply(g, 21)
}
```

**Ownership (`box`, affine move)**
```
fn worker(b: box i64) -> i64 {
    return *b
}

fn main() {
    let h: box i64 = box 21
    print(worker(h))   // `h` is moved here; using `h` again below is a compile error
}
```

**Concurrency: spawn/join, chan/send/recv**
```
fn double(n: i64) -> i64 {
    return n * 2
}

fn producer(c: chan i64) -> unit {
    send(c, 14)
    send(c, 28)
    return
}

fn main() {
    let h1: thread i64 = spawn double(21)
    print(join h1)

    let c: chan i64 = chan
    let h2: thread unit = spawn producer(c)
    let a: i64 = recv(c)
    let b: i64 = recv(c)
    join h2
    print(a + b)
}
```

**Sandbox (separate OS process)**
```
fn background_work() -> unit {
    while true {
    }
}

fn main() {
    let s: sandbox = sandbox background_work()
    let code: i64 = stop s
    print(code)
}
```

**RBAC: `requires` + `acquire`**
```
fn transfer_funds(amount: i64) -> i64 requires(role: "admin") {
    return amount
}

fn main() -> i64 {
    return match oidc_validate_token(token, issuer, audience, jwks) {
        Ok(identity) => match check_role(identity, "admin") {
            Ok(proof) => match acquire transfer_funds(proof) {
                Ok(f) => f(500),
                Err(e) => -1,
            },
            Err(e) => -2,
        },
        Err(e) => -3,
    }
}
```

**`transact` (durable multi-step effect)**
```
fn checkout(amount: i64) -> bool {
    return transact {
        precheck:   db_reachable()
        network:    call_api(txn_id, amount)
        verify:     check(network)
        commit:     update_db(amount)
        compensate: refund(amount)
        log:        write_log(amount, verify)
    }
}
```

**`workflow` (durable state machine)**
```
fn send_reminder(instance_id: i64) -> bool {
    print("reminder for", instance_id)
    return true
}

workflow Onboarding {
    data {
        name: str,
    }

    state Start {
        on_entry {
            send_reminder(instance_id)
        }
        on link Verify -> Verified
        on Cancel -> Cancelled
    }

    state Cancelled terminal {
        on_entry {
            send_reminder(instance_id)
        }
    }

    state Verified terminal {
        on_entry {
            send_reminder(instance_id)
        }
        on_exit {
            send_reminder(instance_id)
        }
    }
}

// Compiler-synthesized (don't redeclare): enum OnboardingEvent { Verify, Cancel },
// struct OnboardingData { name: str },
// fn start_onboarding(data: OnboardingData) -> Result(i64, ...),
// fn advance_onboarding(instance_id: i64, event: OnboardingEvent, payload: json) -> Result(bool, ...),
// fn verify_via_link(instance_id: i64, token: OnboardingLinkToken, payload: json) -> Result(bool, ...)

fn main() -> i64 {
    let d: OnboardingData = OnboardingData("alice")
    return match start_onboarding(d) {
        Ok(id) => id,
        Err(e) => -1,
    }
}
```

**`screen`/`dashboard`/`module` — table read/write access + function-level access, together**
(`emit-ui`/`serve` only; verified end-to-end through `nirdosha emit-ui`)
```
struct Product {
    id: i64,
    name: str,
    price_cents: i64,
    cost_cents: i64,
}

struct Text {
    value: str,
}

fn list_product() -> Result(json, Text) {
    return match db_connect("products.db") {
        Ok(conn) => match db_query(conn, "SELECT id, name, price_cents, cost_cents FROM product ORDER BY id") {
            Ok(rows) => Ok(rows),
            Err(e) => Err(Text(e)),
        },
        Err(e) => Err(Text(e)),
    }
}

// Function-level access control: only "admin" may call create/update/delete at all.
fn create_product(p: Product) -> Result(i64, Text) requires(role: "admin") {
    return match db_connect("products.db") {
        Ok(conn) => match db_execute(conn, "INSERT INTO product (name, price_cents, cost_cents) VALUES (?, ?, ?)", p.name, p.price_cents, p.cost_cents) {
            Ok(n) => Ok(n),
            Err(e) => Err(Text(e)),
        },
        Err(e) => Err(Text(e)),
    }
}

fn update_product(p: Product) -> Result(i64, Text) requires(role: "admin") {
    return match db_connect("products.db") {
        Ok(conn) => match db_execute(conn, "UPDATE product SET name = ?, price_cents = ?, cost_cents = ? WHERE id = ?", p.name, p.price_cents, p.cost_cents, p.id) {
            Ok(n) => Ok(n),
            Err(e) => Err(Text(e)),
        },
        Err(e) => Err(Text(e)),
    }
}

fn delete_product(id: i64) -> Result(i64, Text) requires(role: "admin") {
    return match db_connect("products.db") {
        Ok(conn) => match db_execute(conn, "DELETE FROM product WHERE id = ?", id) {
            Ok(n) => Ok(n),
            Err(e) => Err(Text(e)),
        },
        Err(e) => Err(Text(e)),
    }
}

fn stat_product_count() -> Result(i64, Text) {
    return match db_connect("products.db") {
        Ok(conn) => match db_query(conn, "SELECT COUNT(*) as c FROM product") {
            Ok(rows) => Ok(0),
            Err(e) => Err(Text(e)),
        },
        Err(e) => Err(Text(e)),
    }
}

fn chart_products_by_price() -> Result(json, Text) {
    return match db_connect("products.db") {
        Ok(conn) => match db_query(conn, "SELECT name, price_cents FROM product") {
            Ok(rows) => Ok(rows),
            Err(e) => Err(Text(e)),
        },
        Err(e) => Err(Text(e)),
    }
}

screen Product {
    title: "Catalog"
    list: list_product
    create: create_product
    update: update_product
    delete: delete_product
    paginate {
        page_size: 25
    }
    field name {
        label: "Product Name"
    }
    // Table-level read/write access control, field-granular: only
    // admin/finance can even SEE cost_cents (nulled out otherwise);
    // only admin can CHANGE it.
    field cost_cents {
        label: "Cost (cents)"
        view: role("admin", "finance")
        edit: role("admin")
    }
    action "Restock +10" -> create_product {
        style: "outlined"
        confirm: "Restock this product by 10 units?"
    }
}

dashboard {
    tile "Products" -> stat_product_count
    chart "By Price" -> chart_products_by_price
}

// Nav-grouping only -- no access-control meaning.
module "Catalog" {
    struct Warehouse {
        id: i64,
        location: str,
    }
}

fn main() {}
```

**JSON builtins**
```
struct Text {
    value: str,
}

fn extract_name(doc: json) -> Result(Text, Text) {
    return match json_get_str(doc, "name") {
        Ok(s) => Ok(Text(s)),
        Err(e) => Err(Text(e)),
    }
}

fn main() -> i64 {
    return match json_parse("{\"name\": \"widget\", \"count\": 3}") {
        Ok(doc) => match extract_name(doc) {
            Ok(t) => match json_get_i64(doc, "count") {
                Ok(n) => n,
                Err(e) => -1,
            },
            Err(e) => -2,
        },
        Err(e) => -3,
    }
}
```

**`audited` (Tier-3 escape hatch)**
```
fn divide(a: i64, b: i64) -> i64 {
    audited "b is validated non-zero by the caller's upstream form" {
        return a / b
    }
}

fn main() -> i64 {
    return divide(10, 2)
}
```

## Standing fixtures — emit once per project, not per user story

Two cross-cutting admin panels every generated project should carry,
regardless of which user stories are being processed — real, working
`struct` + CRUD-convention-named `fn`s, verified end-to-end (typecheck,
`nirdosha serve`, live CRUD via curl, real screens in the `emit-ui`
manifest) before being added here. Emit each **exactly once** per
output file, not once per story that happens to touch identity or
notifications. **Replace every `db_connect("<DB>")` below with the
exact same connection-string literal every other `db_connect` call in
this output already uses** — a mismatched literal is silently a
different SQLite file than whatever `--db` migrated, and every call
fails with a "no such table" error that looks like a bug in the fixture
but is actually just two different files.

**Communication Control Panel** (`WORKFLOW.md`'s "communication
control" — the provider config `send_email`/`send_sms`/`send_push`/
`notify` read at send time, LANGUAGE.md §14). Emit the `EmailProvider
Config` block below whenever a story mentions email notifications;
`SmsProviderConfig`/`PushProviderConfig` are the identical shape with
`sms`/`push` names, add whichever channels the stories actually use
(don't add a channel no story needs):

```nirdosha
struct EmailProviderConfig {
    id: i64,
    active: bool,
    host: str,
    port: i64,
    path: str,
    api_key: str,
    from_address: str,
}

fn list_email_provider_config() -> Result(json, Text) requires(role: "admin") {
    return match db_connect("<DB>") {
        Ok(conn) => match db_query(conn, "SELECT id, active, host, port, path, api_key, from_address FROM email_provider_config ORDER BY id") {
            Ok(rows) => Ok(rows),
            Err(e) => Err(Text(e)),
        },
        Err(e) => Err(Text(e)),
    }
}

fn create_email_provider_config(c: EmailProviderConfig) -> Result(i64, Text) requires(role: "admin") {
    return match db_connect("<DB>") {
        Ok(conn) => match db_execute(conn, "INSERT INTO email_provider_config (active, host, port, path, api_key, from_address) VALUES (?, ?, ?, ?, ?, ?)", c.active, c.host, c.port, c.path, c.api_key, c.from_address) {
            Ok(n) => Ok(n),
            Err(e) => Err(Text(e)),
        },
        Err(e) => Err(Text(e)),
    }
}

fn update_email_provider_config(c: EmailProviderConfig) -> Result(i64, Text) requires(role: "admin") {
    return match db_connect("<DB>") {
        Ok(conn) => match db_execute(conn, "UPDATE email_provider_config SET active = ?, host = ?, port = ?, path = ?, api_key = ?, from_address = ? WHERE id = ?", c.active, c.host, c.port, c.path, c.api_key, c.from_address, c.id) {
            Ok(n) => Ok(n),
            Err(e) => Err(Text(e)),
        },
        Err(e) => Err(Text(e)),
    }
}

fn delete_email_provider_config(id: i64) -> Result(i64, Text) requires(role: "admin") {
    return match db_connect("<DB>") {
        Ok(conn) => match db_execute(conn, "DELETE FROM email_provider_config WHERE id = ?", id) {
            Ok(n) => Ok(n),
            Err(e) => Err(Text(e)),
        },
        Err(e) => Err(Text(e)),
    }
}
```

**Identity Role-Mapping Panel** (`ROADMAP.md` Track A item A6 —
translates the app's canonical role vocabulary into whatever the
connected IdP actually puts in a token's `roles` claim; `nirdosha
serve`'s in-memory cache, refreshed on a 30s TTL, is what actually
reads this table — see `LANGUAGE.md` §11a / `compiler/UI_DSL_TODO.md`).
Emit this once whenever any story mentions roles, permissions, or
access control at all:

```nirdosha
struct RoleMapping {
    id: i64,
    app_role: str,
    idp_role: str,
}

fn list_role_mapping() -> Result(json, Text) requires(role: "admin") {
    return match db_connect("<DB>") {
        Ok(conn) => match db_query(conn, "SELECT id, app_role, idp_role FROM role_mapping ORDER BY id") {
            Ok(rows) => Ok(rows),
            Err(e) => Err(Text(e)),
        },
        Err(e) => Err(Text(e)),
    }
}

fn create_role_mapping(m: RoleMapping) -> Result(i64, Text) requires(role: "admin") {
    return match db_connect("<DB>") {
        Ok(conn) => match db_execute(conn, "INSERT INTO role_mapping (app_role, idp_role) VALUES (?, ?)", m.app_role, m.idp_role) {
            Ok(n) => Ok(n),
            Err(e) => Err(Text(e)),
        },
        Err(e) => Err(Text(e)),
    }
}

fn update_role_mapping(m: RoleMapping) -> Result(i64, Text) requires(role: "admin") {
    return match db_connect("<DB>") {
        Ok(conn) => match db_execute(conn, "UPDATE role_mapping SET app_role = ?, idp_role = ? WHERE id = ?", m.app_role, m.idp_role, m.id) {
            Ok(n) => Ok(n),
            Err(e) => Err(Text(e)),
        },
        Err(e) => Err(Text(e)),
    }
}

fn delete_role_mapping(id: i64) -> Result(i64, Text) requires(role: "admin") {
    return match db_connect("<DB>") {
        Ok(conn) => match db_execute(conn, "DELETE FROM role_mapping WHERE id = ?", id) {
            Ok(n) => Ok(n),
            Err(e) => Err(Text(e)),
        },
        Err(e) => Err(Text(e)),
    }
}
```

## Instructions for you (the model)

1. Read the user story, pick the smallest correct set of constructs above.
2. Prefer `enum`+`match` over any string comparison — bare `str` cannot
   appear in a `fn` signature at all.
3. Every `fn` needs an explicit `-> Type` unless it truly returns nothing
   (then omit `-> Type` entirely; don't write `-> unit` needlessly, though
   it's legal).
4. Don't invent syntax not in the grammar above (no tuples, no `for`, no
   `{ field: val }` struct literals, no string concatenation).
5. Watch the statement-extension rule: never start a new statement's line
   with `-`, `*`, `(`, `[`, or any binary operator.
6. **Name functions for UI inference, not just for readability.** Identify
   the primary persistent entity (`struct`) the story's action reads or
   writes — declare a minimal `struct` for it now if none exists yet in
   this output. Then map the action onto that struct's snake_case name
   using the *exact* prefix `nirdosha emit-ui`/`serve` scan for — these
   are load-bearing, not stylistic:
   | Story action shape | Required fn name | UI effect |
   |---|---|---|
   | creates a new record | `create_<entity>` | screen's create form |
   | lists/browses existing records | `list_<entity>` | screen's table |
   | fetches one existing record | `get_<entity>` | screen's detail view |
   | modifies an existing record | `update_<entity>` | table row's Edit |
   | removes a record | `delete_<entity>` | table row's Delete |
   | a single numeric KPI (count/sum/rate) | `stat_<name>() -> i64\|f64` | Dashboard tile |
   | a labeled series for a chart | `chart_<name>() -> json` (rows shaped `{label,value}`) | Dashboard chart |
   A correct-but-differently-worded verb is **invisible to the generated
   UI** — write `create_document`, never `ingest_document`; `list_document`,
   never `get_extracted_document_data`. Don't rename the *entity* half
   (keep it the struct's exact name), only the *verb* half is fixed to
   this vocabulary.
7. A story's action that doesn't fit the CRUD/stat/chart shape (e.g. a
   one-off domain decision like "recommend a workflow variant") stays a
   normally-named helper `fn`, but wire it into the UI by attaching it to
   the entity's `screen` block as `action "<label>" -> <fn>` (see the
   `screen`/`dashboard` key-vocabulary table and worked example above) —
   otherwise it's dead code the generated app can never call.
8. **Encode format constraints the story implies, don't leave them as
   prose.** If a story's `acceptance_criteria`/`narrative`/preconditions
   imply a `str` field must look a certain way (an email address, a
   phone number, an ISO date, a specific character set) or a numeric
   field has a valid range (a percentage 0-100, a non-negative amount,
   an age), add a `screen <Entity> { field <name> { ... } }` block with
   `pattern`/`format`/`min`/`max` for it — see the key-vocabulary table
   above. Prefer `format` over hand-writing a regex when the shape is
   `"email"`/`"phone"`/`"date"`/`"url"`/`"uuid"`; write `pattern` only
   for a shape not in that list. Don't invent a `screen` block just to
   attach one of these keys to a field with no such constraint implied
   by the story — this is for constraints the story actually states or
   clearly implies, not speculative validation.
9. **Include the standing fixtures exactly once per project.** If any
   story in this run mentions email/SMS/push notifications, include
   the matching Communication Control Panel struct+fns from the
   "Standing fixtures" section above; if any story mentions roles,
   permissions, or access control, include the Identity Role-Mapping
   Panel struct+fns. Check whether they're already present in this
   output before adding them again — once per project, not once per
   story that happens to touch the same concern.
10. Output the raw `.nir` source only.
