# The killer demo: a money-transfer ledger, raced

**Claim under test**: a naive two-account transfer service, written the
way an AI agent or a tired human actually writes it on a first pass (no
lock, no explicit transaction, no optimistic version check), stays
correct under real concurrent load on `nirdosha serve` and silently
corrupts data under the same load on the equivalent naive Python
service. This is a real, measured result from the two services in this
directory, not a simulated one — see Methodology below for exactly how
to reproduce every number here.

## The two services

- [`ledger.nir`](./ledger.nir) — `POST /api/transfer {from_id, to_id,
  amount_cents}`: reads both accounts' balances into local variables,
  `sleep_ms(2)` to model a real database round trip, then writes both
  new balances back. Served with `nirdosha serve`.
- [`ledger_naive.py`](./ledger_naive.py) — the same route, the same
  schema, the same read-both/sleep/write-both shape, in ~140 lines of
  Python standard library (`http.server` + `sqlite3`, no `pip install`).
  A fresh SQLite connection per request (the idiomatic pattern real
  frameworks use — Flask's `g.db`, a scoped SQLAlchemy session), not one
  shared connection across threads — see the methodology note below on
  why that specific, different bug was deliberately fixed rather than
  left in.

Both start from the same invariant: two accounts, 1,000,000 cents each,
total 2,000,000. A transfer moves money between them; nothing should
ever change the total. Neither implementation takes a lock, wraps the
read+write in a transaction, or does anything else to protect that
invariant under concurrency — this is deliberately the naive shape both
languages make easy to write by accident, not a hardened one.

## The result

| | `nirdosha serve` | Python, single-threaded (`HTTPServer`) | Python, threaded (`ThreadingHTTPServer`) |
|---|---:|---:|---:|
| Requests | 500 | 500 | 500 |
| Concurrency (client-side) | 50 | 50 | 50 |
| Runs | 2 | 2 | 3 |
| Ledger drift, every run | **0** | **0** | **167 / 377 / -29** |
| Verdict | PASS, every run | PASS, every run | **FAIL, every run** |
| Throughput | 273–284 req/s | 17–25 req/s | 25–27 req/s |

**Every threaded-Python run corrupted the ledger. Every Nirdosha run and
every single-threaded-Python run conserved it exactly.** Concurrency,
not the language, is the variable that flips the result — see "What
this does and doesn't prove" below.

Raw output from the actual runs (2026-09-05, Ryzen/x86_64 Linux, `nirdosha`
release build, Python 3.x stdlib):

```
### Nirdosha, run A (seed 1) ###
  requests:        500 in 1.831s  (273.0 req/s)
  outcomes:        {'ok': 500, 'InsufficientFunds': 0, 'other_err': 0, 'exceptions': 0}
  total before:    2000000
  total after:     2000000
  drift:           0  (conserved, exactly as expected)
  verdict:         PASS

### Nirdosha, run B (seed 2) ###
  requests:        500 in 1.759s  (284.3 req/s)
  total after:     2000000
  drift:           0
  verdict:         PASS

### Python, single-threaded control, run A (seed 1) ###
  requests:        500 in 19.742s  (25.3 req/s)
  outcomes:        {'ok': 469, ..., 'exceptions': 31}
  total after:     2000000
  drift:           0
  verdict:         PASS

### Python, single-threaded control, run B (seed 2) ###
  requests:        500 in 30.107s  (16.6 req/s)
  total after:     2000000
  drift:           0
  verdict:         PASS

### Python, THREADED (the realistic default), run A (seed 1) ###
  requests:        500 in 19.953s  (25.1 req/s)
  outcomes:        {'ok': 466, ..., 'exceptions': 34}
  total before:    2000000
  total after:     2000167
  drift:           167  (CORRUPTED -- money was created or destroyed)
  verdict:         FAIL

### Python, THREADED, run B (seed 2) ###
  total after:     2000377
  drift:           377  (CORRUPTED)
  verdict:         FAIL

### Python, THREADED, run C (seed 3) ###
  total after:     1999971
  drift:           -29  (CORRUPTED)
  verdict:         FAIL
```

Money is both created (+167, +377) and destroyed (-29) across the three
threaded runs — a lost-update race can go either direction, depending
on which of two racing requests' write lands last.

## Why this happens

Both services do the same three things per transfer:

1. `SELECT` both balances into local variables.
2. Wait ~2ms (standing in for a real network round trip to the
   database).
3. Compute each new balance from the value read in step 1, then `UPDATE`
   both rows with the computed absolute value.

Step 3 is the trap: it writes back a value computed from a balance that
may be stale by the time the write happens. Two transfers racing on the
same account can both read the same starting balance, both compute
their own "correct" new balance from it, and the second write simply
overwrites the first — one transfer's effect vanishes (or, if both
credits land, gets double-counted). This is the textbook "lost update"
/ TOCTOU bug, the same shape as `examples/online-trading.nir`'s
`place_order_inner` and a large fraction of real-world double-spend and
coupon-redemption bugs in production systems.

**`nirdosha serve` never hits this window because its request loop is
strictly sequential — one process, no `thread::spawn` anywhere in
`serve.rs` (see that module's own doc comment). A whole request runs to
completion before the next one is even looked at**, so the "stale read"
step 1 describes is not reachable: there is no other request running
between an `nirdosha serve` request's `SELECT`s and its `UPDATE`s.
Python's `ThreadingHTTPServer`, and any production stack built the same
way (a WSGI server with multiple worker threads, a Node.js app with
async handlers touching shared state, a Flask dev server with
`threaded=True`), makes exactly that window available to a second
request — nothing in `ledger_naive.py` closes it, because nothing a
typical naive implementation writes does either.

## What this does and doesn't prove

**Honest framing, matching this repo's own standard (see the main
[README](../../README.md) and the
[wiki's Honest Scope page](https://github.com/arunsoman/nirdosha/wiki/Honest-Scope-and-Roadmap)):**

- This is **not** a claim that Nirdosha's ownership/`spawn`/`chan` type
  system (`docs/LANGUAGE.md`, proven for in-language concurrency, see
  `examples/comparison/01-concurrent-counter.md`) is what prevents this
  race. `db`/`http`/`transact` are interpreter-only today (Track B,
  `docs/PUBLIC_ROADMAP.md`) and `transfer_inner` never touches `spawn`
  or `chan` at all. What prevents it here is a real, verifiable,
  today's-code property of `serve.rs`: its request-handling loop
  processes one request at a time, so two `transfer` calls can never be
  mid-flight together. That's a server-architecture fact, not a
  language-level data-race proof — worth being exactly this precise
  about, per this repo's own conventions.
- That property is also a real, disclosed *cost*: `nirdosha serve`
  cannot use more than one CPU core for request handling today. The
  throughput numbers above show it anyway beating both Python
  configurations at this concurrency level (a Rust process making one
  SQLite call at a time comfortably outruns a CPython process doing the
  same, even before Python's own threading/locking overhead is added
  in) — but the honest limit is architectural, and a future
  multi-threaded `serve.rs` would need a real concurrency story (a
  per-connection lock, `transact`, or equivalent) to keep this
  guarantee, not get to assume it forever.
- The Python side's `sqlite3.connect(...)` calls are made honestly
  naive on purpose — a fresh connection per request (the pattern real
  frameworks actually use) with `PRAGMA busy_timeout` set, specifically
  so the demonstrated bug is the missing transaction isolation around
  the read-then-write sequence, not "don't share one raw SQLite handle
  across threads without a lock" (a real, different, more famous SQLite
  footgun — an earlier draft of this demo used one shared connection
  and hit that bug instead, via a `sqlite3` `SQLITE_MISUSE` error and
  `ConnectionResetError`s; fixed here so the result isolates the
  intended failure mode). The residual `exceptions` count in every run
  above (`27`–`34`, present even in the single-threaded, non-corrupting
  control) is a real, disclosed side effect of that fix under this much
  concurrency — occasional `database is locked` errors and
  connection resets, a well-documented characteristic of SQLite's
  single-writer model under load, not specific to the race this demo
  targets, and not hidden from the numbers above.
- The `-p 50` concurrency and 2ms simulated latency are the actual
  parameters used for every number in this file — not tuned per-run to
  produce a nicer result. See Methodology below to reproduce or vary
  them.

## Methodology

Every number above came from an actual run of the actual code in this
directory, verified by hand before being trusted here (same discipline
`benchmarks/RESULTS.md` uses): each service was started fresh, its
ledger reset to the known starting total via `POST /api/reset_ledger`,
exactly 500 `POST /api/transfer` calls were fired at it through a
`ThreadPoolExecutor(max_workers=50)`, and `POST /api/total` was read
back afterward and diffed against the starting total. No number here
was estimated, extrapolated, or hand-adjusted.

```sh
# Terminal 1 -- Nirdosha
nirdosha serve examples/killer_demo/ledger.nir --port 8099 \
  --db examples/killer_demo/ledger.db

# Terminal 2 -- the Python twin (the realistic, concurrent config)
python3 examples/killer_demo/ledger_naive.py --port 8100 \
  --db examples/killer_demo/ledger_naive.db --threaded

# Terminal 3 -- fire the same load at each and compare
python3 examples/killer_demo/load_test.py --base-url http://127.0.0.1:8099 --n 500 --concurrency 50
python3 examples/killer_demo/load_test.py --base-url http://127.0.0.1:8100 --n 500 --concurrency 50

# Optional: the single-threaded Python control (omit --threaded above)
# confirms the corruption is a concurrency property, not a Python- or
# SQLite-specific one -- it passes every time, at roughly the same
# (slow) per-request latency as the threaded run, just without the
# race window.
```

`load_test.py --seed N` reproduces one specific request ordering; the
runs above used seeds 1/2 (Nirdosha, Python-sequential) and 1/2/3
(Python-threaded).
