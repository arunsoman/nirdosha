#!/usr/bin/env python3
"""The direct Python twin of examples/killer_demo/race_probe.nir --
same schema, same naive transfer logic, same workload shape (8 real OS
threads, 250 transfers each, sleep(2ms) between reading both balances
and writing them back), driven with Python's own `threading.Thread`
instead of Nirdosha's `spawn`/`thread`/`join`. No HTTP, no server, on
either side -- this is the fully apples-to-apples comparison: the same
naive read-modify-write shape, run under each language's own native
concurrency primitive, in-process, hitting the same kind of SQLite file
the same way. See RESULTS.md.

One connection per thread, opened once and reused for that thread's 250
iterations (WAL mode, a busy_timeout) -- the same fairness fix
RESULTS.md's superseded HTTP comparison needed, applied here from the
start: an unpooled, default-journal-mode connection per call would be a
confound for a throughput comparison, though (see race_probe.nir's own
doc comment) it wouldn't change whether the race itself reproduces.

Standard library only -- no `pip install` needed.

Run it:
    python3 examples/killer_demo/race_probe.py
"""
import sqlite3
import threading
import time

DB_PATH = "examples/killer_demo/race_probe_py.db"
SLEEP_MS = 2  # matches race_probe.nir's transfer_inner sleep_ms(2)


def _connect():
    conn = sqlite3.connect(DB_PATH, isolation_level=None, timeout=5.0)
    conn.execute("PRAGMA journal_mode = WAL")
    conn.execute("PRAGMA busy_timeout = 5000")
    return conn


def reset_ledger():
    conn = _connect()
    conn.execute("CREATE TABLE IF NOT EXISTS account (id INTEGER PRIMARY KEY, name TEXT, balance_cents INTEGER)")
    conn.execute("DELETE FROM account")
    conn.execute("INSERT INTO account (id, name, balance_cents) VALUES (1, 'alice', 1000000)")
    conn.execute("INSERT INTO account (id, name, balance_cents) VALUES (2, 'bob', 1000000)")
    conn.close()


def total():
    conn = _connect()
    cur = conn.execute("SELECT COALESCE(SUM(balance_cents), 0) FROM account")
    result = cur.fetchone()[0]
    conn.close()
    return result


def _row_balance(conn, account_id):
    cur = conn.execute("SELECT balance_cents FROM account WHERE id = ?", (account_id,))
    row = cur.fetchone()
    return None if row is None else row[0]


def transfer(conn, from_id, to_id, amount_cents):
    """Identical shape to race_probe.nir's transfer_inner: read both
    balances, sleep to model a DB round trip, write both back."""
    from_balance = _row_balance(conn, from_id)
    if from_balance is None or from_balance < amount_cents:
        return False
    to_balance = _row_balance(conn, to_id)
    if to_balance is None:
        return False

    time.sleep(SLEEP_MS / 1000.0)

    new_from = from_balance - amount_cents
    new_to = to_balance + amount_cents
    conn.execute("UPDATE account SET balance_cents = ? WHERE id = ?", (new_from, from_id))
    conn.execute("UPDATE account SET balance_cents = ? WHERE id = ?", (new_to, to_id))
    return True


def worker(from_id, to_id, iterations, results, index):
    """Runs on its own real OS thread (Python's `threading.Thread` --
    the direct analogue of race_probe.nir's `spawn`), one connection for
    all `iterations` transfers, exactly mirroring race_probe.nir's
    `worker`."""
    conn = _connect()
    successes = 0
    for _ in range(iterations):
        if transfer(conn, from_id, to_id, 1):
            successes += 1
    conn.close()
    results[index] = successes


def main():
    reset_ledger()
    print("ledger reset")

    # 8 real OS threads, 250 transfers each -- 2,000 concurrent
    # transfers total, identical to race_probe.nir's main().
    specs = [(1, 2), (2, 1)] * 4
    results = [0] * len(specs)
    threads = [
        threading.Thread(target=worker, args=(from_id, to_id, 250, results, i))
        for i, (from_id, to_id) in enumerate(specs)
    ]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    print("successful transfers:")
    print(sum(results))

    final_total = total()
    print("final total (expected 2000000):")
    print(final_total)


if __name__ == "__main__":
    main()
