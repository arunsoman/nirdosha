# Racing a naive money-transfer ledger, Nirdosha vs. Python — apples to apples

**This file went through two drafts that didn't hold up, and says so
here rather than quietly fixing them.** The first draft compared
`nirdosha serve` (HTTP) against a Python `ThreadingHTTPServer` (HTTP)
and reported Nirdosha as uncorrupted — true, but for the wrong reason:
`nirdosha serve`'s request loop happens to be single-threaded today
(`serve.rs`'s own doc comment), so the comparison was really "sequential
vs. concurrent," not "Nirdosha vs. Python." The second draft added
`race_probe.nir` to test that directly (see below) and confirmed it: the
same naive logic, run through Nirdosha's own `spawn`/`thread`, corrupts
the ledger too. That draft still compared apples to oranges on the
*Python* side, though — an HTTP server with no Nirdosha HTTP counterpart
left to compare it against. **This version removes the server layer on
both sides entirely.** `ledger.nir` and its HTTP-serving Python twin are
gone. What's left is the actually-fair comparison: the same naive
transfer logic, run under each language's own native concurrency
primitive, in-process, no HTTP, no server, hitting the same kind of
SQLite file the same way.

Every number below is from an actual run of the actual code in this
directory — none are estimated or simulated.

## The two probes

- [`race_probe.nir`](./race_probe.nir) — Nirdosha's own `spawn`/
  `thread`/`join`: 8 real OS threads, 250 transfers each (2,000 total),
  split evenly between the two directions. Each transfer reads both
  balances, `sleep_ms(2)` to model a real DB round trip, then writes
  both new balances back — no lock, no transaction, no version check.
- [`race_probe.py`](./race_probe.py) — line-for-line the same workload
  and the same naive shape, using Python's `threading.Thread` instead:
  8 real OS threads, 250 transfers each, `time.sleep(0.002)` in the same
  spot. One SQLite connection per thread, opened once and reused for
  that thread's 250 iterations, WAL mode, a `busy_timeout` — matched
  deliberately to `race_probe.nir`'s own `db_connect` (pooled, WAL mode
  — `crates/compiler/src/dbconn.rs`), so neither side pays a
  connection-setup or journal-mode tax the other doesn't.

Both start from the same invariant: two accounts, 1,000,000 cents each,
total 2,000,000. Nothing about either program tries to protect that
invariant under concurrency — that's the point of both.

## Result: both race. Neither language's `db` layer prevents this.

| | `race_probe.nir` | `race_probe.py` |
|---|---:|---:|
| Runs | 3 | 3 |
| Ledger drift per run | `+4, -89, -108` | `-54, +82, +384` |
| Runs with drift ≠ 0 | **3 / 3** | **3 / 3** |
| Wall-clock time | 0.97–1.16 s | 18.3–19.1 s |

**Every run of both probes corrupted the ledger.** Money is created and
destroyed in different runs on both sides — the textbook lost-update
race, reproducing reliably at this thread count and iteration count in
both languages. This is the honest, apples-to-apples finding: Nirdosha's
`db`/`http`/`transact` layer is interpreter-only today and carries no
ownership discipline of its own the way `box`/`chan` do
(`docs/PUBLIC_ROADMAP.md`'s Track B) — nothing about the language stops
a program from writing exactly this bug and then actually running it
concurrently. `examples/comparison/01-concurrent-counter.md`'s claim
(no data race is expressible over `chan`/`spawn`'s own in-memory
primitives) is real and unaffected by this result; it just doesn't
extend to `db` yet, and this file is the demonstration of that specific,
disclosed gap.

**What does differ, by a wide margin, is speed on identical work**: 8
threads doing the same 2,000 SELECT/sleep/UPDATE-shaped transfers
finished in ~1 second on Nirdosha and ~18–19 seconds on Python — roughly
an 18x gap, on the same machine, same SQLite file, same WAL mode, same
per-thread-connection-reuse strategy on both sides. That gap is a real
Rust-vs-CPython-plus-GIL runtime difference under write contention, not
an artifact of unequal setup.

## What this does and doesn't prove

- **Does not prove**: Nirdosha is safer than Python for concurrent
  database access. It isn't, today — this file is the corrected
  record after two drafts that implied otherwise for the wrong reasons.
- **Does prove**: for the identical naive, unsynchronized workload, run
  under each language's own native concurrency primitive, Nirdosha is
  roughly 18x faster on this machine. That's a real, measured, and (as
  far as this demo goes) fair result.
- **Separately real, not superseded by this file**: Nirdosha's
  in-language concurrency primitives (`spawn`/`thread`/`chan`, no mutex
  in the grammar at all) genuinely do make a shared-in-memory-state data
  race unrepresentable — see `docs/LANGUAGE.md` and
  `examples/comparison/01-concurrent-counter.md`. That guarantee is
  real; it just has a boundary, and `db` is on the far side of it today.
  Closing that gap (a `transact`-style construct, or serializable
  isolation built into `db`'s own concurrent-access story) is
  `docs/PUBLIC_ROADMAP.md` Track B territory, not yet shipped.
- The 8-thread/250-iteration/2ms-sleep parameters are the actual
  numbers used for every run above, not tuned per run.

## Methodology

```sh
# Nirdosha
nirdosha examples/killer_demo/race_probe.nir

# Python
python3 examples/killer_demo/race_probe.py
```

Each run: reset the ledger to the known starting total in-process,
spawn 8 threads (4 each direction) each doing 250 transfers of 1 cent,
join all of them, then read the total back and diff it against the
starting total. Run three times per language; every number reported
above is from one of those six runs, unedited.
