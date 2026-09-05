// Isolated micro-benchmark of Nirdosha's *actual* dispatch mechanisms,
// reproduced faithfully from the real source (not a toy approximation):
//
//   - `ast::BUILTIN_NAMES` is a `&[&str]` of 48 real names (copied
//     verbatim from crates/compiler/src/ast.rs), and `is_builtin(name)`
//     is exactly `BUILTIN_NAMES.contains(&name)` -- a linear scan, not a
//     hash lookup. This runs on the path to EVERY call in the
//     interpreter (`eval_expr`'s `Expr::Call` arm checks it before
//     falling through to a plugin or a user fn), so both the "real
//     builtin" and "plugin" cases below pay it.
//   - The plugin path is exactly `self.plugins.get(name).cloned()` then
//     an indirect call through the `Arc<dyn Fn>` -- reproduced here with
//     the identical types (`PluginFn = Arc<dyn Fn(&[Value], Span) ->
//     Result<Value, RuntimeError> + Send + Sync>`).
//   - The "real builtin" path is `eval_builtin`'s own shape: a `match
//     name { "a" => ..., "b" => ..., ... }` over the same 48 names, one
//     arm per name, each doing trivial (but not optimized-away) work.
//
// Three call sites benchmarked, 20,000,000 iterations each, best of 5:
//   1. `is_builtin("rot13")` alone (guaranteed miss -- the exact cost
//      every plugin call and every user-fn call pays before falling
//      through, since a plugin/user-fn name can never collide with a
//      real builtin).
//   2. Plugin dispatch: `is_builtin` (miss) + HashMap lookup (hit) +
//      indirect call through `Arc<dyn Fn>`.
//   3. Real-builtin dispatch: `is_builtin` (hit, at `rand_gaussian`'s
//      real position -- picked because plugin `rot13`'s reference
//      workload does comparable single-arg trivial work) + the `match`
//      dispatch + inline work.
//   4. A direct, static Rust function call doing the identical trivial
//      work -- the zero-dispatch floor everything else is measured
//      against.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

// Verbatim copy of crates/compiler/src/ast.rs's BUILTIN_NAMES (48
// entries) -- see that file for the real list; reproduced here so this
// benchmark needs no dependency on the `nirdosha` crate itself.
const BUILTIN_NAMES: &[&str] = &[
    "print", "transpose", "dot", "cross", "zeros", "ones", "identity", "sum", "len", "norm",
    "norm1", "norm_inf", "frobenius_norm", "trace", "det", "inv", "solve", "rank", "is_symmetric",
    "is_diag", "is_square", "rand_seed", "rand_f64", "rand_gaussian", "sleep_ms", "distance",
    "bearing", "lla_to_ecef", "ecef_to_lla", "ecef_to_enu", "enu_to_ecef", "kf_predict_state",
    "kf_predict_cov", "kf_update_state", "kf_update_cov", "json_parse", "json_get",
    "json_get_str", "json_get_i64", "json_get_f64", "json_get_bool", "json_array_len",
    "json_array_get", "json_set_str", "http_get", "http_post", "https_get", "https_post",
];

#[inline(never)]
fn is_builtin(name: &str) -> bool {
    BUILTIN_NAMES.contains(&name)
}

#[derive(Clone, Copy)]
struct Span {
    line: u32,
    col: u32,
}

#[derive(Clone)]
enum Value {
    Int(i64),
    Str(Arc<str>),
}

#[derive(Debug)]
struct RuntimeError;

type PluginFn = Arc<dyn Fn(&[Value], Span) -> Result<Value, RuntimeError> + Send + Sync>;

// The real rot13_call body, copied from crates/plugin-example-rot13/src/lib.rs.
fn rot13_call(args: &[Value], _span: Span) -> Result<Value, RuntimeError> {
    let Value::Str(s) = &args[0] else { unreachable!() };
    let rotated: String = s
        .chars()
        .map(|c| match c {
            'a'..='z' => (((c as u8 - b'a' + 13) % 26) + b'a') as char,
            'A'..='Z' => (((c as u8 - b'A' + 13) % 26) + b'A') as char,
            other => other,
        })
        .collect();
    Ok(Value::Str(Arc::from(rotated.as_str())))
}

#[inline(never)]
fn rot13_direct(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    rot13_call(args, span)
}

// A representative "real builtin" arm: comparable single-arg trivial
// work, sitting at `rand_gaussian`'s real position (index 23 of 48) in
// the match/scan order -- the same relative dispatch cost a builtin at
// that position actually pays.
#[inline(never)]
fn eval_builtin_like(name: &str, args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    match name {
        "print" => Ok(Value::Int(0)),
        "transpose" => Ok(Value::Int(0)),
        "dot" => Ok(Value::Int(0)),
        "cross" => Ok(Value::Int(0)),
        "zeros" => Ok(Value::Int(0)),
        "ones" => Ok(Value::Int(0)),
        "identity" => Ok(Value::Int(0)),
        "sum" => Ok(Value::Int(0)),
        "len" => Ok(Value::Int(0)),
        "norm" => Ok(Value::Int(0)),
        "norm1" => Ok(Value::Int(0)),
        "norm_inf" => Ok(Value::Int(0)),
        "frobenius_norm" => Ok(Value::Int(0)),
        "trace" => Ok(Value::Int(0)),
        "det" => Ok(Value::Int(0)),
        "inv" => Ok(Value::Int(0)),
        "solve" => Ok(Value::Int(0)),
        "rank" => Ok(Value::Int(0)),
        "is_symmetric" => Ok(Value::Int(0)),
        "is_diag" => Ok(Value::Int(0)),
        "is_square" => Ok(Value::Int(0)),
        "rand_seed" => Ok(Value::Int(0)),
        "rand_f64" => Ok(Value::Int(0)),
        // Real position of a single-str-arg-shaped call in the arm
        // order -- does the identical rot13 work so the two dispatch
        // paths' *own* overhead is what differs, not the payload work.
        "rand_gaussian" => rot13_call(args, span),
        "sleep_ms" => Ok(Value::Int(0)),
        "distance" => Ok(Value::Int(0)),
        "bearing" => Ok(Value::Int(0)),
        "lla_to_ecef" => Ok(Value::Int(0)),
        "ecef_to_lla" => Ok(Value::Int(0)),
        "ecef_to_enu" => Ok(Value::Int(0)),
        "enu_to_ecef" => Ok(Value::Int(0)),
        "kf_predict_state" => Ok(Value::Int(0)),
        "kf_predict_cov" => Ok(Value::Int(0)),
        "kf_update_state" => Ok(Value::Int(0)),
        "kf_update_cov" => Ok(Value::Int(0)),
        "json_parse" => Ok(Value::Int(0)),
        "json_get" => Ok(Value::Int(0)),
        "json_get_str" => Ok(Value::Int(0)),
        "json_get_i64" => Ok(Value::Int(0)),
        "json_get_f64" => Ok(Value::Int(0)),
        "json_get_bool" => Ok(Value::Int(0)),
        "json_array_len" => Ok(Value::Int(0)),
        "json_array_get" => Ok(Value::Int(0)),
        "json_set_str" => Ok(Value::Int(0)),
        "http_get" => Ok(Value::Int(0)),
        "http_post" => Ok(Value::Int(0)),
        "https_get" => Ok(Value::Int(0)),
        "https_post" => Ok(Value::Int(0)),
        _ => unreachable!(),
    }
}

fn bench(label: &str, iters: u64, mut f: impl FnMut() -> u64) {
    let mut best = std::time::Duration::MAX;
    let mut checksum = 0u64;
    for _ in 0..5 {
        let start = Instant::now();
        for _ in 0..iters {
            checksum ^= f();
        }
        let elapsed = start.elapsed();
        if elapsed < best {
            best = elapsed;
        }
    }
    let ns_per_iter = best.as_nanos() as f64 / iters as f64;
    println!("{label:<45} best of 5: {:>10.3} ms total   {:>8.2} ns/call   (checksum {checksum})", best.as_secs_f64() * 1000.0, ns_per_iter);
}

fn main() {
    let iters: u64 = 20_000_000;
    let s = Value::Str(Arc::from("Hello, Nirdosha! this is a representative short string."));
    let args = [s];
    let span = Span { line: 1, col: 1 };

    // 1. is_builtin alone, guaranteed miss (what every plugin/user-fn
    //    call pays before falling through).
    bench("1. is_builtin(\"rot13\") -- guaranteed miss", iters, || {
        std::hint::black_box(is_builtin(std::hint::black_box("rot13")));
        1
    });

    // 2. Full plugin dispatch: is_builtin (miss) + HashMap lookup (hit)
    //    + Arc<dyn Fn> clone + indirect call.
    let mut plugins: HashMap<String, PluginFn> = HashMap::new();
    plugins.insert("rot13".to_string(), Arc::new(rot13_call));
    bench("2. plugin dispatch (is_builtin miss + HashMap + dyn Fn)", iters, || {
        let name = std::hint::black_box("rot13");
        if is_builtin(name) {
            unreachable!()
        }
        let f = plugins.get(name).cloned().unwrap();
        let r = f(&args, span);
        match r.unwrap() {
            Value::Str(s) => s.len() as u64,
            Value::Int(n) => n as u64,
        }
    });

    // 3. Real-builtin dispatch: is_builtin (hit, position 23/48) + match
    //    dispatch, doing the identical rot13 work inline.
    bench("3. real-builtin dispatch (is_builtin hit @23/48 + match)", iters, || {
        let name = std::hint::black_box("rand_gaussian");
        if !is_builtin(name) {
            unreachable!()
        }
        let r = eval_builtin_like(name, &args, span);
        match r.unwrap() {
            Value::Str(s) => s.len() as u64,
            Value::Int(n) => n as u64,
        }
    });

    // 4. Direct static call, no dispatch at all -- the floor.
    bench("4. direct static call (zero-dispatch floor)", iters, || {
        let r = rot13_direct(&args, span);
        match r.unwrap() {
            Value::Str(s) => s.len() as u64,
            Value::Int(n) => n as u64,
        }
    });

    // 5/6. Repeat the plugin-dispatch and floor benchmarks with a 64KB
    //      payload instead of 55 bytes -- isolates whether Kind A's
    //      Arc<str>-sharing dispatch cost scales with payload size (it
    //      shouldn't: passing the argument is an Arc clone, a fixed-cost
    //      refcount bump regardless of what the Arc points to).
    let big_payload = "The quick brown fox jumps over the lazy dog. ".repeat(1400); // ~64.4 KB
    println!("\n-- repeated with a {} KB payload --", big_payload.len() / 1024);
    let big_s = Value::Str(Arc::from(big_payload.as_str()));
    let big_args = [big_s];
    let big_iters: u64 = 2_000_000;

    bench("5. plugin dispatch, 64KB payload", big_iters, || {
        let name = std::hint::black_box("rot13");
        if is_builtin(name) {
            unreachable!()
        }
        let f = plugins.get(name).cloned().unwrap();
        let r = f(&big_args, span);
        match r.unwrap() {
            Value::Str(s) => s.len() as u64,
            Value::Int(n) => n as u64,
        }
    });

    bench("6. direct static call, 64KB payload (floor)", big_iters, || {
        let r = rot13_direct(&big_args, span);
        match r.unwrap() {
            Value::Str(s) => s.len() as u64,
            Value::Int(n) => n as u64,
        }
    });
}
