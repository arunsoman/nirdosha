//! CLI entry point. Argument-parsing style deliberately mirrors
//! `crates/compiler/src/main.rs::cmd_serve` (a hand-rolled
//! `while let Some(a) = args.next()` loop, `--x-token`/`--x-token-file`
//! mutual exclusivity for every secret, `--jwks-file`/`--issuer`/
//! `--audience` as an all-or-nothing trio) rather than pulling in a CLI
//! argument-parsing crate for what's still a handful of flags — same "no
//! dependency this repo doesn't already need elsewhere" posture the rest
//! of this project takes.

use std::process::ExitCode;
use std::time::Duration;

use nirdosha_presence_gateway::gateway::{self, Config};
use nirdosha_presence_gateway::jwt::KeySet;
use nirdosha_presence_gateway::presence::PresenceClient;

fn usage() -> &'static str {
    "usage: nirdosha-presence-gateway \\
    --nirdosha-base-url URL \\
    (--presence-token TOKEN | --presence-token-file PATH) \\
    --jwks-file PATH --issuer ISSUER --audience AUDIENCE \\
    [--host 127.0.0.1] [--port 8090] \\
    [--redis-host 127.0.0.1] [--redis-port 6379]"
}

// Identical to `crates/compiler/src/main.rs::cmd_serve`'s own `read_token_file`:
// trims exactly one trailing newline (the common `echo token > file`/
// Kubernetes Secret convention), nothing else -- a token is opaque data,
// so no further parsing is safe to assume.
fn read_token_file(path: &str) -> Result<String, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("error reading {path}: {e}"))?;
    Ok(raw.strip_suffix('\n').map(str::to_string).unwrap_or(raw))
}

struct Args {
    host: String,
    port: u16,
    nirdosha_base_url: String,
    presence_token: String,
    jwks_file: String,
    issuer: String,
    audience: String,
    redis_host: String,
    redis_port: u16,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut host = "127.0.0.1".to_string();
    let mut port: u16 = 8090;
    let mut nirdosha_base_url: Option<String> = None;
    let mut presence_token: Option<String> = None;
    let mut presence_token_file: Option<String> = None;
    let mut jwks_file: Option<String> = None;
    let mut issuer: Option<String> = None;
    let mut audience: Option<String> = None;
    let mut redis_host = "127.0.0.1".to_string();
    let mut redis_port: u16 = 6379;

    while let Some(a) = args.next() {
        match a.as_str() {
            "--host" => host = args.next().ok_or("--host requires a value")?,
            "--port" => port = args.next().ok_or("--port requires a value")?.parse().map_err(|e| format!("--port: {e}"))?,
            "--nirdosha-base-url" => nirdosha_base_url = args.next(),
            // `docs/KUBERNETES.md`'s "Configuration (12-factor)" row: the raw
            // `--presence-token` value is visible via `/proc/<pid>/cmdline`
            // to anything sharing this process's PID namespace -- a real
            // leak in a Kubernetes Pod. `--presence-token-file` (a Secret
            // volume mount) is the way to avoid that, same precedent
            // `nirdosha serve`'s own `--presence-token`/`--otel-token`
            // already set.
            "--presence-token" => presence_token = args.next(),
            "--presence-token-file" => presence_token_file = args.next(),
            "--jwks-file" => jwks_file = args.next(),
            "--issuer" => issuer = args.next(),
            "--audience" => audience = args.next(),
            "--redis-host" => redis_host = args.next().ok_or("--redis-host requires a value")?,
            "--redis-port" => redis_port = args.next().ok_or("--redis-port requires a value")?.parse().map_err(|e| format!("--redis-port: {e}"))?,
            other => return Err(format!("unrecognized argument: {other}\n{}", usage())),
        }
    }

    let presence_token = match (presence_token, presence_token_file) {
        (Some(_), Some(_)) => return Err("--presence-token and --presence-token-file are mutually exclusive -- pass exactly one".to_string()),
        (Some(t), None) => t,
        (None, Some(f)) => read_token_file(&f)?,
        // Unlike `nirdosha serve`'s own `--presence-token` (optional --
        // its absence just 404s two routes, "a feature nobody opted into
        // costs nothing"), this gateway's *entire job* is calling those
        // two routes -- running with no token at all would mean every
        // call fails, silently doing nothing useful. Mandatory here.
        (None, None) => return Err(format!("--presence-token or --presence-token-file is required\n{}", usage())),
    };

    let nirdosha_base_url = nirdosha_base_url.ok_or_else(|| format!("--nirdosha-base-url is required\n{}", usage()))?;

    // All-or-nothing, same posture `nirdosha serve`'s own
    // `--jwks-file`/`--issuer`/`--audience` trio takes -- a WebSocket
    // gateway that accepted every connection unverified because one of
    // the three was missing would be a security hole disguised as "it
    // just worked", not a real degrade-gracefully case (unlike
    // `--presence-token`'s absence on `nirdosha serve`'s side, which just
    // 404s two routes instead of exposing anything).
    let (jwks_file, issuer, audience) = match (jwks_file, issuer, audience) {
        (Some(j), Some(i), Some(a)) => (j, i, a),
        _ => return Err(format!("--jwks-file/--issuer/--audience must all be given together\n{}", usage())),
    };

    Ok(Args { host, port, nirdosha_base_url, presence_token, jwks_file, issuer, audience, redis_host, redis_port })
}

fn run(args: Args) -> Result<(), String> {
    let jwks_json = std::fs::read_to_string(&args.jwks_file).map_err(|e| format!("error reading {}: {e}", args.jwks_file))?;
    let keys = KeySet::from_json(&jwks_json).map_err(|e| format!("{}: {e}", args.jwks_file))?;
    let presence = PresenceClient::new(args.nirdosha_base_url, args.presence_token);
    let redis_url = format!("redis://{}:{}/", args.redis_host, args.redis_port);

    let config = Config {
        host: args.host,
        port: args.port,
        keys,
        issuer: args.issuer,
        audience: args.audience,
        presence,
        redis_url,
        auth_timeout: Duration::from_secs(10),
        heartbeat_interval: Duration::from_secs(30),
        drain_timeout: Duration::from_secs(10),
    };

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(|e| format!("failed to start async runtime: {e}"))?;
    runtime.block_on(async move {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(wait_for_shutdown_signal(shutdown_tx));
        gateway::run(config, shutdown_rx).await.map_err(|e| format!("gateway exited with an I/O error: {e}"))
    })
}

/// Same two signals `nirdosha serve` itself handles (`serve.rs`:
/// `[signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT]`), via
/// `tokio::signal` instead of `signal-hook` -- an async-native equivalent
/// rather than a second signal-handling mechanism layered on top of the
/// one this process already needs for everything else. Unix-only,
/// deliberately: this is a container/Kubernetes-targeted sidecar, the
/// same audience `docs/KUBERNETES.md` scopes the rest of this feature to, not
/// a cross-platform desktop tool the way `nirdosha`'s own CLI has to be.
async fn wait_for_shutdown_signal(tx: tokio::sync::watch::Sender<bool>) {
    let mut sigterm = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to install SIGTERM handler: {e} (graceful shutdown on SIGTERM will not work)");
            return;
        }
    };
    let mut sigint = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to install SIGINT handler: {e} (graceful shutdown on Ctrl-C will not work)");
            return;
        }
    };
    tokio::select! {
        _ = sigterm.recv() => eprintln!("received SIGTERM, shutting down gracefully"),
        _ = sigint.recv() => eprintln!("received SIGINT, shutting down gracefully"),
    }
    let _ = tx.send(true);
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Checked before `parse_args`' own mandatory-field validation --
    // `--help`/`-h` should print usage and exit 0 regardless of whether
    // real config was also supplied, the same "docker run <image>"
    // (defaulting to `CMD ["--help"]`, this crate's own `Dockerfile`)
    // experience should give. Running with no arguments at all is left
    // to fall through to `parse_args`' ordinary validation instead of
    // being treated as an implicit help request -- missing required
    // config is a real startup error, not something to silently exit 0
    // on.
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", usage());
        return ExitCode::SUCCESS;
    }
    let parsed = match parse_args(args.into_iter()) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };
    match run(parsed) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}
