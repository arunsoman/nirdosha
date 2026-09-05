#!/usr/bin/env python3
"""Fires concurrent /api/transfer requests at a running ledger service
(either examples/killer_demo/ledger.nir via `nirdosha serve`, or its
Python twin, ledger_naive.py) and checks one invariant: the sum of every
account's balance never changes. Money can move between the two
accounts; it can never be created or destroyed. Also reports wall-clock
throughput for the same batch, so this one script produces both the
correctness and the speed numbers RESULTS.md reports.

Standard library only (urllib + concurrent.futures) -- no `pip install`
needed.

Usage:
    python3 examples/killer_demo/load_test.py --base-url http://127.0.0.1:8099 --n 500 --concurrency 50
"""
import argparse
import json
import random
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed

INITIAL_TOTAL = 2_000_000


def post(base_url, fn_name, body):
    data = json.dumps(body).encode("utf-8")
    req = urllib.request.Request(f"{base_url}/api/{fn_name}", data=data, method="POST")
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read())


def one_transfer(base_url, rng):
    from_id, to_id = (1, 2) if rng.random() < 0.5 else (2, 1)
    amount = rng.randint(1, 50)  # cents -- small relative to the 1,000,000-cent starting balance
    return post(base_url, "transfer", {"from_id": from_id, "to_id": to_id, "amount_cents": amount})


def run(base_url, n, concurrency, seed):
    print(f"== {base_url}  (n={n} requests, concurrency={concurrency}) ==")
    post(base_url, "reset_ledger", {})
    before = post(base_url, "total", {})["ok"]
    assert before == INITIAL_TOTAL, f"reset didn't produce the expected starting total: got {before}"

    rng_master = random.Random(seed)
    seeds = [rng_master.randrange(2**32) for _ in range(n)]

    outcomes = {"ok": 0, "InsufficientFunds": 0, "other_err": 0, "exceptions": 0}
    started = time.perf_counter()
    with ThreadPoolExecutor(max_workers=concurrency) as pool:
        futures = [pool.submit(one_transfer, base_url, random.Random(s)) for s in seeds]
        for fut in as_completed(futures):
            try:
                result = fut.result()
            except Exception:
                outcomes["exceptions"] += 1
                continue
            if "ok" in result:
                outcomes["ok"] += 1
            elif result.get("err", {}).get("variant") == "InsufficientFunds":
                outcomes["InsufficientFunds"] += 1
            else:
                outcomes["other_err"] += 1
    elapsed = time.perf_counter() - started

    after = post(base_url, "total", {})["ok"]
    drift = after - before

    print(f"  requests:        {n} in {elapsed:.3f}s  ({n / elapsed:.1f} req/s)")
    print(f"  outcomes:        {outcomes}")
    print(f"  total before:    {before}")
    print(f"  total after:     {after}")
    print(f"  drift:           {drift}  ({'CORRUPTED -- money was created or destroyed' if drift != 0 else 'conserved, exactly as expected'})")
    print(f"  verdict:         {'FAIL' if drift != 0 else 'PASS'}")
    print()
    return {"elapsed": elapsed, "throughput": n / elapsed, "outcomes": outcomes, "before": before, "after": after, "drift": drift}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base-url", required=True)
    ap.add_argument("--n", type=int, default=500)
    ap.add_argument("--concurrency", type=int, default=50)
    ap.add_argument("--seed", type=int, default=42)
    args = ap.parse_args()
    run(args.base_url, args.n, args.concurrency, args.seed)


if __name__ == "__main__":
    main()
