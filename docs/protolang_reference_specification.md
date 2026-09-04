# ProtoLang Reference Implementation Specification
## A Language for Distributed, Durable, and Observable Systems

**Version:** 0.1.0-draft  
**Date:** 2026-08-21  
**Status:** Reference Implementation Guide

---

## Table of Contents

1. [Language Overview & Design Philosophy](#1-language-overview)
2. [Core Type System](#2-core-type-system)
3. [Effect System](#3-effect-system)
4. [Distributed Transaction Primitives](#4-distributed-transaction-primitives)
5. [Async Without Function Coloring](#5-async-without-function-coloring)
6. [Null Safety](#6-null-safety)
7. [Linear Types & Resource Management](#7-linear-types)
8. [Type-Safe Database Queries](#8-type-safe-database-queries)
9. [State-Dependent Types (Protocol Types)](#9-state-dependent-types)
10. [Configuration as Code](#10-configuration-as-code)
11. [Built-in Observability](#11-built-in-observability)
12. [Exhaustive Error Handling](#12-exhaustive-error-handling)
13. [Data Race Freedom](#13-data-race-freedom)
14. [Feature Flags & Experiments](#14-feature-flags--experiments)
15. [Implicit Context Propagation](#15-implicit-context-propagation)
16. [Runtime Architecture](#16-runtime-architecture)
17. [Compilation Pipeline](#17-compilation-pipeline)
18. [Standard Library Design](#18-standard-library-design)
19. [Example: Complete Application](#19-example-complete-application)

---

## 1. Language Overview & Design Philosophy

### 1.1 Guiding Principles

1. **Effects are explicit:** Every function declares what it can do beyond returning a value.
2. **Failures are typed:** Every possible failure mode is part of the function's type signature.
3. **Resources are linear:** Acquired resources must be consumed exactly once.
4. **State is protocol-aware:** Objects track their state in the type system.
5. **Observability is automatic:** Tracing, metrics, and logging require zero boilerplate.
6. **Context is implicit but typed:** Request-scoped data propagates automatically.
7. **Configuration is code:** All configuration is statically typed and checked.
8. **Experiments are first-class:** A/B tests are language constructs with automatic cleanup.

### 1.2 Syntax Philosophy

ProtoLang uses a Rust/Scala-like syntax with significant whitespace (Python-style blocks) for readability, but with mandatory type annotations on public APIs. Private functions can use local type inference.

```protolang
// Public function: explicit types and effects
public effect(network, mutates) function processOrder(
  order: Order
): Result<Receipt, PaymentError> throws NetworkError, DBError {
  // body
}

// Private function: local type inference allowed
private function calculateTotal(items) {
  items.fold(0, (sum, item) => sum + item.price)
}
```

---

## 2. Core Type System

### 2.1 Primitive Types

```protolang
// Scalar types (all non-null by default)
bool        // true | false
int8, int16, int32, int64, int128
uint8, uint16, uint32, uint64, uint128
float32, float64
decimal     // Arbitrary precision decimal for financial calculations
char        // Unicode scalar value
string      // UTF-8 encoded, immutable
bytes       // Raw byte array

// Composite types
Array<T>          // Fixed-size, homogeneous
List<T>           // Variable-size, linked or array-backed
Map<K, V>         // Hash map with deterministic iteration
Set<T>            // Hash set
Tuple<T1, T2, ...> // Heterogeneous, fixed-size
Record { ... }    // Named field product type (struct)
Variant { ... }   // Named field sum type (enum with data)
Option<T>         // Some(T) | None
Result<T, E>      // Ok(T) | Err(E)
```

### 2.2 Type Aliases

```protolang
type UserId = UUID
type Money = decimal
type Email = string  // Can attach validation constraints
```

### 2.3 Type Constraints (Refinement Types)

```protolang
type PositiveInt = int where value > 0
type NonEmptyString = string where value.length > 0
type Email = string where value.matches(r"^[^@]+@[^@]+$")
```

The compiler generates runtime checks at construction boundaries and proves what it can at compile time.

### 2.4 Generics

```protolang
function map<T, U>(list: List<T>, f: function(T): U): List<U>

interface Container<T> {
  function get(): Option<T>
  function put(value: T): void
}
```

Generics are monomorphized at compile time (like Rust/C++) for zero-cost abstraction, with optional type erasure for dynamic dispatch.

---

## 3. Effect System

### 3.1 Core Design

Every function has an **effect signature** that the compiler tracks. Effects are part of the function's type, just like parameter and return types.

```protolang
// Effect kinds
pure                    // No side effects, deterministic
mutates               // Mutates local or captured state
reads<Region>         // Reads from a specific memory region
writes<Region>        // Writes to a specific memory region
io                    // Performs file I/O
network               // Makes network calls
async                 // May suspend (see Section 5)
throws<E1, E2, ...>   // May throw specific errors (see Section 12)
linear                // Uses linear resources (see Section 7)
```

### 3.2 Effect Polymorphism

Functions are **effect-polymorphic** by default. They inherit the effects of the functions they call.

```protolang
// This function is inferred as: effect(network, mutates)
function processPayment(order: Order) {
  let gateway = getPaymentGateway()   // effect(network)
  let result = gateway.charge(order)  // effect(network)
  db.save(result)                     // effect(mutates)
  return result
}

// Explicit effect annotation restricts what the function can do
pure function calculateTax(amount: Money, rate: float64): Money {
  // Compiler error if any non-pure function is called
  return amount * rate
}
```

### 3.3 Effect Subtyping

Effects form a lattice:
```
pure < mutates < io < network
pure < mutates < io
```

A `pure` function can be used where `mutates` is expected (covariant in effects), but not vice versa.

### 3.4 Effect Masking

Sometimes you need to call an effectful function from a pure context and handle the effects:

```protolang
pure function getConfigValue(key: string): Option<string> {
  // mask effect(io) to call a cached config loader
  mask effect(io) {
    return configCache.get(key)
  }
}
```

The `mask` block tells the compiler: "I know this has effects, but I'm handling them safely." The compiler verifies that the masked effects do not escape.

### 3.5 Higher-Order Functions and Effects

Higher-order functions automatically propagate effects:

```protolang
function map<T, U>(
  list: List<T>,
  f: function(T): U  // Inherits effects of `f`
): List<U> {
  // ...
}

// Usage:
let urls = ["a.com", "b.com"]
// `results` has effect(network) because `fetch` has effect(network)
let results = urls.map(fetch)
```

---

## 4. Distributed Transaction Primitives

### 4.1 Guardians

A `guardian` is a crash-resilient distributed object. It survives process restarts and maintains stable state.

```protolang
guardian OrderService {
  // Stable fields are persisted to the guardian's write-ahead log
  stable orders: Map<OrderId, Order>
  stable nextId: int64

  // Volatile fields are reconstructed on restart
  volatile cache: LRUCache<OrderId, Order>
  volatile metrics: MetricsCollector

  // Constructor: called when guardian is first created
  init(config: ServiceConfig) {
    this.orders = Map.empty()
    this.nextId = 0
    this.cache = LRUCache.new(config.cacheSize)
    this.metrics = MetricsCollector.new("order_service")
  }

  // Recover block: called after every crash restart
  recover {
    // Rebuild volatile state from stable state
    this.cache = LRUCache.new(1000)
    for order in this.orders.values() {
      this.cache.put(order.id, order)
    }
    this.metrics = MetricsCollector.new("order_service")
  }

  // Public methods are automatically transaction-wrapped
  public function createOrder(request: OrderRequest): Order {
    let id = this.nextId
    this.nextId += 1
    let order = Order.new(id, request)
    this.orders.put(id, order)
    this.cache.put(id, order)
    this.metrics.increment("orders.created")
    return order
  }
}
```

### 4.2 Actions and Topactions

Every method call on a guardian runs inside an implicit `action`. Actions can be nested.

```protolang
// Implicit action (single guardian)
action {
  let order = orderService.createOrder(req)
  paymentService.charge(order)
}

// Explicit topaction (distributed across guardians)
topaction {
  let order = orderService.createOrder(req)     // subaction 1
  let payment = paymentService.charge(order)    // subaction 2
  inventoryService.reserve(order.items)         // subaction 3
  notificationService.sendConfirmation(order)   // subaction 4
}
```

**Semantics:**
- A `topaction` commits via **two-phase commit** across all participating guardians.
- If any subaction fails, all subactions abort.
- If a process crashes during a topaction, the runtime recovers and either commits or aborts all participants based on the coordinator's log.

### 4.3 Open Actions (Saga Pattern)

For long-running operations where holding locks is unacceptable, use `open action` with compensation:

```protolang
open action {
  step reserveHotel(booking) 
    compensate cancelHotel(booking)

  step reserveFlight(booking)
    compensate cancelFlight(booking)

  step chargeCard(booking.total)
    compensate refundCard(booking.total)

  step sendConfirmation(booking)
    // No compensation needed (idempotent)
}
```

**Semantics:**
- Each `step` commits immediately (releases locks).
- If a later step fails, compensations run in **reverse order**.
- The runtime journals each step and compensation to stable storage.
- If the process crashes during compensation, it resumes on restart.

### 4.4 Commit and Abort Handlers

Register callbacks that run on transaction outcome:

```protolang
action {
  let tempFile = fileSystem.createTemp("upload")

  onCommit {
    fileSystem.move(tempFile, "/permanent/" + tempFile.name)
  }

  onAbort {
    fileSystem.delete(tempFile)
  }

  processUpload(tempFile)
}
```

### 4.5 Durable Execution (Persistent Continuations)

The runtime automatically checkpoints execution before every external effect:

```protolang
// This function is automatically durable
// If the process crashes after the HTTP call, it resumes from the next line
durable function processBooking(request: BookingRequest): Booking {
  let hotel = http.get("/hotels/" + request.hotelId)      // checkpoint here
  let flight = http.get("/flights/" + request.flightId)   // checkpoint here
  let booking = db.insert(Booking.new(hotel, flight))      // checkpoint here
  return booking
}
```

**Implementation:** The compiler transforms `durable` functions into state machines. Before each effectful call, the runtime serializes the continuation (stack frame + local variables) to the guardian's write-ahead log. On crash recovery, the runtime deserializes the continuation and resumes execution.

---

## 5. Async Without Function Coloring

### 5.1 Core Design

There is **no `async` keyword**. Every function can suspend at effect boundaries, and the compiler generates the state machine automatically.

```protolang
// This function may suspend at I/O boundaries, but the caller doesn't care
function fetchUser(id: UUID): User {
  return http.get("/users/" + id)  // Suspends here, transparent to caller
}

// This function calls fetchUser but doesn't need to be "async"
function getUserEmail(id: UUID): Option<string> {
  let user = fetchUser(id)  // Transparent suspension
  return user.email         // Option<string> (see Section 6)
}

// Only `sync` functions cannot suspend — used for FFI and signal handlers
sync function hashPassword(password: string): string {
  // Cannot call any function with effect(async) or effect(network)
  return bcrypt.hash(password)
}
```

### 5.2 How It Works

The compiler uses **delimited continuations** (similar to Scheme's `call/cc` but typed and bounded):

1. Every function is compiled into a state machine with states for each suspension point.
2. When an effectful operation is encountered, the runtime captures the current continuation.
3. The I/O operation is dispatched to an event loop.
4. When the result arrives, the continuation is resumed.
5. All of this is invisible to the programmer.

### 5.3 Parallel Execution

```protolang
// `all` runs functions concurrently and waits for all results
let (user, orders) = all {
  fetchUser(userId),
  fetchOrders(userId)
}

// `race` returns the first successful result
let result = race {
  fetchFromCache(key),
  fetchFromDatabase(key)
}

// `background` spawns a task that runs independently
let task = background {
  generateReport(data)
}
// Later...
let report = await task
```

### 5.4 Cancellation

Cancellation is built into the effect system:

```protolang
function loadData(): Data {
  let source = http.get("/source") 
    with timeout = 30s
    with retry = exponential(1s, 60s, 3)
    with circuitBreaker = failureRate(0.5, 60s)

  return parse(source)
}

// Cancellation propagates automatically through the effect tree
function cancelableOperation(): Result {
  let part1 = fetchPart1()
  if isCanceled() {  // Check cancellation token (implicit)
    return Result.canceled()
  }
  let part2 = fetchPart2()
  return combine(part1, part2)
}
```

---

## 6. Null Safety

### 6.1 Core Rule

**No type is nullable by default.** The only way to represent absence is `Option<T>`.

```protolang
function findUser(id: UUID): Option<User>  // May return None

// The compiler forces exhaustive handling
match findUser(id) {
  Some(user) => print(user.name),
  None => print("User not found")
}
// Compile error if `None` branch is missing
```

### 6.2 Safe Navigation

```protolang
// `?.` returns Option<T> if any step is None
let city = user?.address?.city  // Option<string>

// `??` provides a default
let name = user?.name ?? "Anonymous"  // string

// `!` is the unsafe unwrap (requires explicit opt-in)
let name = user!.name  // Compile warning unless in unsafe block
```

### 6.3 Legacy Interop

When calling external APIs that may return null, the compiler wraps the result in `Option<T>`:

```protolang
// FFI declaration tells the compiler this C function may return null
extern function c_findUser(id: int): ?User  // ? marks nullable FFI return

// Usage is automatically safe
let user = c_findUser(42)  // Option<User>
```

---

## 7. Linear Types & Resource Management

### 7.1 Linear Resources

A `linear` resource must be consumed exactly once. The compiler tracks linearity at compile time.

```protolang
// File is a linear type
linear function processFile(path: Path): Result<Data, IOError> {
  let file = File.open(path)  // linear resource acquired
  let data = file.readAll()   // consumes file, returns data
  // Compiler error if file is not consumed
  // Compiler error if file is used after consumption
  return Ok(data)
}

// Explicit close (alternative pattern)
linear function processFile2(path: Path): Result<Data, IOError> {
  let file = File.open(path)
  defer file.close()  // Consumes file at end of scope
  let data = file.readAll()
  return Ok(data)
}
```

### 7.2 Borrowing

For non-linear access to linear resources, use borrowing:

```protolang
linear function processConnection(conn: Connection): Response {
  // `&conn` borrows conn immutably (does not consume)
  let headers = parseHeaders(&conn)

  // `&mut conn` borrows conn mutably (does not consume, but allows mutation)
  let body = readBody(&mut conn)

  // Must still consume conn before returning
  conn.close()
  return Response.new(headers, body)
}
```

### 7.3 Linear Types in Generics

```protolang
// A channel that consumes values (linear send)
interface LinearChannel<T> {
  linear function send(self, value: T): void
  function receive(): Option<T>
  linear function close(self): void
}
```

### 7.4 Memory Management

ProtoLang uses **automatic memory management** (reference counting + generational GC) for non-linear types. Linear types are deallocated immediately upon consumption, with no GC overhead.

---

## 8. Type-Safe Database Queries

### 8.1 Query Syntax

SQL-like syntax that is parsed and type-checked at compile time against the database schema.

```protolang
// Schema is loaded at compile time from the database or a schema file
schema "postgresql://localhost:5432/mydb" as MyDB

// Type-safe query
function getActiveUsers(since: DateTime): List<User> {
  return query MyDB {
    SELECT u.id, u.name, u.email, u.created_at
    FROM users u
    WHERE u.status = 'active'
    AND u.created_at > :since
    ORDER BY u.created_at DESC
    LIMIT 100
  }
}

// The compiler verifies:
// - Table 'users' exists
// - Columns 'id', 'name', 'email', 'status', 'created_at' exist
// - 'status' is comparable with string literal 'active'
// - 'created_at' is comparable with DateTime
// - Return type matches selected columns
```

### 8.2 Query Results as Types

```protolang
// Ad-hoc record type from query
function getUserSummaries(): List<{name: string, email: string}> {
  return query MyDB {
    SELECT name, email FROM users
  }
}

// JOIN with automatic foreign key resolution
function getOrdersWithUsers(): List<{order: Order, user: User}> {
  return query MyDB {
    SELECT o.*, u.*
    FROM orders o
    JOIN users u ON o.user_id = u.id
  }
}
```

### 8.3 Migrations and Schema Evolution

```protolang
// Schema versions are tracked
schema MyDB {
  version 1 {
    table users {
      id: UUID primary key,
      name: string not null,
      email: string not null
    }
  }

  version 2 {
    alter table users {
      add column phone: Option<string>
    }
  }
}

// The compiler warns if queries reference deprecated columns
```

### 8.4 Query Composition

```protolang
// Queries are composable
let baseQuery = query MyDB {
  SELECT * FROM users WHERE status = 'active'
}

let paginated = baseQuery 
  |> orderBy(.created_at, Desc)
  |> limit(100)
  |> offset(200)
```

---

## 9. State-Dependent Types (Protocol Types)

### 9.1 Protocol Definition

Define valid state transitions as part of the type:

```protolang
protocol Connection {
  // States
  state Closed
  state Connected {
    remoteAddr: SocketAddr
    localAddr: SocketAddr
  }
  state Listening {
    port: uint16
  }
  state Error {
    code: int32
    message: string
  }

  // Transitions
  transition Closed -> Connected via connect(addr: SocketAddr)
  transition Closed -> Listening via listen(port: uint16)
  transition Connected -> Closed via close()
  transition Listening -> Closed via close()
  transition any -> Error via fail(reason: string)

  // State-specific methods
  in Connected {
    function send(data: bytes): Result<void, IOError>
    function receive(): Result<bytes, IOError>
  }

  in Listening {
    function accept(): Result<Connection<Connected>, IOError>
  }
}
```

### 9.2 Usage

```protolang
let conn = Connection()           // type: Connection<Closed>
let connected = conn.connect(addr) // type: Connection<Connected>
connected.send(data)              // OK
connected.close()                 // type: Connection<Closed>

// Compile error: cannot call 'send' on Connection<Closed>
closed.send(data)                 // ERROR!

// Compile error: cannot call 'accept' on Connection<Connected>
connected.accept()                // ERROR!
```

### 9.3 Protocol Composition

```protolang
protocol FileHandle {
  state Open { path: Path, position: uint64 }
  state Closed

  transition Closed -> Open via open(path: Path)
  transition Open -> Closed via close()

  in Open {
    function read(buf: &mut bytes): Result<uint64, IOError>
    function seek(pos: uint64): Result<void, IOError>
  }
}

// Combine protocols
protocol TransactionalFile = FileHandle + Transactional {
  // FileHandle that also supports transactions
}
```

### 9.4 Session Types (Protocol Verification)

For communication protocols, use session types to verify that both endpoints follow the same protocol:

```protolang
protocol BuyerSeller {
  // Buyer sends title, Seller sends price
  Buyer: send string
  Seller: receive string

  Seller: send Money
  Buyer: receive Money

  Buyer: choice {
    Accept: send Address, Seller: receive Address, Seller: send Date, Buyer: receive Date, end
    Reject: end
  }
}

// The compiler verifies that buyer and seller implementations are dual
```

---

## 10. Configuration as Code

### 10.1 Configuration Types

```protolang
// Configuration is defined as typed records
config DatabaseConfig {
  host: string = "localhost"
  port: uint16 = 5432
  poolSize: PositiveInt = 10
  ssl: bool = true
}

config FeatureFlags {
  newSearchAlgorithm: bool = false
  maxResults: PositiveInt = 100
  allowedRegions: Set<Region> = [Region.US, Region.EU]
}

config ServiceConfig {
  name: NonEmptyString
  port: uint16
  db: DatabaseConfig
  features: FeatureFlags
}
```

### 10.2 Environment-Specific Values

```protolang
config ServiceConfig {
  name = "order-service"
  port = 8080

  db {
    host = env("DB_HOST") ?? "localhost"
    port = env("DB_PORT")?.parse<uint16>() ?? 5432
    poolSize = env("DB_POOL_SIZE")?.parse<PositiveInt>() ?? 10
  }

  features {
    newSearchAlgorithm = env("FF_NEW_SEARCH") == "true"
  }
}
```

### 10.3 Validation

```protolang
config ServiceConfig {
  validate {
    assert port > 1024, "Port must be > 1024 (non-privileged)"
    assert db.poolSize <= 100, "Pool size too large"
    assert features.allowedRegions.notEmpty(), "Must allow at least one region"
  }
}
```

### 10.4 Compile-Time Checking

```protolang
// All configuration is resolved at compile time where possible
// The compiler checks that all required env vars are documented
// and that all default values are valid

let config = loadConfig<ServiceConfig>()
// Type: ServiceConfig — all fields are guaranteed to be present and valid
```

---

## 11. Built-in Observability

### 11.1 Automatic Instrumentation

Every effectful operation automatically generates telemetry:

```protolang
// No manual tracing needed
effect(network) function fetchUser(id: UUID): User {
  let dbUser = db.query("SELECT * FROM users WHERE id = ?", id)
    // Auto-span: "db.query" with query text, duration, row count

  let profile = http.get("/profiles/" + id)
    // Auto-span: "http.get" with URL, status code, duration

  return merge(dbUser, profile)
    // Auto-span: "merge" (pure function, minimal)
}
```

### 11.2 Custom Spans

```protolang
function processOrder(order: Order): Receipt {
  span "process_order" {
    attribute order.id
    attribute order.total

    let payment = chargePayment(order)
    let inventory = reserveInventory(order)
    let receipt = generateReceipt(payment, inventory)

    return receipt
  }
}
```

### 11.3 Automatic Context Propagation

Trace context, user ID, and request ID propagate automatically across:
- Async boundaries (await, background tasks)
- Guardian boundaries (distributed calls)
- Database queries (automatically added as comments for query correlation)
- HTTP calls (automatically added as headers: `X-Trace-Id`, `X-Request-Id`)

### 11.4 Metrics

```protolang
// Automatic metrics from function calls
counter orders.created
gauge orders.pending
histogram payment.latency

function createOrder(request: OrderRequest): Order {
  orders.created.increment()
  orders.pending.increment()

  let order = db.insert(Order.new(request))

  orders.pending.decrement()
  return order
}
```

### 11.5 Structured Logging

```protolang
function processPayment(order: Order): Result {
  log.info("Processing payment", {
    orderId: order.id,
    amount: order.total,
    currency: order.currency
  })

  // Logs are automatically correlated with the current trace
  // and output as structured JSON
}
```

---

## 12. Exhaustive Error Handling

### 12.1 Error Types

Every function that can fail declares its error types in the signature:

```protolang
function divide(a: int, b: int): int throws DivisionByZero {
  if b == 0 {
    throw DivisionByZero { dividend: a }
  }
  return a / b
}

function parseDate(input: string): DateTime throws ParseError {
  // ...
}
```

### 12.2 Exhaustive Handling

The compiler forces the caller to handle every possible error:

```protolang
// Method 1: try/catch with exhaustive cases
try {
  let result = divide(10, 0)
} catch DivisionByZero(e) {
  print("Cannot divide by zero: " + e.dividend)
}
// Compile error if any error type is missing

// Method 2: Result type (for functional style)
let result = divide(10, 0).toResult()
match result {
  Ok(value) => print(value),
  Err(DivisionByZero(e)) => print("Error: " + e)
}

// Method 3: Propagation with `?` operator
function calculate(a: int, b: int): int throws DivisionByZero {
  let x = divide(a, b)?  // Propagates DivisionByZero to caller
  let y = divide(b, a)?
  return x + y
}
```

### 12.3 Error Composition

```protolang
function processOrder(order: Order): Receipt 
  throws NetworkError, DBError, ValidationError {

  let user = fetchUser(order.userId)?        // may throw NetworkError
  let saved = db.save(order)?                // may throw DBError
  let receipt = validate(saved)?             // may throw ValidationError
  return receipt
}

// Caller must handle ALL three error types
```

### 12.4 Error Sets

```protolang
// Define reusable error sets
type OrderErrors = NetworkError | DBError | ValidationError

function processOrder(order: Order): Receipt throws OrderErrors {
  // ...
}
```

### 12.5 Panics vs. Errors

- **Errors** (`throws`): Recoverable, expected, part of the type system.
- **Panics** (`panic`): Unrecoverable, bugs, abort the current action.

```protolang
function getConfig(key: string): string {
  return config.get(key) ?? panic("Missing required config: " + key)
  // Panic aborts the current action and triggers recovery
}
```

---

## 13. Data Race Freedom

### 13.1 Ownership Model

ProtoLang uses a simplified ownership model compared to Rust:

```protolang
// Unique ownership: only one reference, can mutate
let mut data = Vec.new()
data.push(1)  // OK: we have unique ownership

// Shared ownership: multiple references, read-only
let shared = &data
let shared2 = &data  // OK: multiple immutable borrows
// shared.push(2)  // ERROR: cannot mutate through shared reference

// Mutable borrow: exclusive access, can mutate
let borrowed = &mut data
borrowed.push(2)  // OK
// let borrowed2 = &mut data  // ERROR: cannot have two mutable borrows
```

### 13.2 Thread Safety by Type

Types are automatically classified as `Send` (safe to move between threads) and `Sync` (safe to share between threads):

```protolang
// Automatically derived
impl Send for User  // User contains only Send fields
impl Sync for Config  // Config is immutable

// Not Send: contains a thread-local reference
struct ThreadLocalCache {
  data: Map<string, string>  // Not Send because Map uses thread-local allocator
}
```

### 13.3 Concurrent Primitives

```protolang
// Channels (inspired by Go, but typed and linear)
let (sender, receiver) = Channel<string>.new()

background {
  sender.send("hello")  // Linear send consumes sender
}

let msg = receiver.receive()  // "hello"

// Mutex with compile-time lock checking
let data = Mutex.new(Vec.new())
{
  let guard = data.lock()  // guard is a scoped lock
  guard.push(1)            // OK
}  // Lock automatically released
// guard.push(2)  // ERROR: guard is out of scope

// Atomics for lock-free programming
let counter = AtomicInt64.new(0)
counter.fetchAdd(1)
```

### 13.4 Actor Model (Optional)

For high-concurrency scenarios, ProtoLang supports actors:

```protolang
actor OrderProcessor {
  state orders: Map<OrderId, Order>

  message process(order: Order) {
    this.orders.put(order.id, order)
  }

  message get(id: OrderId): Option<Order> {
    return this.orders.get(id)
  }
}

// Actors are single-threaded internally, so no data races
let processor = OrderProcessor.new()
processor.send(process(order))
let result = processor.ask(get(orderId))
```

---

## 14. Feature Flags & Experiments

### 14.1 Feature Flags as Types

```protolang
featureFlag newSearchAlgorithm: bool = false
featureFlag maxResults: PositiveInt = 100
featureFlag allowedRegions: Set<Region> = [Region.US, Region.EU]
```

### 14.2 Usage

```protolang
function search(query: string): List<Result> {
  if featureFlags.newSearchAlgorithm {
    return searchV2(query)
  } else {
    return searchV1(query)
  }
}

// The compiler knows all feature flags at compile time
// It can optimize the branch if the flag is statically known
```

### 14.3 Experiments

```protolang
experiment SearchAlgorithmExperiment {
  control: searchV1(query)
  treatment: searchV2(query)

  // Experiment configuration
  duration: 30 days
  sampleRate: 0.1  // 10% of traffic

  // Success criteria
  metric clickThroughRate {
    target: +5%
    minSampleSize: 10000
  }

  metric latency {
    target: < 100ms p99
  }
}

// Usage
let results = experiment SearchAlgorithmExperiment(query)
```

### 14.4 Automatic Cleanup

When an experiment concludes:
1. The compiler inlines the winning variant as the default.
2. The losing variant's code is marked as deprecated.
3. After a grace period, the losing variant is deleted.
4. All experiment infrastructure (tracking, bucketing) is removed.

```protolang
// After experiment concludes, this becomes:
function search(query: string): List<Result> {
  return searchV2(query)  // Winning variant inlined
}
```

---

## 15. Implicit Context Propagation

### 15.1 Context Definition

```protolang
context RequestContext {
  traceId: UUID
  spanId: UUID
  userId: Option<UUID>
  requestId: UUID
  logger: Logger
  deadline: Option<DateTime>
}
```

### 15.2 Automatic Propagation

```protolang
function processOrder(order: Order): Receipt {
  // All of these are automatically available
  log.info("Processing order", { orderId: order.id })

  // Propagates across async boundaries
  let user = background {
    fetchUser(order.userId)  // RequestContext automatically passed
  }

  // Propagates across guardian boundaries
  let payment = paymentService.charge(order)  // Context serialized with call

  return Receipt.new(order, payment)
}
```

### 15.3 Context Modification

```protolang
function subOperation(): Result {
  with context { 
    // Modify context for this scope and all children
    logger = logger.withField("subOperation", true)
    deadline = context.deadline?.minus(5s)
  } {
    return doWork()
  }
}
```

### 15.4 Context Safety

The compiler ensures that context values are `Send` and `Sync` so they can safely propagate across threads and processes.

---

## 16. Runtime Architecture

### 16.1 Overview

The ProtoLang runtime consists of:

1. **Language VM:** Executes compiled bytecode with support for continuations, effects, and linear types.
2. **Guardian Runtime:** Manages distributed objects, write-ahead logging, and crash recovery.
3. **Effect Scheduler:** Event loop for async I/O with work-stealing thread pool.
4. **Transaction Coordinator:** Implements 2PC, Saga, and Paxos Commit protocols.
5. **Observability Agent:** Auto-instruments effects and exports telemetry.
6. **Context Propagator:** Manages implicit context across thread and process boundaries.

### 16.2 Guardian Runtime

```
┌─────────────────────────────────────┐
│           Guardian Process          │
├─────────────────────────────────────┤
│  ┌─────────┐  ┌─────────────────┐  │
│  │  WAL    │  │  Stable Storage │  │
│  │  Log    │  │  (RocksDB/etc)  │  │
│  └─────────┘  └─────────────────┘  │
├─────────────────────────────────────┤
│  ┌─────────┐  ┌─────────────────┐  │
│  │ Action  │  │  Volatile Heap  │  │
│  │ Manager │  │  (Regular GC)   │  │
│  └─────────┘  └─────────────────┘  │
├─────────────────────────────────────┤
│  ┌─────────┐  ┌─────────────────┐  │
│  │ 2PC     │  │  Network I/O    │  │
│  │ Coord.  │  │  (Async)        │  │
│  └─────────┘  └─────────────────┘  │
└─────────────────────────────────────┘
```

### 16.3 Durable Execution State Machine

Every `durable` function is compiled to a state machine:

```
State 0: Entry
  → Checkpoint locals
  → Call effect 1
  → Transition to State 1

State 1: After Effect 1
  → Restore locals from checkpoint
  → Process result
  → Checkpoint locals
  → Call effect 2
  → Transition to State 2

State 2: After Effect 2
  → Restore locals
  → Return result
```

On crash, the runtime reads the last checkpoint from the WAL and resumes from the corresponding state.

### 16.4 Memory Layout

```
┌─────────────────┐
│   Stack Frame   │  (per function call)
├─────────────────┤
│  Return Address │
│  Saved Registers│
│  Local Variables│
│  Effect Chain   │  (linked list of active effects)
│  Linear Set     │  (set of linear resources in scope)
└─────────────────┘

┌─────────────────┐
│   Heap Objects  │
├─────────────────┤
│  Header (type)  │
│  Ref Count      │  (for non-linear objects)
│  Linear Flag    │  (for linear objects)
│  Data           │
└─────────────────┘
```

---

## 17. Compilation Pipeline

### 17.1 Stages

```
Source Code
    │
    ▼
┌──────────────┐
│   Lexer      │  → Tokens
└──────────────┘
    │
    ▼
┌──────────────┐
│   Parser     │  → AST
└──────────────┘
    │
    ▼
┌──────────────┐
│  Schema Load │  → DB schemas, config schemas
└──────────────┘
    │
    ▼
┌──────────────┐
│ Type Checker │  → Typed AST
│  + Effects   │  (effect inference, linearity check, null safety)
└──────────────┘
    │
    ▼
┌──────────────┐
│ State Machine│  → Transform async/durable to state machines
│  Transformer │
└──────────────┘
    │
    ▼
┌──────────────┐
│  Optimizer   │  → Optimized IR
└──────────────┘
    │
    ▼
┌──────────────┐
│  Code Gen    │  → Bytecode / Machine Code
└──────────────┘
    │
    ▼
┌──────────────┐
│  Linker      │  → Executable
└──────────────┘
```

### 17.2 Type Checking Rules

**Effect Subsumption:**
```
Γ ⊢ e : T ! E    E ⊆ E'
─────────────────────────
Γ ⊢ e : T ! E'
```

**Linearity Check:**
```
Γ, x : linear T ⊢ e : U ! E    x consumed exactly once in e
─────────────────────────────────────────────────────────────
Γ ⊢ let x = v in e : U ! E
```

**Null Safety:**
```
Γ ⊢ e : Option<T>
Γ, x : T ⊢ e1 : U ! E
Γ ⊢ e2 : U ! E
─────────────────────────────────────
Γ ⊢ match e { Some(x) => e1, None => e2 } : U ! E
```

**State Transition:**
```
Γ ⊢ obj : Protocol<State S>
Protocol has transition S -> S' via method M
─────────────────────────────────────────────
Γ ⊢ obj.M() : Protocol<State S'>
```

### 17.3 Bytecode Format

ProtoLang compiles to a stack-based bytecode with special instructions for:
- `EFFECT_ENTER` / `EFFECT_EXIT` — Push/pop effect frames
- `LINEAR_ACQUIRE` / `LINEAR_CONSUME` — Track linear resources
- `CHECKPOINT` — Serialize continuation to WAL
- `CONTEXT_GET` / `CONTEXT_SET` — Access implicit context
- `SPAN_START` / `SPAN_END` — Observability
- `ACTION_BEGIN` / `ACTION_COMMIT` / `ACTION_ABORT` — Transactions

---

## 18. Standard Library Design

### 18.1 Core Modules

```protolang
import std.collections.{List, Map, Set}
import std.io.{File, Path}
import std.net.{Http, Tcp, Udp}
import std.db.{Connection, Query, Transaction}
import std.crypto.{Hash, Cipher}
import std.time.{DateTime, Duration, Timer}
import std.json.{Json, Encoder, Decoder}
import std.log.{Logger, Level}
import std.metrics.{Counter, Gauge, Histogram}
import std.testing.{Test, Assert}
```

### 18.2 HTTP Server Example

```protolang
import std.net.http.{Server, Request, Response, Router}
import std.json

// Define routes with automatic parameter extraction and validation
let router = Router.new()
  .get("/users/:id", getUser)
  .post("/orders", createOrder)
  .get("/health", healthCheck)

// Route handler: types are automatically validated from path/query/body
function getUser(req: Request, ctx: RouteContext): Response {
  let id = req.pathParam<UUID>("id")?        // Auto-parsed, auto-validated
  let includeDeleted = req.queryParam<bool>("include_deleted") ?? false

  let user = userService.findUser(id, includeDeleted)?

  return Response.ok(json.encode(user))
}

// Server with automatic observability
let server = Server.new(router)
  .withPort(8080)
  .withMiddleware(loggingMiddleware)
  .withMiddleware(tracingMiddleware)
  .withMiddleware(authMiddleware)

server.start()
```

### 18.3 Database Module

```protolang
import std.db

// Connection pool managed by the runtime
let pool = db.Pool.new(config.db)

// Type-safe queries
function getUser(id: UUID): Option<User> {
  return pool.query {
    SELECT * FROM users WHERE id = :id
  }.first()
}

// Transactions are automatic or explicit
function transferFunds(from: AccountId, to: AccountId, amount: Money): Result {
  return pool.transaction {
    let fromAccount = db.query {
      SELECT * FROM accounts WHERE id = :from FOR UPDATE
    }.first() ?? return Err(AccountNotFound(from))

    let toAccount = db.query {
      SELECT * FROM accounts WHERE id = :to FOR UPDATE
    }.first() ?? return Err(AccountNotFound(to))

    if fromAccount.balance < amount {
      return Err(InsufficientFunds)
    }

    db.execute {
      UPDATE accounts SET balance = balance - :amount WHERE id = :from
    }
    db.execute {
      UPDATE accounts SET balance = balance + :amount WHERE id = :to
    }

    return Ok(())
  }
}
```

---

## 19. Example: Complete Application

### 19.1 E-Commerce Order Service

```protolang
// ============================================
// ProtoLang Reference Implementation: Order Service
// ============================================

// ─── Configuration ──────────────────────────
config ServiceConfig {
  name: NonEmptyString = "order-service"
  port: uint16 = 8080

  db: DatabaseConfig {
    host: string = env("DB_HOST") ?? "localhost"
    port: uint16 = env("DB_PORT")?.parse() ?? 5432
    name: string = "orders"
  }

  features: FeatureFlags {
    newPaymentGateway: bool = env("FF_NEW_PAYMENT") == "true"
    enableNotifications: bool = true
  }
}

// ─── Schema ─────────────────────────────────
schema "postgresql://localhost:5432/orders" as OrderDB

// ─── Domain Types ───────────────────────────
record Order {
  id: OrderId,
  userId: UserId,
  items: List<OrderItem>,
  total: Money,
  status: OrderStatus,
  createdAt: DateTime
}

variant OrderStatus {
  Pending,
  Paid { paymentId: PaymentId },
  Shipped { trackingNumber: string },
  Delivered { deliveredAt: DateTime },
  Canceled { reason: string }
}

record OrderItem {
  productId: ProductId,
  quantity: PositiveInt,
  unitPrice: Money
}

// ─── Protocol Types ─────────────────────────
protocol PaymentGateway {
  state Idle
  state Processing { transactionId: string }
  state Completed { receipt: PaymentReceipt }
  state Failed { error: PaymentError }

  transition Idle -> Processing via charge(amount: Money, orderId: OrderId)
  transition Processing -> Completed via confirm()
  transition Processing -> Failed via decline(reason: string)
  transition any -> Idle via reset()
}

// ─── Guardians ──────────────────────────────
guardian OrderService {
  stable orders: Map<OrderId, Order>
  stable payments: Map<OrderId, PaymentId>
  stable nextOrderId: int64 = 0

  volatile cache: LRUCache<OrderId, Order>
  volatile metrics: MetricsCollector

  recover {
    this.cache = LRUCache.new(10000)
    this.metrics = MetricsCollector.new("order_service")
    for order in this.orders.values() {
      this.cache.put(order.id, order)
    }
  }

  // ─── Public API ───────────────────────────
  public function createOrder(request: CreateOrderRequest): Order 
    throws ValidationError, InventoryError {

    // Validate
    if request.items.isEmpty() {
      throw ValidationError { field: "items", message: "Cannot be empty" }
    }

    // Calculate total
    let total = request.items.fold(Money.zero(), (sum, item) => {
      let product = inventoryService.getProduct(item.productId)?
      if product.stock < item.quantity {
        throw InventoryError { productId: item.productId, available: product.stock }
      }
      return sum + (product.price * item.quantity)
    })

    // Create order
    let orderId = OrderId(this.nextOrderId)
    this.nextOrderId += 1

    let order = Order {
      id: orderId,
      userId: request.userId,
      items: request.items,
      total: total,
      status: OrderStatus.Pending,
      createdAt: DateTime.now()
    }

    this.orders.put(orderId, order)
    this.cache.put(orderId, order)

    log.info("Order created", { orderId: orderId, total: total })
    metrics.counter("orders.created").increment()

    return order
  }

  public function processPayment(orderId: OrderId, paymentMethod: PaymentMethod): Order 
    throws OrderNotFound, PaymentError, NetworkError {

    let order = this.orders.get(orderId) ?? throw OrderNotFound(orderId)

    // Use feature flag for payment gateway selection
    let gateway = if featureFlags.newPaymentGateway {
      newPaymentGateway()
    } else {
      legacyPaymentGateway()
    }

    // Durable payment processing
    let receipt = durable {
      let processing = gateway.charge(order.total, orderId)
      let confirmed = processing.confirm() 
        with timeout = 30s
        with retry = exponential(1s, 10s, 3)
      return confirmed.receipt
    }

    // Update order atomically
    topaction {
      let updated = order.withStatus(OrderStatus.Paid { paymentId: receipt.id })
      this.orders.put(orderId, updated)
      this.cache.put(orderId, updated)
      this.payments.put(orderId, receipt.id)

      if featureFlags.enableNotifications {
        notificationService.sendPaymentConfirmation(updated, receipt)
      }
    }

    log.info("Payment processed", { orderId: orderId, paymentId: receipt.id })
    metrics.counter("orders.paid").increment()
    metrics.histogram("payment.latency").record(receipt.duration)

    return this.orders.get(orderId).unwrap()
  }

  public function fulfillOrder(orderId: OrderId): Order 
    throws OrderNotFound, ShippingError {

    let order = this.orders.get(orderId) ?? throw OrderNotFound(orderId)

    // Saga pattern for fulfillment
    open action {
      step reserveInventory(order.items)
        compensate releaseInventory(order.items)

      step createShipment(order)
        compensate cancelShipment(order)

      step updateOrderStatus(orderId, OrderStatus.Shipped { 
        trackingNumber: generateTrackingNumber() 
      })
    }

    return this.orders.get(orderId).unwrap()
  }

  public function getOrder(id: OrderId): Option<Order> {
    // Check cache first
    if let Some(cached) = this.cache.get(id) {
      metrics.counter("orders.cache_hit").increment()
      return Some(cached)
    }

    // Fall back to stable storage
    let order = this.orders.get(id)
    if let Some(o) = order {
      this.cache.put(id, o)
    }
    return order
  }
}

// ─── HTTP API ───────────────────────────────
import std.net.http.{Server, Request, Response}

let router = Router.new()
  .post("/orders", createOrderHandler)
  .post("/orders/:id/pay", payOrderHandler)
  .post("/orders/:id/fulfill", fulfillOrderHandler)
  .get("/orders/:id", getOrderHandler)

function createOrderHandler(req: Request): Response {
  let request = req.bodyJson<CreateOrderRequest>()?
  let order = orderService.createOrder(request)?
  return Response.created(json.encode(order))
}

function payOrderHandler(req: Request): Response {
  let orderId = req.pathParam<OrderId>("id")?
  let payment = req.bodyJson<PaymentRequest>()?
  let order = orderService.processPayment(orderId, payment.method)?
  return Response.ok(json.encode(order))
}

function fulfillOrderHandler(req: Request): Response {
  let orderId = req.pathParam<OrderId>("id")?
  let order = orderService.fulfillOrder(orderId)?
  return Response.ok(json.encode(order))
}

function getOrderHandler(req: Request): Response {
  let orderId = req.pathParam<OrderId>("id")?
  match orderService.getOrder(orderId) {
    Some(order) => Response.ok(json.encode(order)),
    None => Response.notFound()
  }
}

// ─── Main Entry Point ───────────────────────
function main(): void {
  let config = loadConfig<ServiceConfig>()

  log.info("Starting service", { name: config.name, port: config.port })

  let server = Server.new(router)
    .withPort(config.port)
    .withRequestTimeout(30s)
    .withMaxConnections(10000)

  // Graceful shutdown
  onSignal(SIGTERM) {
    log.info("Shutting down gracefully...")
    server.stop()
    orderService.shutdown()
  }

  server.start()
}
```

---

## Appendix A: Grammar Summary

```ebnf
program        ::= module* declaration*
module         ::= "import" path ("as" identifier)?
path           ::= identifier ("." identifier)*

declaration    ::= functionDecl
                 | recordDecl
                 | variantDecl
                 | guardianDecl
                 | protocolDecl
                 | configDecl
                 | featureFlagDecl
                 | experimentDecl
                 | typeAlias

functionDecl   ::= visibility? effect* "function" identifier 
                   "(" params ")" (":" type)? ("throws" typeList)?
                   block

visibility     ::= "public" | "private" | "internal"
effect         ::= "pure" | "mutates" | "io" | "network" | "async" 
                 | "linear" | "durable" | "sync"

params         ::= param ("," param)*
param          ::= identifier ":" type ("=" expression)?

type           ::= primitive
                 | identifier
                 | type "<" typeList ">"
                 | "function" "(" params ")" (":" type)? effect*
                 | "Option" "<" type ">"
                 | "Result" "<" type "," type ">"
                 | protocolRef

protocolRef    ::= identifier "<" identifier ">"

block          ::= "{" statement* "}"
statement      ::= letDecl
                 | expression
                 | returnStmt
                 | ifStmt
                 | matchStmt
                 | forStmt
                 | whileStmt
                 | tryStmt
                 | actionStmt
                 | topactionStmt
                 | openActionStmt

letDecl        ::= "let" pattern ("=" expression)?
pattern        ::= identifier
                 | "(" pattern ("," pattern)* ")"
                 | variantPattern

expression     ::= literal
                 | identifier
                 | expression "(" args ")"
                 | expression "." identifier
                 | expression "?"            // try/unwrap
                 | expression "??" expression // default
                 | expression "?." identifier // safe navigation
                 | "match" expression "{" matchArm* "}"
                 | "if" expression block ("else" block)?
                 | "all" "{" expression* "}"
                 | "race" "{" expression* "}"
                 | "background" block
                 | "await" expression
                 | block

matchArm       ::= pattern "=>" expression

actionStmt     ::= "action" block
topactionStmt  ::= "topaction" block
openActionStmt ::= "open" "action" "{" step* "}"
step           ::= "step" expression ("compensate" expression)?

// ... (additional grammar rules)
```

---

## Appendix B: Effect System Formalization

### B.1 Effect Lattice

```
              ┌─────────────┐
              │   network   │
              └──────┬──────┘
                     │
              ┌──────┴──────┐
              │      io     │
              └──────┬──────┘
                     │
              ┌──────┴──────┐
              │   mutates   │
              └──────┬──────┘
                     │
              ┌──────┴──────┐
              │     pure    │
              └─────────────┘
```

### B.2 Effect Inference Rules

```
// Pure literal
─────────────────
Γ ⊢ n : int ! pure

// Variable reference
x : T ∈ Γ
───────────────
Γ ⊢ x : T ! pure

// Function application
Γ ⊢ f : (T1 -> T2) ! Ef    Γ ⊢ arg : T1 ! Ea
────────────────────────────────────────────────
Γ ⊢ f(arg) : T2 ! (Ef ∪ Ea ∪ Ecall)

where Ecall = effects of the function body

// Effect polymorphism
Γ ⊢ e : T ! E    E ⊆ E'
─────────────────────────
Γ ⊢ e : T ! E'
```

---

## Appendix C: Runtime WAL Format

```protobuf
message WALEntry {
  uint64 sequence_number = 1;
  uint64 timestamp = 2;
  string guardian_id = 3;

  oneof entry {
    ActionBegin action_begin = 10;
    ActionCommit action_commit = 11;
    ActionAbort action_abort = 12;
    StepComplete step_complete = 13;
    CompensationRun compensation_run = 14;
    Checkpoint checkpoint = 20;
    ContextUpdate context_update = 30;
  }
}

message ActionBegin {
  string action_id = 1;
  string parent_action_id = 2;
  repeated string participant_guardians = 3;
}

message Checkpoint {
  string continuation_id = 1;
  bytes serialized_stack = 2;
  bytes serialized_locals = 3;
  uint32 next_state = 4;
}
```

---

## Appendix D: Comparison with Existing Languages

| Feature | ProtoLang | Rust | Go | Java | TypeScript | Kotlin |
|---------|-----------|------|-----|------|------------|--------|
| Effect System | ✅ Native | ❌ | ❌ | ❌ | ❌ | ❌ |
| Null Safety | ✅ Default | ✅ | ❌ | Partial | Partial | ✅ |
| Linear Types | ✅ Selective | ✅ All | ❌ | ❌ | ❌ | ❌ |
| Async Without Coloring | ✅ | ❌ | Partial | ❌ | ❌ | ❌ |
| Type-Safe SQL | ✅ Native | Library | ❌ | Library | ❌ | Library |
| State-Dependent Types | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Durable Execution | ✅ Native | ❌ | ❌ | ❌ | ❌ | ❌ |
| Built-in Observability | ✅ | Library | Library | Library | Library | Library |
| Exhaustive Errors | ✅ | ✅ | Partial | Partial | ❌ | ❌ |
| Data Race Freedom | ✅ | ✅ | Partial | ❌ | ❌ | ❌ |
| Config as Code | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Feature Flags | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Implicit Context | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |

---

*End of Reference Implementation Specification*
