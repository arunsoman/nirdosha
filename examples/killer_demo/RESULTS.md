# The killer demo: a money-transfer ledger, raced

**Claim under test**: a naive two-account transfer service, written the
way an AI agent or a tired human actually writes it on a first pass (no
lock, no explicit transaction, no optimistic version check), stays
correct under real concurrent HTTP load on `nirdosha serve` and silently
corrupts data under the same load on an equally naive Python service.
**A second, harder question this file also answers, because the first
draft didn't and got called on it**: is that safety coming from the
*language*, or just from `nirdosha serve`'s current server architecture?
See "Does the language itself prevent this?" below — the honest answer
is the second one, and it's demonstrated, not asserted.

Every number in this file is from an actual run of the actual code in
this directory (see Methodology) — none are estimated or simulated.

## The two services

- [`ledger.nir`](./ledger.nir) — `POST /api/transfer {from_id, to_id,
  amount_cents}`: reads both accounts' balances into local variables,
  `sleep_ms(2)` to model a real database round trip, then writes both
  new balances back. Served with `nirdosha serve`.
- [`ledger_naive.py`](./ledger_naive.py) — the same route, the same
  schema, the same read-both/sleep/write-both shape, in Python standard
  library (`http.server` + `sqlite3`, no `pip install`). One SQLite
  connection per *thread* (reused across that thread's requests, WAL
  mode, a `busy_timeout`) — matched deliberately to what
  `ledger.nir`'s own `db_connect` already gets for free (a pooled, WAL
  mode connection — `crates/compiler/src/dbconn.rs`), so the throughput
  numbers below compare the two runtimes, not "pooled vs. unpooled" or
  "WAL vs. rollback-journal." An earlier draft of this file used a
  fresh, unpooled, default-journal-mode connection per request on the
  Python side; that's a real, meaningful confound for a *speed*
  comparison (SQLite's default journal mode also serializes readers
  behind the one active writer) and has been fixed — see
  `ledger_naive.py`'s `_connect()` doc comment. It does **not** touch
  the race this demo is about: WAL mode changes who blocks whom at the
  storage-engine level, not whether the SELECT-then-UPDATE sequence is
  wrapped in a transaction — it still isn't, on either side.

Both start from the same invariant: two accounts, 1,000,000 cents each,
total 2,000,000. A transfer moves money between them; nothing should
ever change the total. Neither implementation takes a lock, wraps the
read+write in a transaction, or does anything else to protect that
invariant under concurrency — this is deliberately the naive shape both
languages make easy to write by accident, not a hardened one.

## Result 1: `nirdosha serve` vs. Python, over real HTTP

500 `POST /api/transfer` calls, fired at 50-way client concurrency via
`load_test.py`, checked against `POST /api/total` afterward.

| | `nirdosha serve` | Python, `HTTPServer` (single-threaded control) | Python, `ThreadingHTTPServer` (the realistic default) |
|---|---:|---:|---:|
| Runs | 2 | 2 | 6 |
| Runs with drift ≠ 0 | **0 / 2** | **0 / 2** | **5 / 6** |
| Throughput | 274–283 req/s | 67 req/s | 111–118 req/s |

Drift on every threaded-Python run: `0, -261, +93, -33, -144, +1`. Money
is created and destroyed in different runs — a lost-update race can go
either direction depending on which of two racing writes lands last,
and (like any real race) it doesn't fire on literally every run — run A
came back clean. 5 of 6 is the honest number, not "always," and it's
still a services-grade reliability disaster no one would accept.

Nirdosha and single-threaded Python conserved the total on every run,
with no exceptions. Nirdosha is also faster here — a Rust process
making one SQLite call at a time comfortably outruns CPython doing the
same, even with both sides now using a comparable pooled/WAL connection
strategy — but that gap is secondary to the correctness result, and (see
below) doesn't come from anything language-guaranteed either.

## Does the language itself prevent this?

**No — and this file said so only in prose the first time, which is a
fair thing to be skeptical of.** Here's the direct test.

[`race_probe.nir`](./race_probe.nir) takes `nirdosha serve` out of the
picture entirely. It's the exact same naive `transfer`/`total` logic as
`ledger.nir`, run from `main()` with Nirdosha's own `spawn`/`thread`/
`join` — 8 real OS threads, 250 transfers each, all hitting the same
pooled SQLite connection `db_connect` already shares process-wide. No
HTTP, no `serve.rs`, nothing to be single-threaded about.

```sh
nirdosha examples/killer_demo/race_probe.nir
```

Three runs, expected total 2,000,000 every time:

```
Run 1:  successful transfers: 2000   final total: 2000004
Run 2:  successful transfers: 2000   final total: 1999911
Run 3:  successful transfers: 2000   final total: 1999892
```

**It corrupts, every time — the same lost-update race, in pure
Nirdosha, with zero Python or HTTP involved.** This settles it: the
safety in Result 1 above is not a language-level data-race proof the
way `spawn`/`chan`'s *in-language* ownership guarantees already are
(`docs/LANGUAGE.md`, `examples/comparison/01-concurrent-counter.md`,
which is about a shared in-memory counter, a case the type system
genuinely does close off). It's a real, verifiable, today's-code
property of one specific thing: `nirdosha serve`'s request-handling loop
processes one request at a time (`serve.rs`'s own doc comment — no
`thread::spawn` anywhere in that file), so two `transfer` calls arriving
over HTTP can never be mid-flight together. The instant a program
introduces its own concurrency — `spawn`, exactly as intended — that
protection is gone, because `db`/`transact`/`http` are interpreter-only
today (Track B, `docs/PUBLIC_ROADMAP.md`) and carry no ownership
discipline of their own the way `box`/`chan` do.

## What this does and doesn't prove

- **Does prove**: today, `nirdosha serve` is a safer place to run this
  exact naive code than a threaded Python server is, because of a real,
  disclosed architectural fact about `serve.rs`, and it's also faster at
  this concurrency level.
- **Does not prove**: that Nirdosha the *language* makes this class of
  bug unrepresentable. It doesn't, yet — `race_probe.nir` is the
  counter-proof, produced on purpose. A future multi-threaded
  `serve.rs` would reintroduce exactly this race unless `db`/`transact`
  (or a new construct) grows a real concurrency-safety story first, not
  get to assume today's guarantee forever.
- **Also disclosed**: `nirdosha serve`'s single-threaded loop is a real,
  disclosed *cost* too — it cannot use more than one CPU core for
  request handling. The throughput numbers above show it winning
  anyway at this concurrency level, not that the ceiling doesn't exist.
- The Python side's residual `exceptions` (2–26 per run, present even in
  the non-corrupting single-threaded control) are `ConnectionResetError`s
  and occasional `database is locked` errors under 50-way concurrency —
  a disclosed, real characteristic of SQLite's single-writer model, not
  hidden from the numbers and not the mechanism this demo targets.
- The 50-way concurrency and 2ms simulated DB latency are the actual
  parameters used for every number in this file, not tuned per run.

## Methodology

Each service was started fresh, its ledger reset to the known starting
total via `POST /api/reset_ledger`, exactly 500 `POST /api/transfer`
calls were fired at it through a `ThreadPoolExecutor(max_workers=50)`,
and `POST /api/total` was read back afterward and diffed against the
starting total.

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
# SQLite-specific one.

# The language-level question, independent of serve.rs entirely:
nirdosha examples/killer_demo/race_probe.nir
```

`load_test.py --seed N` reproduces one specific request ordering; the
runs above used seeds 1/2 (Nirdosha, Python-sequential) and 1–6
(Python-threaded).
