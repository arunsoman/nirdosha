#!/usr/bin/env python3
"""The Python twin of examples/killer_demo/ledger.nir -- same schema,
same routes, same JSON response shape ({"ok": ...} / {"err": {...}}),
and deliberately the *same* naive read-modify-write shape as the
Nirdosha side's `transfer_inner`: SELECT both balances into Python
variables, sleep to model a real DB round trip, then write both new
values back. No lock, no transaction isolation bump, no optimistic
version check -- because most real naive deployments don't have one
either. See examples/killer_demo/RESULTS.md for what this demonstrates.

Standard library only (sqlite3 + http.server) -- no `pip install`
needed, so anyone cloning the repo gets the exact same run.

Run it:
    python3 examples/killer_demo/ledger_naive.py --port 8100 \
        --db examples/killer_demo/ledger_naive.db

The one deliberate, disclosed difference from a typical Flask app:
`--threaded` controls whether requests are served concurrently
(`ThreadingHTTPServer`, the realistic default -- Flask's dev server with
`threaded=True`, or any WSGI server with more than one worker thread)
or one at a time (`HTTPServer`, matching Nirdosha's own single-threaded
`serve.rs`) -- see RESULTS.md for why both runs are reported.
"""
import argparse
import json
import sqlite3
import time
from http.server import BaseHTTPRequestHandler, HTTPServer, ThreadingHTTPServer

DB_PATH = "examples/killer_demo/ledger_naive.db"
SLEEP_MS = 2  # matches ledger.nir's transfer_inner sleep_ms(2)
_db_path = DB_PATH  # set from argv in main()


def _connect():
    # A fresh connection per request -- the idiomatic pattern real
    # frameworks use (Flask's `g.db`, a SQLAlchemy scoped session), and
    # deliberately *not* one global connection shared across threads:
    # this demo's point is the missing transaction isolation around the
    # SELECT-then-UPDATE sequence below, not "don't share a raw SQLite
    # handle across threads" (a real but different bug -- see
    # RESULTS.md's methodology note for why that one's fixed here
    # instead of left in). `busy_timeout` makes a concurrent writer wait
    # for SQLite's own file lock instead of raising "database is
    # locked", so what this demo measures is the logical race, not
    # filesystem lock contention.
    conn = sqlite3.connect(_db_path, isolation_level=None, timeout=5.0)
    conn.execute("PRAGMA busy_timeout = 5000")
    return conn


def _row_balance(conn, account_id):
    cur = conn.execute("SELECT balance_cents FROM account WHERE id = ?", (account_id,))
    row = cur.fetchone()
    return None if row is None else row[0]


def reset_ledger():
    conn = _connect()
    conn.execute("CREATE TABLE IF NOT EXISTS account (id INTEGER PRIMARY KEY, name TEXT, balance_cents INTEGER)")
    conn.execute("DELETE FROM account")
    conn.execute("INSERT INTO account (id, name, balance_cents) VALUES (1, 'alice', 1000000)")
    conn.execute("INSERT INTO account (id, name, balance_cents) VALUES (2, 'bob', 1000000)")
    conn.close()
    return {"ok": {"value": "reset"}}


def balance(body):
    account_id = body["id"]
    conn = _connect()
    bal = _row_balance(conn, account_id)
    conn.close()
    if bal is None:
        return {"err": {"variant": "NotFound", "payload": ["account"]}}
    return {"ok": bal}


def total():
    conn = _connect()
    cur = conn.execute("SELECT COALESCE(SUM(balance_cents), 0) FROM account")
    result = cur.fetchone()[0]
    conn.close()
    return {"ok": result}


def transfer(body):
    from_id = body["from_id"]
    to_id = body["to_id"]
    amount_cents = body["amount_cents"]
    if amount_cents <= 0:
        return {"err": {"variant": "InvalidAmount", "payload": ["amount_cents must be positive"]}}

    conn = _connect()
    from_balance = _row_balance(conn, from_id)
    if from_balance is None:
        conn.close()
        return {"err": {"variant": "NotFound", "payload": ["from account"]}}
    if from_balance < amount_cents:
        conn.close()
        return {"err": {"variant": "InsufficientFunds", "payload": ["not enough funds"]}}
    to_balance = _row_balance(conn, to_id)
    if to_balance is None:
        conn.close()
        return {"err": {"variant": "NotFound", "payload": ["to account"]}}

    # Models a real network round trip to the database between reading
    # both balances and writing them back -- see module docstring.
    time.sleep(SLEEP_MS / 1000.0)

    new_from = from_balance - amount_cents
    new_to = to_balance + amount_cents
    conn.execute("UPDATE account SET balance_cents = ? WHERE id = ?", (new_from, from_id))
    conn.execute("UPDATE account SET balance_cents = ? WHERE id = ?", (new_to, to_id))
    conn.close()
    return {"ok": {"value": "transferred"}}


ROUTES = {
    "reset_ledger": lambda body: reset_ledger(),
    "balance": balance,
    "total": lambda body: total(),
    "transfer": transfer,
}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        pass  # quiet -- load_test.py reports its own summary

    def do_POST(self):
        if not self.path.startswith("/api/"):
            self._send(404, {"err": "not found"})
            return
        fn_name = self.path[len("/api/"):]
        handler = ROUTES.get(fn_name)
        if handler is None:
            self._send(404, {"err": f"no such function `{fn_name}`"})
            return
        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length) if length else b"{}"
        try:
            body = json.loads(raw) if raw.strip() else {}
        except json.JSONDecodeError as e:
            self._send(400, {"err": f"malformed JSON body: {e}"})
            return
        try:
            result = handler(body)
        except Exception as e:  # pragma: no cover - defense in depth, mirrors serve.rs's catch_unwind
            self._send(500, {"err": f"`{fn_name}` raised: {e}"})
            return
        self._send(200, result)

    def _send(self, status, payload):
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    global _db_path
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8100)
    ap.add_argument("--db", default=DB_PATH)
    ap.add_argument("--threaded", action="store_true", help="serve requests concurrently (the realistic default -- see module docstring)")
    args = ap.parse_args()

    _db_path = args.db
    reset_ledger()

    server_cls = ThreadingHTTPServer if args.threaded else HTTPServer
    httpd = server_cls((args.host, args.port), Handler)
    mode = "threaded (concurrent)" if args.threaded else "single-threaded (sequential)"
    print(f"ledger_naive.py listening on http://{args.host}:{args.port}  [{mode}]", flush=True)
    httpd.serve_forever()


if __name__ == "__main__":
    main()
