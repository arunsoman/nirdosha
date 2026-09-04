# ProtoLang `std.io` Package Specification
## Unified I/O: Files, Network, Database, and Streams as First-Class Language Constructs

**Version:** 0.1.0-draft  
**Date:** 2026-08-21  
**Status:** Language Package Specification

---

## Table of Contents

1. [Design Philosophy](#1-design-philosophy)
2. [Core I/O Abstractions](#2-core-io-abstractions)
3. [File I/O](#3-file-io)
4. [Network I/O](#4-network-io)
5. [HTTP / HTTPS](#5-http--https)
6. [WebSockets](#6-websockets)
7. [Database / SQL as I/O](#7-database--sql-as-io)
8. [Pipes & Process I/O](#8-pipes--process-io)
9. [Streaming & Backpressure](#9-streaming--backpressure)
10. [Serialization I/O](#10-serialization-io)
11. [Resource Pools & Connection Management](#11-resource-pools--connection-management)
12. [TLS / SSL](#12-tls--ssl)
13. [I/O Observability](#13-io-observability)
14. [Error Model](#14-error-model)
15. [Complete Examples](#15-complete-examples)

---

## 1. Design Philosophy

### 1.1 Everything External is I/O

In ProtoLang, **I/O is any operation that crosses the process boundary** — whether to a file, a network socket, a database, a pipe, or even another guardian. The `std.io` package unifies all of these under a single abstraction hierarchy.

```
┌─────────────────────────────────────────────────────────────┐
│                    std.io Abstraction                        │
├─────────────────────────────────────────────────────────────┤
│  I/O Effect Hierarchy:                                       │
│  ├── effect(io)        // Any external operation              │
│  ├── effect(file)      // File system operations              │
│  ├── effect(network)   // Network operations                  │
│  │   ├── effect(tcp)   // TCP socket operations               │
│  │   ├── effect(udp)   // UDP socket operations               │
│  │   ├── effect(http)  // HTTP operations                     │
│  │   ├── effect(ws)    // WebSocket operations                │
│  │   └── effect(tls)   // TLS operations                      │
│  ├── effect(db)        // Database operations                 │
│  │   ├── effect(sql)   // SQL query operations                │
│  │   └── effect(nosql) // NoSQL operations                    │
│  └── effect(pipe)      // Pipe / process I/O                │
└─────────────────────────────────────────────────────────────┘
```

### 1.2 Linear Resources

Every I/O handle is a **linear resource**. It must be consumed exactly once (typically via `close()` or by passing ownership to a pool). The compiler tracks this at compile time.

```protolang
// The compiler ensures `file` is consumed exactly once
linear function readConfig(path: Path): Result<Config, IOError> {
  let file = File.open(path)        // acquire linear resource
  defer file.close()                // consume via deferred close
  let content = file.readAll()      // borrow file immutably
  return parseConfig(content)
}
```

### 1.3 State-Dependent I/O

I/O resources have protocol-defined states. The compiler prevents invalid operations:

```protolang
let socket = TcpSocket()            // TcpSocket<Closed>
let connected = socket.connect(addr) // TcpSocket<Connected>
connected.send(data)                // OK
connected.close()                   // TcpSocket<Closed>
connected.send(data)                // COMPILE ERROR: send on Closed
```

### 1.4 Transparent Async

All I/O operations suspend transparently. No `async` keyword, no function coloring.

```protolang
function fetchData(url: string): bytes {
  // Suspends automatically while waiting for network
  return http.get(url).body
}

// Callers don't need to be "async"
function processUser(id: UUID): User {
  let profile = fetchData("/users/" + id)  // transparent suspension
  return parseUser(profile)
}
```

### 1.5 Unified Error Model

All I/O errors are typed, exhaustive, and carry structured context:

```protolang
function readFile(path: Path): bytes 
  throws FileNotFound | PermissionDenied | IOError | TimeoutError {
  // ...
}
```

### 1.6 Automatic Observability

Every I/O operation generates spans, metrics, and logs automatically. No manual instrumentation.

---

## 2. Core I/O Abstractions

### 2.1 The `Resource` Protocol

All I/O resources implement the `Resource` protocol:

```protolang
protocol Resource {
  state Open
  state Closed

  transition Open -> Closed via close()

  // Metadata about the resource
  in Open {
    function id(): ResourceId
    function isOpen(): bool = true
  }

  in Closed {
    function isOpen(): bool = false
  }
}
```

### 2.2 The `Reader` Protocol

```protolang
protocol Reader {
  // Read up to `len` bytes into `buf`
  // Returns number of bytes read (0 = EOF)
  effect(io) function read(buf: &mut bytes): Result<uint64, IOError>

  // Read exactly `len` bytes, or fail
  effect(io) function readExact(len: uint64): Result<bytes, IOError>

  // Read until delimiter
  effect(io) function readUntil(delimiter: bytes): Result<bytes, IOError>

  // Read all remaining bytes
  effect(io) function readAll(): Result<bytes, IOError>

  // Stream interface
  effect(io) function stream(): Stream<byte>
}
```

### 2.3 The `Writer` Protocol

```protolang
protocol Writer {
  // Write data, return bytes written
  effect(io) function write(data: bytes): Result<uint64, IOError>

  // Write all data, or fail
  effect(io) function writeAll(data: bytes): Result<void, IOError>

  // Flush buffered data
  effect(io) function flush(): Result<void, IOError>

  // Stream interface
  effect(io) function sink(): Sink<byte>
}
```

### 2.4 The `Closer` Protocol

```protolang
protocol Closer {
  // Close the resource (linear consumption)
  linear function close(self): void

  // Try to close, return error if already closed
  linear function tryClose(self): Result<void, IOError>
}
```

### 2.5 The `Seeker` Protocol

```protolang
protocol Seeker {
  enum Whence {
    Start,    // Offset from start of file
    Current,  // Offset from current position
    End       // Offset from end of file
  }

  effect(io) function seek(offset: int64, whence: Whence): Result<uint64, IOError>
  effect(io) function position(): Result<uint64, IOError>
}
```

### 2.6 Combined Protocols

```protolang
// A file implements Reader + Writer + Seeker + Closer + Resource
protocol FileHandle = Reader + Writer + Seeker + Closer + Resource

// A socket implements Reader + Writer + Closer + Resource
protocol Socket = Reader + Writer + Closer + Resource

// A read-only file implements Reader + Seeker + Closer + Resource
protocol ReadOnlyFile = Reader + Seeker + Closer + Resource
```

---

## 3. File I/O

### 3.1 The `File` Type

```protolang
protocol File {
  include FileHandle

  state Open {
    path: Path
    mode: FileMode
    position: uint64
  }
  state Closed

  // Creation
  transition Closed -> Open via open(path: Path, mode: FileMode)
  transition Closed -> Open via create(path: Path)
  transition Closed -> Open via append(path: Path)

  // Operations
  in Open {
    effect(file) function metadata(): Result<FileMetadata, IOError>
    effect(file) function setPermissions(perm: Permissions): Result<void, IOError>
    effect(file) function truncate(size: uint64): Result<void, IOError>
    effect(file) function sync(): Result<void, IOError>  // fsync
  }
}

enum FileMode {
  Read,           // "r"
  Write,          // "w" (truncate)
  Append,         // "a"
  ReadWrite,      // "r+"
  WriteRead,      // "w+"
  AppendRead,     // "a+"
  Create,         // "x" (exclusive create)
}

record FileMetadata {
  size: uint64,
  createdAt: DateTime,
  modifiedAt: DateTime,
  accessedAt: DateTime,
  permissions: Permissions,
  isDirectory: bool,
  isSymlink: bool
}

record Permissions {
  owner: PermissionBits,
  group: PermissionBits,
  other: PermissionBits
}

record PermissionBits {
  read: bool,
  write: bool,
  execute: bool
}
```

### 3.2 File Operations

```protolang
// Open a file (linear resource)
let file = File.open("/tmp/data.txt", FileMode.Read)?
defer file.close()

let content = file.readAll()?

// Write with automatic sync
let out = File.create("/tmp/output.txt")?
defer out.close()

out.writeAll("Hello, ProtoLang")?
out.sync()?  // Ensure data reaches disk

// Seek and read
let dataFile = File.open("/tmp/data.bin", FileMode.ReadWrite)?
defer dataFile.close()

dataFile.seek(1024, Whence.Start)?
let chunk = dataFile.readExact(512)?
```

### 3.3 Memory-Mapped Files

```protolang
protocol MemoryMap {
  include Resource

  state Mapped {
    addr: uintptr,
    size: uint64,
    mode: MapMode
  }
  state Unmapped

  transition Unmapped -> Mapped via map(file: &File, mode: MapMode)
  transition Mapped -> Unmapped via unmap()

  in Mapped {
    effect(file) function slice(): &mut bytes
    effect(file) function sync(): Result<void, IOError>
  }
}

enum MapMode {
  ReadOnly,
  ReadWrite,
  CopyOnWrite
}

// Usage
let file = File.open("/tmp/large.bin", FileMode.Read)?
let mmap = MemoryMap.map(&file, MapMode.ReadOnly)?
defer mmap.unmap()

let data = mmap.slice()
// Access `data` as a byte slice without read() syscalls
```

### 3.4 Directory Operations

```protolang
protocol Directory {
  include Resource

  state Open { path: Path }
  state Closed

  transition Closed -> Open via open(path: Path)
  transition Open -> Closed via close()

  in Open {
    effect(file) function entries(): Stream<DirEntry>
    effect(file) function createDir(name: string): Result<void, IOError>
    effect(file) function remove(name: string): Result<void, IOError>
    effect(file) function rename(from: string, to: string): Result<void, IOError>
  }
}

record DirEntry {
  name: string,
  path: Path,
  metadata: FileMetadata
}

// Usage
let dir = Directory.open("/tmp")?
defer dir.close()

for entry in dir.entries() {
  log.info("Found", { name: entry.name, size: entry.metadata.size })
}
```

### 3.5 Path Operations

```protolang
record Path {
  segments: List<string>
}

function Path.join(a: Path, b: Path): Path
function Path.parent(p: Path): Option<Path>
function Path.fileName(p: Path): Option<string>
function Path.extension(p: Path): Option<string>
function Path.isAbsolute(p: Path): bool
function Path.exists(p: Path): effect(file) bool
function Path.isFile(p: Path): effect(file) bool
function Path.isDir(p: Path): effect(file) bool

// Platform-specific paths
let temp = Path.tempDir()
let home = Path.homeDir()
let cwd = Path.currentDir()
```

### 3.6 Temporary Files

```protolang
// Automatically deleted when closed
linear function createTempFile(prefix: string, suffix: string): Result<TempFile, IOError>

protocol TempFile {
  include File

  // On close, the file is automatically deleted
  linear function close(self): void

  // Persist the temp file to a permanent location
  linear function persist(self, target: Path): Result<File, IOError>
}

// Usage
let temp = createTempFile("upload", ".tmp")?
defer temp.close()  // Deleted automatically

temp.writeAll(uploadData)?
let permanent = temp.persist("/data/final.bin")?
// `temp` is consumed, `permanent` is a regular File
```

---

## 4. Network I/O

### 4.1 TCP Sockets

```protolang
protocol TcpSocket {
  include Socket

  state Closed
  state Bound { localAddr: SocketAddr }
  state Listening { localAddr: SocketAddr, backlog: uint32 }
  state Connected { 
    localAddr: SocketAddr, 
    remoteAddr: SocketAddr,
    keepAlive: bool,
    noDelay: bool
  }
  state Error { code: int32, message: string }

  // Client transitions
  transition Closed -> Connected via connect(addr: SocketAddr)

  // Server transitions
  transition Closed -> Bound via bind(addr: SocketAddr)
  transition Bound -> Listening via listen(backlog: uint32)
  transition Listening -> Connected via accept()

  // Common transitions
  transition any -> Closed via close()
  transition any -> Error via fail(code: int32, message: string)

  in Connected {
    effect(tcp) function setKeepAlive(enabled: bool): Result<void, IOError>
    effect(tcp) function setNoDelay(enabled: bool): Result<void, IOError>
    effect(tcp) function setReadTimeout(timeout: Duration): Result<void, IOError>
    effect(tcp) function setWriteTimeout(timeout: Duration): Result<void, IOError>
    effect(tcp) function peek(buf: &mut bytes): Result<uint64, IOError>
    effect(tcp) function shutdown(how: ShutdownMode): Result<void, IOError>
  }

  in Listening {
    effect(tcp) function accept(): Result<TcpSocket<Connected>, IOError>
  }
}

enum ShutdownMode {
  Read,   // Shut down read half
  Write,  // Shut down write half
  Both    // Shut down both halves
}

record SocketAddr {
  ip: IPAddr,
  port: uint16
}

variant IPAddr {
  V4 { octets: [uint8; 4] },
  V6 { octets: [uint8; 16], scopeId: uint32 }
}
```

### 4.2 TCP Client

```protolang
// Simple connection
let socket = TcpSocket()
let conn = socket.connect(SocketAddr { ip: IPAddr.V4([127, 0, 0, 1]), port: 8080 })?
defer conn.close()

conn.writeAll("Hello")?
let response = conn.readAll()?

// With timeouts and options
let conn2 = TcpSocket()
  .withReadTimeout(30s)
  .withWriteTimeout(30s)
  .withNoDelay(true)
  .connect(addr)?
defer conn2.close()
```

### 4.3 TCP Server

```protolang
let listener = TcpSocket()
let bound = listener.bind(SocketAddr { ip: IPAddr.V4([0, 0, 0, 0]), port: 8080 })?
let listening = bound.listen(128)?

log.info("Server listening", { port: 8080 })

// Accept connections in a loop
while true {
  let client = listening.accept()?  // TcpSocket<Connected>

  // Handle each client in a background task
  background {
    defer client.close()
    handleClient(client)
  }
}
```

### 4.4 UDP Sockets

```protolang
protocol UdpSocket {
  include Resource

  state Closed
  state Bound { localAddr: SocketAddr }
  state Connected { localAddr: SocketAddr, remoteAddr: SocketAddr }

  transition Closed -> Bound via bind(addr: SocketAddr)
  transition Bound -> Connected via connect(addr: SocketAddr)
  transition any -> Closed via close()

  in Bound {
    effect(udp) function sendTo(data: bytes, addr: SocketAddr): Result<uint64, IOError>
    effect(udp) function recvFrom(buf: &mut bytes): Result<(uint64, SocketAddr), IOError>
  }

  in Connected {
    effect(udp) function send(data: bytes): Result<uint64, IOError>
    effect(udp) function recv(buf: &mut bytes): Result<uint64, IOError>
  }
}

// Usage: UDP client
let udp = UdpSocket()
let bound = udp.bind(SocketAddr { ip: IPAddr.V4([0, 0, 0, 0]), port: 0 })?
defer bound.close()

bound.sendTo("Hello", serverAddr)?
let (len, from) = bound.recvFrom(buf)?

// Usage: UDP server
let server = UdpSocket()
let serverBound = server.bind(SocketAddr { ip: IPAddr.V4([0, 0, 0, 0]), port: 53 })?

while true {
  let (len, clientAddr) = serverBound.recvFrom(buf)?
  let response = processDnsQuery(buf.slice(0, len))
  serverBound.sendTo(response, clientAddr)?
}
```

### 4.5 Unix Domain Sockets

```protolang
protocol UnixSocket {
  include TcpSocket  // Same state machine, different address type

  transition Closed -> Connected via connect(path: Path)
  transition Closed -> Bound via bind(path: Path)
}

// Usage
let socket = UnixSocket()
let conn = socket.connect(Path.from("/var/run/docker.sock"))?
defer conn.close()
```

---

## 5. HTTP / HTTPS

### 5.1 HTTP as I/O

HTTP is treated as a layered I/O protocol built on top of TCP/TLS. It has its own state-dependent types and effect tracking.

```protolang
// Effect hierarchy
// effect(http) implies effect(network) implies effect(io)
```

### 5.2 HTTP Client

```protolang
protocol HttpClient {
  include Resource

  state Idle { baseUrl: Option<Url>, defaultHeaders: Map<string, string> }
  state Closed

  transition Closed -> Idle via new(config: ClientConfig)
  transition Idle -> Closed via close()

  in Idle {
    effect(http) function request(req: Request): Result<Response, HttpError>
    effect(http) function get(url: Url): Result<Response, HttpError>
    effect(http) function post(url: Url, body: Body): Result<Response, HttpError>
    effect(http) function put(url: Url, body: Body): Result<Response, HttpError>
    effect(http) function delete(url: Url): Result<Response, HttpError>
    effect(http) function patch(url: Url, body: Body): Result<Response, HttpError>
    effect(http) function head(url: Url): Result<Response, HttpError>
    effect(http) function options(url: Url): Result<Response, HttpError>
  }
}

record ClientConfig {
  timeout: Duration = 30s,
  connectTimeout: Duration = 10s,
  readTimeout: Duration = 30s,
  writeTimeout: Duration = 30s,
  maxRedirects: uint32 = 10,
  followRedirects: bool = true,
  poolSize: uint32 = 10,
  defaultHeaders: Map<string, string> = Map.empty(),
  tls: Option<TlsConfig> = None,
  proxy: Option<ProxyConfig> = None,
  retry: Option<RetryConfig> = None
}

record RetryConfig {
  maxRetries: uint32 = 3,
  backoff: BackoffStrategy = BackoffStrategy.Exponential(1s, 60s),
  retryOn: Set<StatusCode> = [408, 429, 500, 502, 503, 504],
  retryOnTimeout: bool = true
}
```

### 5.3 Request / Response Types

```protolang
record Request {
  method: HttpMethod,
  url: Url,
  headers: Map<string, string>,
  body: Body,
  timeout: Option<Duration>,
  version: HttpVersion = HttpVersion.HTTP11
}

record Response {
  status: StatusCode,
  headers: Map<string, string>,
  body: Body,
  version: HttpVersion,
  url: Url,
  time: Duration  // How long the request took
}

variant HttpMethod {
  GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS, TRACE, CONNECT
}

variant Body {
  Empty,
  Bytes { data: bytes },
  String { text: string },
  Json { value: Json },
  Stream { data: Stream<byte> },
  Form { fields: Map<string, string> },
  Multipart { parts: List<Part> }
}

record Part {
  name: string,
  fileName: Option<string>,
  contentType: Option<string>,
  body: Body
}

record StatusCode {
  code: uint16
}

function StatusCode.isInformational(): bool  // 1xx
function StatusCode.isSuccess(): bool        // 2xx
function StatusCode.isRedirect(): bool       // 3xx
function StatusCode.isClientError(): bool    // 4xx
function StatusCode.isServerError(): bool    // 5xx
```

### 5.4 HTTP Client Usage

```protolang
// Simple GET
let client = HttpClient.new(ClientConfig { timeout: 30s })?
defer client.close()

let response = client.get("https://api.example.com/users")?
if response.status.isSuccess() {
  let users = response.body.json<List<User>>()?
  print(users)
}

// POST with JSON body
let newUser = User { name: "Alice", email: "alice@example.com" }
let created = client.post(
  "https://api.example.com/users",
  Body.Json(json.encode(newUser))
)?

// With custom headers and per-request timeout
let response = client.request(Request {
  method: HttpMethod.GET,
  url: "https://api.example.com/slow-endpoint",
  headers: Map.from([("Authorization", "Bearer " + token)]),
  body: Body.Empty,
  timeout: Some(120s)
})?

// Streaming download
let response = client.get("https://example.com/large-file.zip")?
let file = File.create("/tmp/download.zip")?
defer file.close()

for chunk in response.body.stream() {
  file.writeAll(chunk)?
}

// Automatic retry and circuit breaker
let resilientClient = HttpClient.new(ClientConfig {
  retry: RetryConfig {
    maxRetries: 5,
    backoff: BackoffStrategy.Exponential(1s, 60s),
    retryOn: [500, 502, 503, 504]
  }
})?
```

### 5.5 HTTP Server

```protolang
protocol HttpServer {
  include Resource

  state Idle { router: Router, config: ServerConfig }
  state Running { boundAddr: SocketAddr }
  state Closed

  transition Idle -> Running via start()
  transition Running -> Closed via stop()
  transition any -> Closed via close()

  in Idle {
    function withRoute(path: string, handler: RouteHandler): HttpServer<Idle>
    function withMiddleware(mw: Middleware): HttpServer<Idle>
    function withPort(port: uint16): HttpServer<Idle>
  }

  in Running {
    effect(http) function accept(): Result<Connection, IOError>
    effect(http) function shutdown(timeout: Duration): Result<void, IOError>
  }
}

record ServerConfig {
  port: uint16 = 8080,
  host: IPAddr = IPAddr.V4([0, 0, 0, 0]),
  readTimeout: Duration = 30s,
  writeTimeout: Duration = 30s,
  idleTimeout: Duration = 120s,
  maxHeaderBytes: uint32 = 1048576,  // 1MB
  maxBodyBytes: uint64 = 10485760,   // 10MB
  tls: Option<TlsConfig> = None
}

type RouteHandler = function(Request, RouteContext): Response
type Middleware = function(Request, RouteContext, NextHandler): Response
type NextHandler = function(Request, RouteContext): Response
```

### 5.6 Server Routing

```protolang
// Define routes with automatic parameter extraction
let router = Router.new()
  // Static routes
  .get("/health", healthHandler)
  .get("/metrics", metricsHandler)

  // Path parameters (auto-parsed and validated)
  .get("/users/:id", getUserHandler)
  .put("/users/:id", updateUserHandler)
  .delete("/users/:id", deleteUserHandler)

  // Query parameter validation
  .get("/users", listUsersHandler)
  // Validates: ?page=int&limit=int&sort=string

  // Sub-routers
  .mount("/api/v1", apiV1Router)
  .mount("/api/v2", apiV2Router)

  // Middleware chain
  .withMiddleware(loggingMiddleware)
  .withMiddleware(tracingMiddleware)
  .withMiddleware(authMiddleware)
  .withMiddleware(rateLimitMiddleware)

// Handler with typed parameters
function getUserHandler(req: Request, ctx: RouteContext): Response {
  let userId = ctx.pathParam<UUID>("id")?        // Auto-parsed UUID
  let includeDeleted = ctx.queryParam<bool>("include_deleted") ?? false

  let user = userService.findUser(userId, includeDeleted)?

  return Response.ok(Body.Json(json.encode(user)))
}

// Handler with body parsing
function createUserHandler(req: Request, ctx: RouteContext): Response {
  let user = req.bodyJson<CreateUserRequest>()?   // Auto-validated against schema

  let created = userService.createUser(user)?

  return Response.created(Body.Json(json.encode(created)))
    .withHeader("Location", "/users/" + created.id)
}

// Start server
let server = HttpServer.new(ServerConfig { port: 8080 })
  .withRouter(router)
  .start()?

log.info("Server started", { addr: server.boundAddr })

// Graceful shutdown
onSignal(SIGTERM) {
  server.shutdown(30s)?  // Wait up to 30s for connections to close
}
```

### 5.7 Response Builder

```protolang
function handler(req: Request): Response {
  return Response.new()
    .withStatus(200)
    .withHeader("Content-Type", "application/json")
    .withHeader("Cache-Control", "max-age=3600")
    .withBody(Body.Json(data))
    .withCookie(Cookie {
      name: "session",
      value: sessionId,
      httpOnly: true,
      secure: true,
      sameSite: SameSite.Strict,
      maxAge: 24h
    })
}

// Convenience constructors
Response.ok(body)           // 200
Response.created(body)      // 201
Response.noContent()        // 204
Response.badRequest(body)   // 400
Response.unauthorized(body) // 401
Response.forbidden(body)    // 403
Response.notFound(body)     // 404
Response.internalError(body) // 500
Response.serviceUnavailable(body) // 503
```

---

## 6. WebSockets

### 6.1 WebSocket as State-Dependent I/O

```protolang
protocol WebSocket {
  include Resource

  state Closed
  state Connecting { url: Url, headers: Map<string, string> }
  state Open { 
    url: Url,
    protocol: Option<string>,
    extensions: List<string>,
    lastPing: DateTime
  }
  state Closing { code: uint16, reason: string }
  state Error { code: uint16, reason: string }

  // Client transitions
  transition Closed -> Connecting via connect(url: Url, headers: Map<string, string>)
  transition Connecting -> Open via handshake()
  transition Connecting -> Error via fail(code: uint16, reason: string)

  // Server transitions
  transition Closed -> Open via accept(request: Request)

  // Common transitions
  transition Open -> Closing via close(code: uint16, reason: string)
  transition Closing -> Closed via confirmClose()
  transition any -> Closed via forceClose()
  transition any -> Error via fail(code: uint16, reason: string)

  in Open {
    effect(ws) function sendText(text: string): Result<void, IOError>
    effect(ws) function sendBinary(data: bytes): Result<void, IOError>
    effect(ws) function sendPing(data: bytes = []): Result<void, IOError>
    effect(ws) function sendPong(data: bytes = []): Result<void, IOError>
    effect(ws) function receive(): Result<Message, IOError>
    effect(ws) function receiveText(): Result<string, IOError>
    effect(ws) function receiveBinary(): Result<bytes, IOError>
    effect(ws) function setReadTimeout(timeout: Duration): Result<void, IOError>
    effect(ws) function setWriteTimeout(timeout: Duration): Result<void, IOError>
    effect(ws) function messages(): Stream<Message>
  }
}

variant Message {
  Text { text: string },
  Binary { data: bytes },
  Ping { data: bytes },
  Pong { data: bytes },
  Close { code: uint16, reason: string }
}
```

### 6.2 WebSocket Client

```protolang
let ws = WebSocket()
let open = ws.connect("wss://echo.example.com/socket", Map.from([
  ("Authorization", "Bearer " + token)
]))?

// Send and receive
open.sendText("Hello, WebSocket!")?
let msg = open.receive()?
match msg {
  Message.Text(t) => print("Received: " + t),
  Message.Binary(b) => print("Received binary: " + b.length + " bytes"),
  Message.Close(c, r) => print("Closed: " + c + " - " + r),
  _ => {}
}

// Streaming messages
for msg in open.messages() {
  match msg {
    Message.Text(t) => handleMessage(t),
    Message.Close(c, r) => {
      log.info("Connection closed", { code: c, reason: r })
      break
    },
    _ => {}
  }
}

// Automatic ping/pong
let heartbeat = background {
  while open.isOpen() {
    sleep(30s)
    if open.isOpen() {
      open.sendPing()?
    }
  }
}

// Graceful close
open.close(1000, "Normal closure")?
```

### 6.3 WebSocket Server

```protolang
// Upgrade HTTP connection to WebSocket
let router = Router.new()
  .get("/ws", websocketHandler)

function websocketHandler(req: Request, ctx: RouteContext): Response {
  // The upgrade is handled automatically by the framework
  let ws = WebSocket.accept(req)?

  // Handle the WebSocket in a background task
  background {
    defer ws.close(1000, "Done")
    handleWebSocket(ws)
  }

  // Return 101 Switching Protocols (handled by framework)
  return Response.upgrade()
}

function handleWebSocket(ws: WebSocket<Open>): void {
  for msg in ws.messages() {
    match msg {
      Message.Text(t) => {
        let response = processMessage(t)
        ws.sendText(response)?
      },
      Message.Binary(b) => {
        let response = processBinary(b)
        ws.sendBinary(response)?
      },
      Message.Ping(data) => ws.sendPong(data)?,
      Message.Close(code, reason) => {
        log.info("Client closed", { code: code, reason: reason })
        break
      }
    }
  }
}
```

---

## 7. Database / SQL as I/O

### 7.1 Philosophy: DB is I/O

In ProtoLang, database operations are **first-class I/O operations**. They use the same resource management, effect tracking, and observability as file and network I/O. The key difference is **type-safe queries**.

```
Database I/O Effect Hierarchy:
├── effect(db)           // Any database operation
│   ├── effect(sql)      // SQL query execution
│   ├── effect(sql.read) // SELECT operations
│   ├── effect(sql.write)// INSERT/UPDATE/DELETE operations
│   ├── effect(sql.schema) // DDL operations
│   └── effect(nosql)    // NoSQL operations
```

### 7.2 Connection as Linear Resource

```protolang
protocol DbConnection {
  include Resource

  state Open { 
    url: DbUrl,
    pool: Option<ConnectionPool>,
    inTransaction: bool,
    transactionLevel: uint32
  }
  state Closed

  transition Closed -> Open via connect(url: DbUrl)
  transition Open -> Closed via close()

  in Open {
    effect(sql) function query<Q, R>(query: Q): Result<QueryResult<R>, DbError>
    effect(sql.write) function execute<Q>(query: Q): Result<ExecutionResult, DbError>
    effect(sql) function prepare<Q>(query: Q): Result<PreparedStatement<Q>, DbError>
    effect(sql) function transaction<T>(block: function(): T): Result<T, DbError>
    effect(sql) function beginTransaction(): Result<void, DbError>
    effect(sql) function commit(): Result<void, DbError>
    effect(sql) function rollback(): Result<void, DbError>
    effect(sql) function rollbackToSavepoint(name: string): Result<void, DbError>
    effect(sql.schema) function migrate(migration: Migration): Result<void, DbError>
    effect(sql) function ping(): Result<Duration, DbError>
  }
}

record DbUrl {
  scheme: string,      // "postgresql", "mysql", "sqlite", etc.
  host: string,
  port: uint16,
  database: string,
  username: Option<string>,
  password: Option<string>,
  params: Map<string, string>
}

record ExecutionResult {
  rowsAffected: uint64,
  lastInsertId: Option<uint64>
}

record QueryResult<T> {
  rows: List<T>,
  columns: List<ColumnInfo>,
  rowCount: uint64,
  executionTime: Duration
}

record ColumnInfo {
  name: string,
  type: SqlType,
  nullable: bool,
  length: Option<uint32>
}
```

### 7.3 Type-Safe Query Syntax

SQL queries are parsed at **compile time** against the database schema. The compiler connects to the database (or reads a schema file) during build.

```protolang
// Schema declaration at module level
schema "postgresql://localhost:5432/shop" as ShopDB

// Type-safe query: compiler verifies table, columns, types
function getActiveUsers(since: DateTime): List<User> {
  let result = query ShopDB {
    SELECT u.id, u.name, u.email, u.created_at, u.status
    FROM users u
    WHERE u.status = 'active'
    AND u.created_at > :since
    ORDER BY u.created_at DESC
    LIMIT 100
  }

  return result.rows
}

// The compiler verifies:
// 1. Table 'users' exists in schema 'ShopDB'
// 2. Columns 'id', 'name', 'email', 'created_at', 'status' exist
// 3. 'status' is comparable with string literal 'active'
// 4. 'created_at' is comparable with DateTime
// 5. Return type List<User> matches selected columns
```

### 7.4 Query Result Mapping

```protolang
// Automatic mapping to records
function getUser(id: UUID): Option<User> {
  let result = query ShopDB {
    SELECT * FROM users WHERE id = :id
  }
  return result.rows.first()
}

// Ad-hoc record types from JOINs
function getOrdersWithUsers(): List<{order: Order, user: User}> {
  return query ShopDB {
    SELECT o.*, u.*
    FROM orders o
    JOIN users u ON o.user_id = u.id
  }.rows
}

// Aggregation with type-safe aliases
function getRevenueByMonth(): List<{month: string, revenue: Money}> {
  return query ShopDB {
    SELECT 
      DATE_TRUNC('month', o.created_at) AS month,
      SUM(o.total) AS revenue
    FROM orders o
    WHERE o.status = 'completed'
    GROUP BY month
    ORDER BY month DESC
  }.rows
}
```

### 7.5 Query Composition

```protolang
// Queries are composable at compile time
let baseQuery = query ShopDB {
  SELECT * FROM products WHERE status = 'active'
}

let filtered = baseQuery
  |> where(p => p.category == "electronics")
  |> orderBy(.price, Asc)
  |> limit(50)
  |> offset(page * 50)

// The compiler generates a single optimized SQL query
```

### 7.6 Transactions

```protolang
function transferFunds(from: AccountId, to: AccountId, amount: Money): Result<void, DbError> {
  return conn.transaction {
    // All operations in this block are part of the same transaction
    let fromAccount = query ShopDB {
      SELECT * FROM accounts WHERE id = :from FOR UPDATE
    }.rows.first() ?? return Err(AccountNotFound(from))

    let toAccount = query ShopDB {
      SELECT * FROM accounts WHERE id = :to FOR UPDATE
    }.rows.first() ?? return Err(AccountNotFound(to))

    if fromAccount.balance < amount {
      return Err(InsufficientFunds)
    }

    execute ShopDB {
      UPDATE accounts SET balance = balance - :amount WHERE id = :from
    }

    execute ShopDB {
      UPDATE accounts SET balance = balance + :amount WHERE id = :to
    }

    execute ShopDB {
      INSERT INTO transactions (from_account, to_account, amount, created_at)
      VALUES (:from, :to, :amount, NOW())
    }

    // Auto-committed if block returns Ok
    // Auto-rolled back if block returns Err or panics
    return Ok(())
  }
}
```

### 7.7 Prepared Statements

```protolang
// Compile-time prepared statement
let stmt = conn.prepare(ShopDB {
  SELECT * FROM users WHERE email = :email
})?

// Reuse with different parameters
let user1 = stmt.execute({ email: "alice@example.com" })?.rows.first()
let user2 = stmt.execute({ email: "bob@example.com" })?.rows.first()
```

### 7.8 Streaming Results

```protolang
// Stream large result sets without loading into memory
function processLargeTable(): void {
  let stream = query ShopDB {
    SELECT * FROM events WHERE created_at > :startDate
  }.stream()

  for event in stream {
    processEvent(event)
  }
}
```

### 7.9 Connection Pooling

```protolang
protocol ConnectionPool {
  include Resource

  state Active { config: PoolConfig }
  state Closed

  transition Closed -> Active via new(config: PoolConfig)
  transition Active -> Closed via close()

  in Active {
    effect(sql) function acquire(): Result<DbConnection, DbError>
    effect(sql) function withConnection<T>(block: function(DbConnection): T): Result<T, DbError>
    function stats(): PoolStats
  }
}

record PoolConfig {
  minConnections: uint32 = 2,
  maxConnections: uint32 = 10,
  maxIdleTime: Duration = 30min,
  maxLifetime: Duration = 1h,
  connectionTimeout: Duration = 30s,
  healthCheckInterval: Duration = 30s
}

record PoolStats {
  total: uint32,
  active: uint32,
  idle: uint32,
  waiting: uint32
}

// Usage
let pool = ConnectionPool.new(PoolConfig {
  maxConnections: 20,
  connectionTimeout: 10s
})?
defer pool.close()

// Automatic connection management
let user = pool.withConnection { conn =>
  query ShopDB { SELECT * FROM users WHERE id = :id }.rows.first()
}?
```

### 7.10 Migrations

```protolang
// Schema versioning is part of the language
schema ShopDB {
  version 1 {
    table users {
      id: UUID primary key default gen_random_uuid(),
      name: string not null,
      email: string not null unique,
      created_at: DateTime not null default now()
    }

    table orders {
      id: UUID primary key default gen_random_uuid(),
      user_id: UUID not null references users(id),
      total: Money not null,
      status: OrderStatus not null default 'pending',
      created_at: DateTime not null default now()
    }

    index idx_orders_user ON orders(user_id)
    index idx_orders_status ON orders(status)
  }

  version 2 {
    alter table users {
      add column phone: Option<string>
      add column last_login: Option<DateTime>
    }

    alter table orders {
      add column shipping_address: Option<Address>
    }
  }

  version 3 {
    create table products {
      id: UUID primary key default gen_random_uuid(),
      name: string not null,
      price: Money not null,
      stock: int32 not null default 0
    }
  }
}

// The compiler:
// 1. Generates migration SQL
// 2. Validates that queries don't reference removed columns
// 3. Warns about deprecated columns
// 4. Can auto-apply migrations in development mode
```

### 7.11 NoSQL Support

```protolang
// MongoDB-style document store
protocol DocumentDb {
  include Resource

  state Connected { url: DbUrl }
  state Closed

  transition Closed -> Connected via connect(url: DbUrl)
  transition Connected -> Closed via close()

  in Connected {
    effect(nosql) function collection<T>(name: string): Collection<T>
    effect(nosql) function transaction<T>(block: function(): T): Result<T, DbError>
  }
}

protocol Collection<T> {
  effect(nosql) function find(filter: DocumentFilter<T>): QueryBuilder<T>
  effect(nosql) function findOne(filter: DocumentFilter<T>): Result<Option<T>, DbError>
  effect(nosql) function insert(document: T): Result<InsertResult, DbError>
  effect(nosql) function update(filter: DocumentFilter<T>, update: Update<T>): Result<UpdateResult, DbError>
  effect(nosql) function delete(filter: DocumentFilter<T>): Result<DeleteResult, DbError>
  effect(nosql) function aggregate(pipeline: AggregationPipeline): Result<List<Json>, DbError>
}

// Usage
let db = DocumentDb.connect("mongodb://localhost:27017/shop")?
defer db.close()

let users = db.collection<User>("users")

let activeUsers = users.find({ status: "active", age: { $gte: 18 } })
  .sort({ createdAt: Desc })
  .limit(100)
  .toList()?
```

---

## 8. Pipes & Process I/O

### 8.1 Standard Streams

```protolang
// stdio is available as linear resources
module std.io.stdio

// Standard input (read-only)
let stdin: ReadOnlyFile  // Already open, cannot close

// Standard output (write-only)
let stdout: WriteOnlyFile  // Already open, cannot close

// Standard error (write-only)
let stderr: WriteOnlyFile  // Already open, cannot close

// Usage
stdout.writeAll("Hello, World!
")?
stderr.writeAll("Error: something went wrong
")?

let line = stdin.readUntil("
")?
```

### 8.2 Process I/O

```protolang
protocol ChildProcess {
  include Resource

  state Running { pid: int32 }
  state Exited { code: int32, stdout: bytes, stderr: bytes }
  state Error { message: string }

  transition Closed -> Running via spawn(command: string, args: List<string>)
  transition Running -> Exited via wait()
  transition Running -> Error via kill()

  in Running {
    effect(io) function stdin(): Writer
    effect(io) function stdout(): Reader
    effect(io) function stderr(): Reader
    effect(io) function kill(signal: Signal): Result<void, IOError>
    effect(io) function wait(): Result<ExitStatus, IOError>
    effect(io) function tryWait(): Result<Option<ExitStatus>, IOError>
  }
}

enum Signal {
  SIGTERM, SIGKILL, SIGINT, SIGHUP, SIGUSR1, SIGUSR2
}

record ExitStatus {
  code: int32,
  success: bool
}

// Usage
let proc = ChildProcess.spawn("git", ["clone", "https://github.com/example/repo.git"])?

// Stream stdout in real-time
for line in proc.stdout().lines() {
  print("[git] " + line)
}

let status = proc.wait()?
if !status.success {
  let err = proc.stderr().readAll()?
  log.error("Git failed", { code: status.code, stderr: err })
}
```

### 8.3 Pipes

```protolang
protocol Pipe {
  include Resource

  state Open { readEnd: Reader, writeEnd: Writer }
  state Closed

  transition Closed -> Open via new()
  transition Open -> Closed via close()

  in Open {
    function reader(): Reader
    function writer(): Writer
  }
}

// Usage
let pipe = Pipe.new()?
let reader = pipe.reader()
let writer = pipe.writer()

background {
  writer.writeAll("Hello from writer")?
  writer.close()?
}

let msg = reader.readAll()?
print(msg)  // "Hello from writer"
```

---

## 9. Streaming & Backpressure

### 9.1 Stream Protocol

```protolang
protocol Stream<T> {
  // Pull-based streaming with backpressure
  effect(io) function next(): Result<Option<T>, IOError>

  // Transformations (lazy)
  function map<U>(f: function(T): U): Stream<U>
  function filter(pred: function(T): bool): Stream<T>
  function take(n: uint64): Stream<T>
  function skip(n: uint64): Stream<T>
  function chunk(size: uint64): Stream<List<T>>
  function window(size: uint64): Stream<List<T>>

  // Terminal operations
  effect(io) function fold<U>(init: U, f: function(U, T): U): Result<U, IOError>
  effect(io) function collect(): Result<List<T>, IOError>
  effect(io) function forEach(f: function(T): void): Result<void, IOError>
  effect(io) function drain(): Result<void, IOError>

  // Combinators
  function merge(other: Stream<T>): Stream<T>
  function zip<U>(other: Stream<U>): Stream<(T, U)>
  function throttle(rate: Rate): Stream<T>
  function debounce(duration: Duration): Stream<T>
  function buffer(size: uint64): Stream<List<T>>
}

record Rate {
  count: uint64,
  per: Duration
}
```

### 9.2 Sink Protocol

```protolang
protocol Sink<T> {
  effect(io) function send(item: T): Result<void, IOError>
  effect(io) function close(): Result<void, IOError>

  // Combinators
  function map<U>(f: function(U): T): Sink<U>
  function filter(pred: function(T): bool): Sink<T>
  function batch(size: uint64, timeout: Duration): Sink<List<T>>
}
```

### 9.3 Stream Usage

```protolang
// Read a large file as a stream
let file = File.open("/tmp/large.log", FileMode.Read)?
let lines = file.stream().split("
")

// Process with backpressure
lines
  .filter(line => line.contains("ERROR"))
  .map(parseLogEntry)
  .filter(entry => entry.timestamp > startDate)
  .chunk(100)           // Batch into groups of 100
  .forEach(batch => {
    db.insertBatch(batch)?
  })?

// HTTP response streaming
let response = http.get("https://example.com/stream")?
for chunk in response.body.stream() {
  processChunk(chunk)
}

// Throttled processing
let events = eventSource.stream()
  .throttle(Rate { count: 100, per: 1s })
  .debounce(100ms)

for event in events {
  handleEvent(event)
}
```

### 9.4 Pipe Between Streams

```protolang
// Connect a stream to a sink
let source = File.open("/tmp/input.txt", FileMode.Read)?.stream()
let sink = File.create("/tmp/output.txt")?.sink()

source.pipeTo(sink)?  // Handles backpressure automatically

// Transform and pipe
File.open("/tmp/input.txt", FileMode.Read)?.stream()
  .map(line => line.toUpperCase())
  .filter(line => line.length > 0)
  .pipeTo(File.create("/tmp/output.txt")?.sink())?
```

---

## 10. Serialization I/O

### 10.1 JSON I/O

```protolang
module std.io.json

// Encode to JSON (pure, no I/O)
pure function encode<T>(value: T): string

// Decode from JSON (pure, no I/O)
pure function decode<T>(json: string): Result<T, JsonError>

// Stream encoding/decoding
function encodeStream<T>(stream: Stream<T>): Stream<string>
function decodeStream<T>(stream: Stream<string>): Stream<Result<T, JsonError>>

// File I/O convenience
function readFile<T>(path: Path): Result<T, IOError> {
  let file = File.open(path, FileMode.Read)?
  defer file.close()
  let content = file.readAll()?
  return decode<T>(content)
}

function writeFile<T>(path: Path, value: T): Result<void, IOError> {
  let file = File.create(path)?
  defer file.close()
  return file.writeAll(encode(value))
}
```

### 10.2 Protocol Buffers

```protolang
module std.io.protobuf

// Generated from .proto files at compile time
// Types are checked against the proto schema

function encode<T>(message: T): bytes
function decode<T>(data: bytes): Result<T, ProtoError>
function encodeStream<T>(stream: Stream<T>): Stream<bytes>
function decodeStream<T>(stream: Stream<bytes>): Stream<Result<T, ProtoError>>
```

### 10.3 CSV I/O

```protolang
module std.io.csv

protocol CsvReader {
  include Resource

  effect(io) function readRow(): Result<Option<Row>, CsvError>
  effect(io) function readRows(): Result<List<Row>, CsvError>
  effect(io) function stream(): Stream<Row>

  function withDelimiter(delim: char): CsvReader
  function withHeader(hasHeader: bool): CsvReader
}

protocol CsvWriter {
  include Resource

  effect(io) function writeRow(row: Row): Result<void, CsvError>
  effect(io) function writeRows(rows: List<Row>): Result<void, CsvError>

  function withDelimiter(delim: char): CsvWriter
}

// Type-safe CSV mapping
record User {
  id: UUID,
  name: string,
  email: string,
  age: Option<int32>
}

// The compiler verifies column names and types against the record
let csv = CsvReader.open("/tmp/users.csv")?
  .withHeader(true)
  .withDelimiter(',')

for user in csv.stream().map(decode<User>) {
  db.insert(user)?
}
```

---

## 11. Resource Pools & Connection Management

### 11.1 Generic Pool Protocol

```protolang
protocol Pool<T> where T: Resource {
  state Active { config: PoolConfig }
  state Closed

  transition Closed -> Active via new(config: PoolConfig, factory: function(): T)
  transition Active -> Closed via close()

  in Active {
    effect(io) function acquire(): Result<T, PoolError>
    effect(io) function tryAcquire(): Result<Option<T>, PoolError>
    effect(io) function release(item: T): Result<void, PoolError>
    effect(io) function withItem<R>(block: function(T): R): Result<R, PoolError>
    function stats(): PoolStats
  }
}

record PoolConfig {
  minSize: uint32 = 2,
  maxSize: uint32 = 10,
  maxIdleTime: Duration = 30min,
  maxLifetime: Duration = 1h,
  acquireTimeout: Duration = 30s,
  healthCheckInterval: Duration = 30s,
  validationQuery: Option<string> = None
}
```

### 11.2 Pool Usage

```protolang
// Generic pool for any resource
let tcpPool = Pool.new(PoolConfig { maxSize: 50 }, () => {
  let socket = TcpSocket()
  socket.connect(backendAddr)?
  return socket
})?

defer tcpPool.close()

// Automatic acquire/release
let response = tcpPool.withItem { conn =>
  conn.writeAll(request)?
  return conn.readAll()
}?
```

---

## 12. TLS / SSL

### 12.1 TLS as I/O Layer

```protolang
protocol TlsStream {
  include Socket

  state Handshaking { host: string }
  state Connected { 
    peerCertificate: Certificate,
    cipherSuite: string,
    protocolVersion: string,
    isResumed: bool
  }
  state Closed

  transition Closed -> Handshaking via handshake(underlying: Socket, config: TlsConfig)
  transition Handshaking -> Connected via completeHandshake()
  transition Handshaking -> Error via fail(reason: TlsError)
  transition any -> Closed via close()

  in Connected {
    effect(tls) function renegotiate(): Result<void, TlsError>
    effect(tls) function peerCertificate(): Certificate
    effect(tls) function sessionInfo(): SessionInfo
  }
}

record TlsConfig {
  certFile: Option<Path>,
  keyFile: Option<Path>,
  caFile: Option<Path>,
  verifyMode: VerifyMode = VerifyMode.Peer,
  minVersion: TlsVersion = TlsVersion.TLS13,
  maxVersion: TlsVersion = TlsVersion.TLS13,
  cipherSuites: List<string> = [],
  alpnProtocols: List<string> = ["h2", "http/1.1"]
}

enum VerifyMode {
  None,      // Don't verify certificates
  Peer,      // Verify peer certificate
  FailIfNoPeerCert  // Verify and fail if no cert presented
}

enum TlsVersion {
  TLS10, TLS11, TLS12, TLS13
}

record Certificate {
  subject: string,
  issuer: string,
  notBefore: DateTime,
  notAfter: DateTime,
  serialNumber: string,
  fingerprint: string
}
```

### 12.2 TLS Usage

```protolang
// Client TLS
let socket = TcpSocket()
let tcp = socket.connect(SocketAddr { ip: IPAddr.V4([1, 1, 1, 1]), port: 443 })?

let tls = TlsStream.handshake(tcp, TlsConfig {
  verifyMode: VerifyMode.Peer,
  minVersion: TlsVersion.TLS12
})?
let secure = tls.completeHandshake()?
defer secure.close()

secure.writeAll("GET / HTTP/1.1
Host: cloudflare.com

")?
let response = secure.readAll()?

// Server TLS
let server = TcpSocket()
let listening = server.bind(addr)?.listen(128)?

while true {
  let client = listening.accept()?
  background {
    let tls = TlsStream.handshake(client, TlsConfig {
      certFile: Some("/etc/ssl/cert.pem"),
      keyFile: Some("/etc/ssl/key.pem"),
      verifyMode: VerifyMode.None
    })?
    let secure = tls.completeHandshake()?
    defer secure.close()
    handleHttps(secure)
  }
}
```

---

## 13. I/O Observability

### 13.1 Automatic Instrumentation

Every I/O operation in `std.io` automatically generates:

1. **Spans** — hierarchical tracing of I/O operations
2. **Metrics** — counters, histograms, and gauges
3. **Logs** — structured logging with context

```protolang
// No manual instrumentation needed
let file = File.open("/tmp/data.txt", FileMode.Read)?
let content = file.readAll()?

// Auto-generated telemetry:
// Span: "file.open" { path: "/tmp/data.txt", mode: "Read" }
// Span: "file.readAll" { bytes: 1024, duration: 5ms }
// Metric: io.file.read_bytes += 1024
// Metric: io.file.read_latency.record(5ms)
```

### 13.2 I/O Metrics

```protolang
// Auto-registered metrics
io.file.read_bytes       // Counter: total bytes read from files
io.file.write_bytes      // Counter: total bytes written to files
io.file.open_count       // Counter: total file open operations
io.file.error_count      // Counter: total file errors
io.file.read_latency     // Histogram: file read latency
io.file.write_latency    // Histogram: file write latency

io.net.bytes_sent        // Counter: total network bytes sent
io.net.bytes_received    // Counter: total network bytes received
io.net.connect_count     // Counter: total connection attempts
io.net.error_count       // Counter: total network errors
io.net.connect_latency   // Histogram: connection establishment latency
io.net.request_latency   // Histogram: HTTP request latency

io.db.query_count        // Counter: total queries executed
io.db.query_latency      // Histogram: query execution latency
io.db.connection_count   // Gauge: active database connections
io.db.transaction_count  // Counter: total transactions
io.db.error_count        // Counter: total DB errors
```

### 13.3 Custom I/O Spans

```protolang
function batchProcess(files: List<Path>): void {
  span "batch_process" {
    attribute file.count = files.length

    for file in files {
      span "process_file" {
        attribute file.path = file

        let data = File.open(file, FileMode.Read)?.readAll()?
        let result = process(data)
        File.create(file + ".out")?.writeAll(result)?

        // Auto-nested spans:
        // - "file.open"
        // - "file.readAll"
        // - "file.create"
        // - "file.writeAll"
      }
    }
  }
}
```

### 13.4 I/O Context Propagation

Trace context propagates automatically across:
- File operations (stored in extended attributes where supported)
- Network calls (HTTP headers, custom TCP framing)
- Database queries (appended as SQL comments for correlation)
- WebSocket messages (metadata frames)

```protolang
// The current trace ID is automatically included in:
// - HTTP requests as "X-Trace-Id" header
// - SQL queries as "/* trace_id=abc123 */" comment
// - WebSocket metadata frames

let response = http.get("https://api.example.com/data")?
// Request headers automatically include:
// X-Trace-Id: abc123
// X-Span-Id: def456
// X-Request-Id: ghi789

let result = query ShopDB { SELECT * FROM users }?
// Executed SQL:
// /* trace_id=abc123 span_id=jkl012 */
// SELECT * FROM users
```

---

## 14. Error Model

### 14.1 I/O Error Hierarchy

```protolang
variant IOError {
  // Generic errors
  NotFound { path: Option<Path> },
  PermissionDenied { path: Option<Path> },
  AlreadyExists { path: Path },
  InvalidInput { message: string },
  InvalidData { message: string },
  UnexpectedEof,
  WriteZero,
  Interrupted,
  Other { message: string },

  // Network-specific
  ConnectionRefused { addr: SocketAddr },
  ConnectionReset { addr: SocketAddr },
  ConnectionAborted { addr: SocketAddr },
  NotConnected,
  AddrInUse { addr: SocketAddr },
  AddrNotAvailable { addr: SocketAddr },
  BrokenPipe,
  TimedOut { operation: string, duration: Duration },

  // TLS-specific
  TlsError { reason: TlsFailure },
  CertificateInvalid { reason: CertError },

  // DB-specific
  DbError { reason: DbFailure },

  // Pool-specific
  PoolExhausted { maxSize: uint32 },
  PoolTimeout { duration: Duration }
}

variant TlsFailure {
  HandshakeFailed,
  CertificateVerifyFailed,
  ProtocolError,
  AlertReceived { code: uint8, message: string }
}

variant CertError {
  Expired { notAfter: DateTime },
  NotYetValid { notBefore: DateTime },
  InvalidName { expected: string, actual: string },
  SelfSigned,
  UntrustedIssuer,
  Revoked
}

variant DbFailure {
  ConnectionLost,
  QueryError { sql: string, message: string },
  ConstraintViolation { table: string, constraint: string },
  UniqueViolation { table: string, column: string },
  ForeignKeyViolation { table: string, constraint: string },
  SerializationFailure,
  DeadlockDetected,
  MigrationError { version: uint32, message: string }
}
```

### 14.2 Error Context

All I/O errors carry structured context:

```protolang
try {
  let file = File.open("/root/secret.txt", FileMode.Read)?
} catch PermissionDenied(e) {
  log.error("Cannot access file", {
    path: e.path,
    user: context.userId,
    requiredPermission: "read",
    traceId: context.traceId
  })
} catch FileNotFound(e) {
  log.error("File missing", { path: e.path })
}
// Compile error if any error type is not handled
```

### 14.3 Retryable Errors

Some errors are automatically classified as retryable:

```protolang
function IOError.isRetryable(): bool {
  match this {
    Interrupted => true,
    TimedOut => true,
    ConnectionRefused => true,
    ConnectionReset => true,
    DbError(DeadlockDetected) => true,
    DbError(SerializationFailure) => true,
    _ => false
  }
}

// Used by automatic retry logic
let client = HttpClient.new(ClientConfig {
  retry: RetryConfig {
    retryOn: [500, 502, 503, 504],
    retryOnTimeout: true  // Uses isRetryable() under the hood
  }
})?
```

---

## 15. Complete Examples

### 15.1 HTTP Proxy Server

```protolang
import std.io.net.http.{Server, Request, Response}
import std.io.net.{TcpSocket, SocketAddr, IPAddr}

function main(): void {
  let router = Router.new()
    .all("/*", proxyHandler)

  let server = HttpServer.new(ServerConfig { port: 8080 })
    .withRouter(router)
    .start()?

  log.info("Proxy server started", { port: 8080 })

  onSignal(SIGTERM) {
    server.shutdown(30s)?
  }
}

function proxyHandler(req: Request, ctx: RouteContext): Response {
  let targetUrl = "https://backend.example.com" + req.path

  span "proxy_request" {
    attribute target = targetUrl
    attribute method = req.method

    // Forward the request
    let backend = HttpClient.new(ClientConfig {
      timeout: 60s,
      followRedirects: false
    })?
    defer backend.close()

    let response = backend.request(Request {
      method: req.method,
      url: targetUrl,
      headers: req.headers.without(["Host", "Connection"]),
      body: req.body,
      timeout: Some(60s)
    })?

    // Stream the response back
    return Response.new()
      .withStatus(response.status.code)
      .withHeaders(response.headers)
      .withBody(response.body)
  }
}
```

### 15.2 Real-Time Chat Server (WebSocket + DB)

```protolang
import std.io.net.ws.{WebSocket, Message}
import std.io.db

schema "postgresql://localhost:5432/chat" as ChatDB

guardian ChatServer {
  stable rooms: Map<string, Room>
  stable messages: Map<string, List<ChatMessage>>
  volatile connections: Map<string, List<WebSocket<Open>>>

  recover {
    this.connections = Map.empty()
    for room in this.rooms.values() {
      this.connections.put(room.id, [])
    }
  }

  public function joinRoom(roomId: string, user: User, ws: WebSocket<Open>): void {
    // Validate room exists
    let room = this.rooms.get(roomId) ?? {
      ws.sendText(json.encode({ error: "Room not found" }))?
      ws.close(1008, "Room not found")?
      return
    }

    // Add connection
    this.connections.getOrDefault(roomId, []).push(ws)

    // Send recent messages
    let recent = query ChatDB {
      SELECT * FROM messages 
      WHERE room_id = :roomId 
      ORDER BY created_at DESC 
      LIMIT 50
    }.rows

    ws.sendText(json.encode({ type: "history", messages: recent }))?

    // Broadcast join
    broadcast(roomId, { type: "join", user: user })

    // Handle messages
    for msg in ws.messages() {
      match msg {
        Message.Text(t) => handleMessage(roomId, user, t, ws),
        Message.Close(code, reason) => {
          leaveRoom(roomId, user, ws)
          break
        },
        _ => {}
      }
    }
  }

  private function handleMessage(
    roomId: string, 
    user: User, 
    text: string, 
    ws: WebSocket<Open>
  ): void {
    let message = ChatMessage {
      id: UUID.new(),
      roomId: roomId,
      userId: user.id,
      text: text,
      createdAt: DateTime.now()
    }

    // Persist to DB
    topaction {
      execute ChatDB {
        INSERT INTO messages (id, room_id, user_id, text, created_at)
        VALUES (:message.id, :message.roomId, :message.userId, :message.text, :message.createdAt)
      }
      this.messages.getOrDefault(roomId, []).push(message)
    }

    // Broadcast to room
    broadcast(roomId, { type: "message", message: message })
  }

  private function broadcast(roomId: string, event: Json): void {
    let msg = json.encode(event)
    for conn in this.connections.getOrDefault(roomId, []) {
      if conn.isOpen() {
        conn.sendText(msg)?
      }
    }
  }

  private function leaveRoom(roomId: string, user: User, ws: WebSocket<Open>): void {
    let conns = this.connections.getOrDefault(roomId, [])
    conns.remove(ws)
    broadcast(roomId, { type: "leave", user: user })
    ws.close(1000, "Goodbye")?
  }
}

// HTTP upgrade handler
function chatHandler(req: Request, ctx: RouteContext): Response {
  let roomId = ctx.queryParam<string>("room")?
  let token = ctx.header<string>("Authorization")?
  let user = authService.validate(token)?

  let ws = WebSocket.accept(req)?
  background {
    chatServer.joinRoom(roomId, user, ws)
  }

  return Response.upgrade()
}
```

### 15.3 ETL Pipeline (File → Stream → DB)

```protolang
import std.io.{File, Stream}
import std.io.csv
import std.io.db

schema "postgresql://localhost:5432/warehouse" as WarehouseDB

function runETL(inputPath: Path, batchSize: uint64): Result<void, IOError> {
  let file = File.open(inputPath, FileMode.Read)?
  defer file.close()

  let pool = ConnectionPool.new(PoolConfig { maxConnections: 5 })?
  defer pool.close()

  // Parse CSV as a stream
  let csv = CsvReader.fromReader(file)
    .withHeader(true)
    .withDelimiter(',')

  // Transform, validate, and batch insert
  csv.stream()
    .map(parseProduct)           // Parse each row into a Product record
    .filter(validateProduct)     // Filter out invalid records
    .chunk(batchSize)            // Batch into groups
    .throttle(Rate { count: 1000, per: 1s })  // Rate limit
    .forEach { batch =>
      pool.withConnection { conn =>
        conn.transaction {
          for product in batch {
            execute WarehouseDB {
              INSERT INTO products (id, name, price, category, updated_at)
              VALUES (:product.id, :product.name, :product.price, :product.category, NOW())
              ON CONFLICT (id) DO UPDATE SET
                name = EXCLUDED.name,
                price = EXCLUDED.price,
                category = EXCLUDED.category,
                updated_at = EXCLUDED.updated_at
            }
          }
        }?
      }?
    }?

  log.info("ETL complete", { 
    input: inputPath,
    batches: metrics.counter("etl.batches").value,
    records: metrics.counter("etl.records").value
  })

  return Ok(())
}

function parseProduct(row: csv.Row): Result<Product, CsvError> {
  return Product {
    id: row.get<UUID>("id")?,
    name: row.get<string>("name")?,
    price: row.get<Money>("price")?,
    category: row.get<string>("category")?
  }
}

function validateProduct(product: Product): bool {
  return product.price > Money.zero() 
    && product.name.length > 0
    && product.category in ["electronics", "clothing", "food"]
}
```

### 15.4 Distributed File Sync (TCP + File I/O)

```protolang
import std.io.net.{TcpSocket, SocketAddr, IPAddr}
import std.io.{File, Path}

protocol FileSyncProtocol {
  command SyncRequest { path: Path, checksum: string }
  command SyncResponse { path: Path, data: bytes, checksum: string }
  command SyncAck { path: Path, success: bool }
}

function syncClient(serverAddr: SocketAddr, localDir: Path): Result<void, IOError> {
  let socket = TcpSocket()
  let conn = socket.connect(serverAddr)?
  defer conn.close()

  // Walk local directory
  let dir = Directory.open(localDir)?
  defer dir.close()

  for entry in dir.entries() {
    if !entry.metadata.isFile { continue }

    let checksum = calculateChecksum(entry.path)?
    let request = FileSyncProtocol.SyncRequest {
      path: entry.path,
      checksum: checksum
    }

    conn.writeAll(encode(request))?

    let response = decode<FileSyncProtocol.SyncResponse>(conn.readAll())?

    if response.checksum != checksum {
      let file = File.create(entry.path)?
      defer file.close()
      file.writeAll(response.data)?

      conn.writeAll(encode(FileSyncProtocol.SyncAck {
        path: entry.path,
        success: true
      }))?
    }
  }

  return Ok(())
}

function syncServer(listenAddr: SocketAddr, sourceDir: Path): Result<void, IOError> {
  let socket = TcpSocket()
  let bound = socket.bind(listenAddr)?
  let listening = bound.listen(10)?

  log.info("Sync server listening", { addr: listenAddr })

  while true {
    let client = listening.accept()?
    background {
      defer client.close()
      handleSyncClient(client, sourceDir)
    }
  }
}

function handleSyncClient(conn: TcpSocket<Connected>, sourceDir: Path): void {
  while true {
    let request = decode<FileSyncProtocol.SyncRequest>(conn.readAll())?
    let sourcePath = sourceDir.join(request.path)

    let sourceChecksum = calculateChecksum(sourcePath)?

    if sourceChecksum == request.checksum {
      // File is up to date, send empty response
      conn.writeAll(encode(FileSyncProtocol.SyncResponse {
        path: request.path,
        data: [],
        checksum: sourceChecksum
      }))?
    } else {
      let file = File.open(sourcePath, FileMode.Read)?
      defer file.close()
      let data = file.readAll()?

      conn.writeAll(encode(FileSyncProtocol.SyncResponse {
        path: request.path,
        data: data,
        checksum: sourceChecksum
      }))?
    }

    let ack = decode<FileSyncProtocol.SyncAck>(conn.readAll())?
    if !ack.success {
      log.warn("Sync failed for file", { path: ack.path })
    }
  }
}
```

---

## Appendix A: I/O Effect Lattice

```
                              ┌─────────────┐
                              │   effect    │
                              │   (io)      │
                              └──────┬──────┘
                                     │
           ┌─────────────────────────┼─────────────────────────┐
           │                         │                         │
    ┌──────┴──────┐          ┌──────┴──────┐          ┌──────┴──────┐
    │ effect(file)│          │effect(network)│         │ effect(db)  │
    └──────┬──────┘          └──────┬──────┘          └──────┬──────┘
           │                         │                         │
    ┌──────┴──────┐          ┌──────┴──────┐          ┌──────┴──────┐
    │ read/write  │    ┌─────┼─────┬───────┼─────┐    │  effect(sql) │
    │  seek/sync  │    │     │     │       │     │    │ effect(nosql)│
    └─────────────┘    │     │     │       │     │    └─────────────┘
                  ┌────┴┐ ┌──┴──┐ ┌┴────┐ ┌┴───┐
                  │effect│ │effect│ │effect│ │effect│
                  │(tcp) │ │(udp) │ │(http)│ │(ws)  │
                  └──────┘ └─────┘ └──────┘ └─────┘
                              │
                         ┌────┴────┐
                         │ effect  │
                         │  (tls)  │
                         └─────────┘
```

**Subtyping Rules:**
- `effect(file)` <: `effect(io)`
- `effect(network)` <: `effect(io)`
- `effect(db)` <: `effect(io)`
- `effect(tcp)` <: `effect(network)` <: `effect(io)`
- `effect(http)` <: `effect(network)` <: `effect(io)`
- `effect(sql)` <: `effect(db)` <: `effect(io)`

---

## Appendix B: I/O Resource Lifecycle

```
┌──────────┐     acquire()      ┌──────────┐     use      ┌──────────┐
│  Closed   │ ─────────────────> │   Open   │ ──────────> │  Error   │
│  (pool)   │                    │ (active) │             │(terminal)│
└──────────┘                    └──────────┘             └──────────┘
       ^                              │
       │                              │ close()
       │                              │
       └──────────────────────────────┘
```

All I/O resources follow this lifecycle:
1. **Acquire** from factory or pool
2. **Use** for I/O operations
3. **Close** to release (linear consumption)
4. **Error** state on failure (auto-transitions to Closed on cleanup)

---

## Appendix C: Performance Considerations

| Operation | Latency Target | Throughput Target | Buffer Strategy |
|-----------|---------------|-------------------|-----------------|
| File read (cached) | < 1μs | 10 GB/s | Direct I/O + page cache |
| File read (uncached) | < 10ms | 500 MB/s | Async I/O + readahead |
| TCP round-trip (local) | < 100μs | 10 Gbps | Zero-copy sendfile |
| TCP round-trip (WAN) | < 50ms | 1 Gbps | BBR congestion control |
| HTTP request | < 1ms (local) | 100K req/s | Connection pooling |
| DB query (simple) | < 1ms | 50K qps | Prepared statement cache |
| DB query (complex) | < 100ms | 1K qps | Query result streaming |
| WebSocket message | < 100μs | 1M msg/s | Frame batching |

---

*End of std.io Package Specification*
