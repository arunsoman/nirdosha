//! ROADMAP.md Track A1: "actually kill the process mid-transaction under
//! load and confirm crash-replay behaves, not just trust the existing test
//! suite." `tests/transact_durability.rs` proves the replay *logic* by
//! seeding `TransactLog` rows by hand and calling
//! `replay_pending_transactions` in-process -- real, but it never actually
//! kills a real OS process. This file does the thing A1 asks for literally:
//! spawns a real `nirdosha serve` child process, throws real concurrent
//! HTTP load at it, SIGKILLs it mid-flight (`Child::kill`, a real signal to
//! a real PID, not a simulated crash), and restarts a fresh process against
//! the same `--transact-log` file to prove the durability log converges and
//! the actual business side effect (a real, separate SQLite "ledger" table
//! -- standing in for whatever a real `commit` would durably write) matches
//! it exactly, with nothing lost and nothing double-applied.
//!
//! Two kill/restart cycles, not one, so the final convergence check is
//! proven against the cumulative effect of two real crashes, not a single
//! lucky one.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nirdosha::transact_log::TransactLog;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").expect("binding a fresh loopback listener should never fail").local_addr().unwrap().port()
}

fn temp_path(name: &str) -> std::path::PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("nirdosha-killtest-{name}-{}-{n}", std::process::id()))
}

/// The `.nir` program under test: `checkout(amount)` is a `transact` whose
/// `commit` writes into a real, separate SQLite "ledger" file keyed by
/// `txn_id` (`ON CONFLICT ... DO NOTHING`) -- the idempotent-by-construction
/// shape a real `commit` needs so that a replay-driven re-invocation (after
/// a crash left the row `commit_pending`) can never double-apply the write.
/// `sleep_ms` in both `network` and `commit` widens each request's real
/// wall-clock duration, which is what makes "kill mid-flight" land inside
/// an in-progress `transact` under real concurrent load instead of always
/// landing between requests.
fn write_program(ledger_path: &std::path::Path) -> std::path::PathBuf {
    let ledger_str = ledger_path.to_string_lossy().replace('\\', "\\\\");
    let src = format!(
        r#"
        fn call_gateway(txn_id: str, amount: i64) -> i64 {{
            sleep_ms(3)
            return amount
        }}
        fn check(resp: i64) -> bool {{
            return resp >= 0
        }}
        fn apply_ledger(txn_id: str, amount: i64) -> i64 {{
            sleep_ms(3)
            return match db_connect("{ledger_str}") {{
                Ok(conn) => match db_execute(conn, "CREATE TABLE IF NOT EXISTS ledger (txn_id TEXT PRIMARY KEY, amount INTEGER)") {{
                    Ok(created) => match db_execute(conn, "INSERT INTO ledger (txn_id, amount) VALUES (?, ?) ON CONFLICT(txn_id) DO NOTHING", txn_id, amount) {{
                        Ok(n) => amount,
                        Err(e) => 1 / 0,
                    }},
                    Err(e) => 1 / 0,
                }},
                Err(e) => 1 / 0,
            }}
        }}
        fn checkout(amount: i64) -> bool {{
            return transact {{
                network: call_gateway(txn_id, amount)
                verify:  check(network)
                commit:  apply_ledger(txn_id, network)
            }}
        }}
        fn main() -> unit {{}}
    "#
    );
    let path = temp_path("program").with_extension("nir");
    std::fs::write(&path, src).expect("writing the test .nir program should succeed");
    path
}

/// Owns a spawned `nirdosha serve` child process and guarantees it's dead
/// before this guard goes away -- `std::process::Child` does *not* kill
/// its process on `Drop` (a plain `Child` left to fall out of scope after
/// a failed `assert!` panics mid-test would leak a real, still-running
/// server process holding its port and durability-log file open
/// indefinitely; this repo's own test run hit exactly that leak once,
/// during this file's own development, before this guard existed).
struct ServerGuard(Child);

impl ServerGuard {
    fn id(&self) -> u32 {
        self.0.id()
    }

    /// Explicit kill, for the test's own two deliberate mid-load kills --
    /// still safe to let `Drop` run afterward too (killing an
    /// already-exited process just fails harmlessly).
    fn kill_and_wait(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_server(nir_path: &std::path::Path, port: u16, transact_log: &std::path::Path) -> ServerGuard {
    let child = Command::new(env!("CARGO_BIN_EXE_nirdosha"))
        .arg("serve")
        .arg(nir_path)
        .arg("--port")
        .arg(port.to_string())
        .arg(format!("--transact-log={}", transact_log.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("nirdosha serve should spawn as a real child process");
    ServerGuard(child)
}

/// Crash replay (`serve.rs`) runs synchronously before the listener opens
/// (`TRANSACT.md`'s "Crash replay" section) -- so "the port is now
/// accepting connections" is exactly the right signal that replay already
/// finished, not something this test needs to separately poll for.
fn wait_for_port(port: u16) {
    for _ in 0..500 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("server on port {port} never came up");
}

/// One hand-rolled `POST /api/checkout` call with a short read/write
/// timeout, so a mid-flight SIGKILL (connection reset, or the server
/// simply never responding) fails fast instead of hanging the test.
/// Returns `Some(committed)` only for a real, complete HTTP 200 response --
/// anything else (refused connection, reset mid-read, non-200 status) is
/// `None`, meaning "this client never learned the outcome," which is
/// exactly the ambiguity `transact`'s durability story exists to resolve
/// without the client's help.
fn checkout(port: u16, amount: i64) -> Option<bool> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
    let body = format!("{{\"amount\":{amount}}}");
    let req = format!(
        "POST /api/checkout HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(req.as_bytes()).ok()?;
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).ok()?;
    let resp = String::from_utf8_lossy(&resp);
    let mut parts = resp.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or("");
    let body = parts.next().unwrap_or("");
    let status: u16 = head.lines().next()?.split_whitespace().nth(1)?.parse().ok()?;
    if status != 200 {
        return None;
    }
    Some(body.trim() == "true")
}

/// A batch of client load in flight: `handles` are still running,
/// `confirmed` accumulates every amount whose client actually observed a
/// definite `true` response so far. Kept separate from joining (below) so
/// the caller can kill the server *while these are still in flight* --
/// joining first would mean every request already finished, which would
/// prove nothing about crash recovery.
struct LoadInFlight {
    handles: Vec<std::thread::JoinHandle<()>>,
    confirmed: Arc<Mutex<Vec<i64>>>,
}

/// Starts `num_threads` concurrent client threads firing `checkout` calls
/// across every amount in `amounts` (a shared atomic cursor divides the
/// work) and returns immediately, once real load has had time to actually
/// start overlapping in-flight requests -- the caller kills the server
/// right after this returns, then calls `join_load` to collect results.
fn start_load(port: u16, amounts: std::ops::RangeInclusive<i64>, num_threads: usize) -> LoadInFlight {
    let cursor = Arc::new(AtomicU64::new(*amounts.start() as u64));
    let end = *amounts.end() as u64;
    let confirmed = Arc::new(Mutex::new(Vec::new()));
    let handles: Vec<_> = (0..num_threads)
        .map(|_| {
            let cursor = Arc::clone(&cursor);
            let confirmed = Arc::clone(&confirmed);
            std::thread::spawn(move || loop {
                let a = cursor.fetch_add(1, Ordering::Relaxed);
                if a > end {
                    break;
                }
                if let Some(true) = checkout(port, a as i64) {
                    confirmed.lock().unwrap().push(a as i64);
                }
            })
        })
        .collect();
    std::thread::sleep(Duration::from_millis(300));
    LoadInFlight { handles, confirmed }
}

/// Waits for a load batch's client threads to finish (fast, once the
/// server they were talking to is dead -- every further connect attempt
/// fails immediately) and returns every amount confirmed `true`.
fn join_load(load: LoadInFlight) -> Vec<i64> {
    for h in load.handles {
        let _ = h.join();
    }
    load.confirmed.lock().unwrap().clone()
}

const BATCH: i64 = 240;
const THREADS: usize = 12;

#[test]
fn kill_mid_transaction_under_concurrent_load_converges_after_two_real_crashes() {
    let ledger_path = temp_path("ledger").with_extension("db");
    let transact_log = temp_path("txlog").with_extension("db");
    let nir_path = write_program(&ledger_path);

    let mut confirmed_committed: Vec<i64> = Vec::new();

    // ---- Round 1: real load, real SIGKILL mid-flight ----------------------
    let port1 = free_port();
    let mut child1 = spawn_server(&nir_path, port1, &transact_log);
    wait_for_port(port1);
    let load1 = start_load(port1, 1..=BATCH, THREADS);
    let killed_pid1 = child1.id();
    child1.kill_and_wait();
    confirmed_committed.extend(join_load(load1));

    assert!(
        confirmed_committed.len() < BATCH as usize,
        "the kill landed only after every request in round 1 had already completed -- this run proves \
         nothing about crash recovery under load; got {} / {BATCH} confirmed before the kill",
        confirmed_committed.len()
    );

    // ---- Restart: replay must run before this process's port opens -------
    let port2 = free_port();
    let mut child2 = spawn_server(&nir_path, port2, &transact_log);
    wait_for_port(port2);
    assert_ne!(killed_pid1, child2.id(), "sanity: round 2 must run against a genuinely new process, not the one just killed");

    // ---- Round 2: more real load against the restarted process, killed again -
    let load2 = start_load(port2, (BATCH + 1)..=(2 * BATCH), THREADS);
    let killed_pid2 = child2.id();
    child2.kill_and_wait();
    confirmed_committed.extend(join_load(load2));

    // ---- Final restart: this replay must resolve everything from both crashes
    let port3 = free_port();
    let child3 = spawn_server(&nir_path, port3, &transact_log);
    wait_for_port(port3);
    assert_ne!(killed_pid2, child3.id(), "sanity: the final check must run against a genuinely new process too");

    // ---- The durability log must be fully resolved -- A1's actual ask ----
    let tlog = TransactLog::open(&nirdosha::durability::LogTarget::Sqlite(transact_log.clone()))
        .expect("the durability log should still open after two real crashes");
    let unresolved = tlog.list_unresolved().expect("list_unresolved should succeed");
    assert!(unresolved.is_empty(), "crash replay left unresolved rows after two real SIGKILLs: {unresolved:?}");

    // ---- The real business side effect must match the log exactly --------
    let ledger_conn = rusqlite::Connection::open(&ledger_path).expect("the ledger db should exist");
    let ledger_count: i64 = ledger_conn.query_row("SELECT count(*) FROM ledger", [], |r| r.get(0)).expect("ledger table should exist");

    let raw_log = rusqlite::Connection::open(&transact_log).expect("the log db should also open directly, for raw state counts");
    let committed_count: i64 =
        raw_log.query_row("SELECT count(*) FROM transact_log WHERE state = 'committed'", [], |r| r.get(0)).expect("query should succeed");
    let compensated_count: i64 =
        raw_log.query_row("SELECT count(*) FROM transact_log WHERE state = 'compensated'", [], |r| r.get(0)).expect("query should succeed");
    let non_terminal_count: i64 = raw_log
        .query_row("SELECT count(*) FROM transact_log WHERE state NOT IN ('committed', 'compensated')", [], |r| r.get(0))
        .expect("query should succeed");

    assert_eq!(non_terminal_count, 0, "every row must have reached a terminal state after two replay passes");
    assert_eq!(compensated_count, 0, "`check` never returns false in this program -- nothing should ever compensate");
    assert_eq!(
        ledger_count, committed_count,
        "the real ledger table must have exactly one row per committed transact after crash replay -- a mismatch \
         here means either a lost write (the log says committed but the ledger disagrees) or a double-apply"
    );

    // ---- Every response a client actually saw as `true` must be durable ---
    let missing: Vec<i64> = confirmed_committed
        .iter()
        .copied()
        .filter(|amount| {
            let exists: i64 =
                ledger_conn.query_row("SELECT count(*) FROM ledger WHERE amount = ?1", [amount], |r| r.get(0)).unwrap_or(0);
            exists == 0
        })
        .collect();
    assert!(
        missing.is_empty(),
        "a client observed a committed `true` response for these amounts, but they're missing from the ledger \
         after crash replay -- a committed response must never be a lie: {missing:?}"
    );

    // `child3`'s `Drop` (`ServerGuard`) kills and reaps it once it goes
    // out of scope at the end of this function -- no manual kill needed
    // here, and it also fires on an early panic from any `assert!` above,
    // which a bare `Child` would not have.
    let _ = std::fs::remove_file(&nir_path);
    let _ = std::fs::remove_file(&ledger_path);
    let _ = std::fs::remove_file(&transact_log);
}
