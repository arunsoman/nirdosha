//! Freestanding kernels for the six Vector/Matrix builtins with genuine
//! data-dependent control flow (`det`/`inv`/`solve`/`rank`/
//! `kf_update_state`/`kf_update_cov` — partial-pivot row selection is a
//! runtime `if v > max_val` inside a loop, not something whose trip count
//! or instruction shape is known at codegen time the way every other
//! Vector/Matrix builtin's is). Rather than hand-emit branchy LLVM IR for
//! that control flow, `codegen.rs` calls these — compiled once, ahead of
//! time, into a linked static library — exactly the way it already calls
//! `@printf`/`@abort`/the libm intrinsics. A native `call` to `-O2`-
//! compiled code costs exactly what inlined IR would; this is an
//! implementation-risk choice, not a performance one.
//!
//! **This crate is its own, separate Cargo workspace** (`Cargo.toml`'s
//! own doc comment), built by `crates/compiler/build.rs` via `cargo
//! rustc` into a `.a` at `nirdosha`'s own build time — not part of the
//! `nirdosha` lib's own crate graph, so it still cannot `use` anything
//! from `interpreter.rs` directly across that compilation-unit
//! boundary. Every Vector/Matrix algorithm here is therefore a
//! deliberate line-for-line mirror of the corresponding `&[f64]`-taking
//! function in `interpreter.rs` (`matrix_det`/`matrix_inv`/
//! `matrix_solve`/`matrix_rank`/`kf_update`/`mat_mul_f64`/
//! `mat_vec_mul_f64`/`mat_transpose_f64`/`vec_add_f64`/`vec_sub_f64`) —
//! if you change the algorithm in one place, change it in the other, and
//! `crates/compiler/tests/codegen.rs`'s interpreter-parity tests will catch a
//! divergence immediately if you forget.
//!
//! **Unlike before, this crate is a genuine Cargo package with real
//! dependencies** (`Cargo.toml`'s `[dependencies]` — `rust_decimal`,
//! used by this file's `nir_dec128_*` kernels): the old bare-`rustc`
//! invocation had no dependency resolution at all, which is exactly
//! why `Ty::Dec128` stayed interpreter-only long after `tcp`/`file`
//! were compiled — `rust_decimal` simply wasn't reachable from a
//! dependency-free `rustc` call. `cargo rustc`, not `cargo build`, is
//! what `build.rs` actually invokes: it both compiles this crate *and*
//! forwards `--print=native-static-libs` to the one real `rustc`
//! invocation that produces the final artifact, in a single command —
//! the same two facts the old bare-`rustc` call captured together, now
//! captured through Cargo's own dependency-resolved build instead of
//! around it.
#![allow(clippy::missing_safety_doc)]

const SINGULAR_EPSILON: f64 = 1e-10;

fn matrix_det(elems: &[f64], n: usize) -> f64 {
    let mut a: Vec<f64> = elems.to_vec();
    let mut det = 1.0;
    for col in 0..n {
        let mut pivot_row = col;
        let mut max_val = a[col * n + col].abs();
        for row in (col + 1)..n {
            let v = a[row * n + col].abs();
            if v > max_val {
                max_val = v;
                pivot_row = row;
            }
        }
        if max_val == 0.0 {
            return 0.0;
        }
        if pivot_row != col {
            for k in 0..n {
                a.swap(col * n + k, pivot_row * n + k);
            }
            det = -det;
        }
        det *= a[col * n + col];
        for row in (col + 1)..n {
            let factor = a[row * n + col] / a[col * n + col];
            for k in col..n {
                a[row * n + k] -= factor * a[col * n + k];
            }
        }
    }
    det
}

fn matrix_inv(elems: &[f64], n: usize) -> Option<Vec<f64>> {
    let mut a: Vec<f64> = elems.to_vec();
    let mut inv = vec![0.0; n * n];
    for i in 0..n {
        inv[i * n + i] = 1.0;
    }
    for col in 0..n {
        let mut pivot_row = col;
        let mut max_val = a[col * n + col].abs();
        for row in (col + 1)..n {
            let v = a[row * n + col].abs();
            if v > max_val {
                max_val = v;
                pivot_row = row;
            }
        }
        if max_val < SINGULAR_EPSILON {
            return None;
        }
        if pivot_row != col {
            for k in 0..n {
                a.swap(col * n + k, pivot_row * n + k);
                inv.swap(col * n + k, pivot_row * n + k);
            }
        }
        let pivot = a[col * n + col];
        for k in 0..n {
            a[col * n + k] /= pivot;
            inv[col * n + k] /= pivot;
        }
        for row in 0..n {
            if row == col {
                continue;
            }
            let factor = a[row * n + col];
            if factor != 0.0 {
                for k in 0..n {
                    a[row * n + k] -= factor * a[col * n + k];
                    inv[row * n + k] -= factor * inv[col * n + k];
                }
            }
        }
    }
    Some(inv)
}

fn matrix_solve(a_elems: &[f64], n: usize, b_elems: &[f64]) -> Option<Vec<f64>> {
    let mut a: Vec<f64> = a_elems.to_vec();
    let mut b: Vec<f64> = b_elems.to_vec();
    for col in 0..n {
        let mut pivot_row = col;
        let mut max_val = a[col * n + col].abs();
        for row in (col + 1)..n {
            let v = a[row * n + col].abs();
            if v > max_val {
                max_val = v;
                pivot_row = row;
            }
        }
        if max_val < SINGULAR_EPSILON {
            return None;
        }
        if pivot_row != col {
            for k in 0..n {
                a.swap(col * n + k, pivot_row * n + k);
            }
            b.swap(col, pivot_row);
        }
        for row in (col + 1)..n {
            let factor = a[row * n + col] / a[col * n + col];
            for k in col..n {
                a[row * n + k] -= factor * a[col * n + k];
            }
            b[row] -= factor * b[col];
        }
    }
    let mut x = vec![0.0; n];
    for row in (0..n).rev() {
        let mut sum = b[row];
        for k in (row + 1)..n {
            sum -= a[row * n + k] * x[k];
        }
        x[row] = sum / a[row * n + row];
    }
    Some(x)
}

fn matrix_rank(elems: &[f64], rows: usize, cols: usize) -> usize {
    let mut a: Vec<f64> = elems.to_vec();
    let mut rank = 0;
    let mut pivot_row = 0;
    for col in 0..cols {
        if pivot_row >= rows {
            break;
        }
        let mut best_row = pivot_row;
        let mut max_val = a[pivot_row * cols + col].abs();
        for row in (pivot_row + 1)..rows {
            let v = a[row * cols + col].abs();
            if v > max_val {
                max_val = v;
                best_row = row;
            }
        }
        if max_val < SINGULAR_EPSILON {
            continue;
        }
        if best_row != pivot_row {
            for k in 0..cols {
                a.swap(pivot_row * cols + k, best_row * cols + k);
            }
        }
        for row in (pivot_row + 1)..rows {
            let factor = a[row * cols + col] / a[pivot_row * cols + col];
            for k in col..cols {
                a[row * cols + k] -= factor * a[pivot_row * cols + k];
            }
        }
        pivot_row += 1;
        rank += 1;
    }
    rank
}

fn mat_mul_f64(a: &[f64], ar: usize, ac: usize, b: &[f64], bc: usize) -> Vec<f64> {
    let mut out = vec![0.0; ar * bc];
    for i in 0..ar {
        for j in 0..bc {
            out[i * bc + j] = (0..ac).map(|k| a[i * ac + k] * b[k * bc + j]).sum();
        }
    }
    out
}

fn mat_vec_mul_f64(a: &[f64], ar: usize, ac: usize, v: &[f64]) -> Vec<f64> {
    (0..ar).map(|i| (0..ac).map(|k| a[i * ac + k] * v[k]).sum()).collect()
}

fn mat_transpose_f64(a: &[f64], r: usize, c: usize) -> Vec<f64> {
    let mut out = vec![0.0; r * c];
    for i in 0..r {
        for j in 0..c {
            out[j * r + i] = a[i * c + j];
        }
    }
    out
}

fn vec_add_f64(a: &[f64], b: &[f64]) -> Vec<f64> {
    a.iter().zip(b).map(|(x, y)| x + y).collect()
}

fn vec_sub_f64(a: &[f64], b: &[f64]) -> Vec<f64> {
    a.iter().zip(b).map(|(x, y)| x - y).collect()
}

fn kf_update(
    x: &[f64],
    p: &[f64],
    z: &[f64],
    h: &[f64],
    r: &[f64],
    n: usize,
    m: usize,
) -> Option<(Vec<f64>, Vec<f64>)> {
    let hx = mat_vec_mul_f64(h, m, n, x);
    let y = vec_sub_f64(z, &hx);
    let ht = mat_transpose_f64(h, m, n);
    let hp = mat_mul_f64(h, m, n, p, n);
    let hpht = mat_mul_f64(&hp, m, n, &ht, m);
    let s = vec_add_f64(&hpht, r);
    let s_inv = matrix_inv(&s, m)?;
    let pht = mat_mul_f64(p, n, n, &ht, m);
    let k = mat_mul_f64(&pht, n, m, &s_inv, m);
    let ky = mat_vec_mul_f64(&k, n, m, &y);
    let x_new = vec_add_f64(x, &ky);
    let kh = mat_mul_f64(&k, n, m, h, n);
    let mut i_minus_kh = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..n {
            i_minus_kh[i * n + j] = if i == j { 1.0 } else { 0.0 } - kh[i * n + j];
        }
    }
    let p_new = mat_mul_f64(&i_minus_kh, n, n, p, n);
    Some((x_new, p_new))
}

// ---- sha256_hex kernel ----------------------------------------------------
//
// A from-scratch FIPS 180-4 SHA-256 implementation, not a binding to the
// `sha2` crate `interpreter.rs`'s own `sha256_hex`/`sha256_hex_chain`
// use — this file is compiled as an isolated `rustc --crate-type
// staticlib` invocation with no `--extern` flags (`build.rs`'s doc
// comment), so it has no access to Cargo dependencies at all, only
// `std`. Verified bit-for-bit against `interpreter.rs`'s `sha2`-backed
// output for the empty string, ASCII text, and the exact two-part
// chained form `sha256_hex_chain` uses (`crates/compiler/tests/sha256_hex.rs`),
// not just against the standard's own published test vectors.

const SHA256_H0: [u32; 8] =
    [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
    0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
    0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
    0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2,
];

/// One 64-byte block's worth of compression, updating `state` in place —
/// the algorithm's actual core (message-schedule expansion, 64 mixing
/// rounds), everything else in this section is padding/framing around
/// this.
fn sha256_compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let temp1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(SHA256_K[i]).wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

/// Hashes `a` followed by `b` as one continuous message (`b` empty is
/// the 1-arg `sha256_hex(s)` case; `b` non-empty is the 2-arg
/// `sha256_hex(prev_hash, payload)` chained form) — streaming both
/// buffers through the same padding/block state the way multiple
/// `Sha256::update` calls on one hasher already do in
/// `interpreter.rs`, since hashing "a then b" one block at a time is
/// mathematically identical to hashing the concatenation `a ++ b` in
/// one pass; there's no need to actually concatenate them into a new
/// buffer first (which `str`'s lack of a concatenation operator
/// wouldn't let calling Nirdosha code do anyway — this streaming
/// approach is what makes that a non-issue at the kernel level too).
fn sha256(a: &[u8], b: &[u8]) -> [u8; 32] {
    let mut state = SHA256_H0;
    let total_len = (a.len() + b.len()) as u64;

    let mut block = [0u8; 64];
    let mut filled = 0usize;
    for &byte in a.iter().chain(b.iter()) {
        block[filled] = byte;
        filled += 1;
        if filled == 64 {
            sha256_compress(&mut state, &block);
            filled = 0;
        }
    }

    // Padding: a single `1` bit (0x80, since messages here are always a
    // whole number of bytes), then zero bits, then the original message
    // length in bits as a big-endian 64-bit integer -- padded so the
    // total is a multiple of 64 bytes, with the length always the final
    // 8 bytes of the final block, same as every other SHA-256
    // implementation's framing (FIPS 180-4 §5.1.1).
    block[filled] = 0x80;
    filled += 1;
    if filled > 56 {
        for b in &mut block[filled..64] {
            *b = 0;
        }
        sha256_compress(&mut state, &block);
        filled = 0;
    }
    for b in &mut block[filled..56] {
        *b = 0;
    }
    let bit_len = total_len.wrapping_mul(8);
    block[56..64].copy_from_slice(&bit_len.to_be_bytes());
    sha256_compress(&mut state, &block);

    let mut out = [0u8; 32];
    for (i, word) in state.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Lowercase hex encoding, matching `interpreter.rs`'s own
/// `format!("{b:02x}")` per byte exactly.
fn hex_encode(bytes: &[u8], out: &mut [u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (i, &b) in bytes.iter().enumerate() {
        out[2 * i] = HEX[(b >> 4) as usize];
        out[2 * i + 1] = HEX[(b & 0x0f) as usize];
    }
}

/// The interpreter's own `constant_time_eq`, line-for-line: length
/// mismatch is a real, immediate difference (a real, accepted timing
/// leak of *length* — the property this function actually protects is
/// "don't leak *which byte* differs"), otherwise XOR-accumulate every
/// byte pair with no early exit.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---- extern "C" boundary -----------------------------------------------
//
// Every pointer here is trusted, not validated: typeck.rs already proved
// every call site passes correctly-shaped, correctly-sized buffers before
// codegen.rs ever emits the `call` instruction that reaches these — the
// same "the checker is the real gate" convention interpreter.rs's own
// `unreachable!()`s already follow for builtin dispatch.

/// Determinant of an `n x n` matrix. Never fails — `0.0` for singular is
/// a real, legitimate answer for `det` specifically.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_det(a: *const f64, n: i64) -> f64 {
    let n = n as usize;
    let a = unsafe { std::slice::from_raw_parts(a, n * n) };
    matrix_det(a, n)
}

/// Inverse of an `n x n` matrix into `out` (also `n x n`). Returns `1` on
/// success, `0` if singular (caller traps on `0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_inv(a: *const f64, n: i64, out: *mut f64) -> i32 {
    let n = n as usize;
    let a = unsafe { std::slice::from_raw_parts(a, n * n) };
    match matrix_inv(a, n) {
        Some(v) => {
            let out = unsafe { std::slice::from_raw_parts_mut(out, n * n) };
            out.copy_from_slice(&v);
            1
        }
        None => 0,
    }
}

/// Solves `A x = b` for an `n x n` `A` and length-`n` `b`, into `out`
/// (length `n`). Returns `1` on success, `0` if `A` is singular.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_solve(a: *const f64, n: i64, b: *const f64, out: *mut f64) -> i32 {
    let n = n as usize;
    let a = unsafe { std::slice::from_raw_parts(a, n * n) };
    let b = unsafe { std::slice::from_raw_parts(b, n) };
    match matrix_solve(a, n, b) {
        Some(x) => {
            let out = unsafe { std::slice::from_raw_parts_mut(out, n) };
            out.copy_from_slice(&x);
            1
        }
        None => 0,
    }
}

/// Rank of a `rows x cols` matrix. Never fails.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_rank(a: *const f64, rows: i64, cols: i64) -> i64 {
    let rows = rows as usize;
    let cols = cols as usize;
    let a = unsafe { std::slice::from_raw_parts(a, rows * cols) };
    matrix_rank(a, rows, cols) as i64
}

/// Linear Kalman filter update step's state output, into `out` (length
/// `n`). `x`/`p`/`z`/`h`/`r` are the state vector (len `n`), state
/// covariance (`n x n`), measurement (len `m`), measurement matrix
/// (`m x n`), and measurement-noise covariance (`m x m`). Returns `1` on
/// success, `0` if the innovation covariance is singular.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_kf_update_state(
    x: *const f64,
    p: *const f64,
    z: *const f64,
    h: *const f64,
    r: *const f64,
    n: i64,
    m: i64,
    out: *mut f64,
) -> i32 {
    let n = n as usize;
    let m = m as usize;
    let x = unsafe { std::slice::from_raw_parts(x, n) };
    let p = unsafe { std::slice::from_raw_parts(p, n * n) };
    let z = unsafe { std::slice::from_raw_parts(z, m) };
    let h = unsafe { std::slice::from_raw_parts(h, m * n) };
    let r = unsafe { std::slice::from_raw_parts(r, m * m) };
    match kf_update(x, p, z, h, r, n, m) {
        Some((x_new, _)) => {
            let out = unsafe { std::slice::from_raw_parts_mut(out, n) };
            out.copy_from_slice(&x_new);
            1
        }
        None => 0,
    }
}

/// Same update step's covariance output, into `out` (`n x n`). Same
/// shapes/return convention as `nir_kf_update_state`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_kf_update_cov(
    x: *const f64,
    p: *const f64,
    z: *const f64,
    h: *const f64,
    r: *const f64,
    n: i64,
    m: i64,
    out: *mut f64,
) -> i32 {
    let n = n as usize;
    let m = m as usize;
    let x = unsafe { std::slice::from_raw_parts(x, n) };
    let p = unsafe { std::slice::from_raw_parts(p, n * n) };
    let z = unsafe { std::slice::from_raw_parts(z, m) };
    let h = unsafe { std::slice::from_raw_parts(h, m * n) };
    let r = unsafe { std::slice::from_raw_parts(r, m * m) };
    match kf_update(x, p, z, h, r, n, m) {
        Some((_, p_new)) => {
            let out = unsafe { std::slice::from_raw_parts_mut(out, n * n) };
            out.copy_from_slice(&p_new);
            1
        }
        None => 0,
    }
}

/// `str`'s `==`/`!=` — length check, then a byte-for-byte compare.
/// Returns `1` if equal, `0` otherwise. `codegen.rs`'s `str_eq` is the
/// only caller — it already only ever passes buffers `{ptr, i64}`-typed
/// `str` values actually own, matching every other kernel's "the checker
/// is the real gate" trust convention (this file's module doc).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_str_eq(a: *const u8, a_len: i64, b: *const u8, b_len: i64) -> i32 {
    if a_len != b_len {
        return 0;
    }
    let n = a_len as usize;
    let a = unsafe { std::slice::from_raw_parts(a, n) };
    let b = unsafe { std::slice::from_raw_parts(b, n) };
    (a == b) as i32
}

// ---- tcp/tcp_listener kernels --------------------------------------------
//
// A `tcp`/`tcp_listener` handle is a raw OS socket handle, not a Rust
// `TcpStream`/`TcpListener` kept alive across calls the way
// `interpreter.rs`'s `Value::Tcp`'s `Arc<Mutex<Option<..>>>` does — the
// kernel already tracks everything a "handle" needs, so `codegen.rs`
// lowers `Ty::Tcp`/`Ty::TcpListener` straight to `i64`. Every kernel below
// reconstructs a `std`-level view of that handle for the duration of one
// call via the platform's own `from_raw_*`, wrapped in `ManuallyDrop`
// wherever the handle must stay open afterward (only `nir_tcp_stop`
// actually wants the real `Drop`/close to run). This mirrors
// `interpreter.rs`'s exact error/port-validation behavior
// (`Expr::Connect`/`Expr::Listen`/`read_tcp`/`write_tcp`) — see each fn's
// doc comment for the specific line it matches.
//
// Unix represents a socket as a `RawFd` (`i32`); Windows represents it as
// a `RawSocket` (`u64`), a structurally different type with a differently
// named conversion trait (`IntoRawSocket`/`FromRawSocket` vs.
// `IntoRawFd`/`FromRawFd`). The four tiny `handle_*` helpers below are the
// only platform-conditional surface — every kernel fn's own body is
// platform-agnostic, calling only these. **The Windows path is untested**
// (no Windows machine available to this project — see README.md's
// "Honest scope"): it's a direct, believed-correct port of the Unix path
// using the equivalent stdlib API, not verified end-to-end against a real
// Windows TCP round-trip. Report a bug if it doesn't work.

use std::io::{Read, Write};
use std::mem::ManuallyDrop;
use std::net::{TcpListener, TcpStream};

#[cfg(unix)]
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd, RawFd};
#[cfg(windows)]
use std::os::windows::io::{FromRawSocket, IntoRawSocket, OwnedSocket, RawSocket};

#[cfg(unix)]
fn handle_of_stream(s: TcpStream) -> i64 {
    s.into_raw_fd() as i64
}
#[cfg(windows)]
fn handle_of_stream(s: TcpStream) -> i64 {
    s.into_raw_socket() as i64
}

#[cfg(unix)]
fn handle_of_listener(l: TcpListener) -> i64 {
    l.into_raw_fd() as i64
}
#[cfg(windows)]
fn handle_of_listener(l: TcpListener) -> i64 {
    l.into_raw_socket() as i64
}

#[cfg(unix)]
unsafe fn stream_from_handle(h: i64) -> TcpStream {
    unsafe { TcpStream::from_raw_fd(h as RawFd) }
}
#[cfg(windows)]
unsafe fn stream_from_handle(h: i64) -> TcpStream {
    unsafe { TcpStream::from_raw_socket(h as RawSocket) }
}

#[cfg(unix)]
unsafe fn listener_from_handle(h: i64) -> TcpListener {
    unsafe { TcpListener::from_raw_fd(h as RawFd) }
}
#[cfg(windows)]
unsafe fn listener_from_handle(h: i64) -> TcpListener {
    unsafe { TcpListener::from_raw_socket(h as RawSocket) }
}

/// Connects to `host:port` (`host` a `{ptr, len}` UTF-8 buffer). Returns
/// the new connection's handle on success, `-1` on failure — mirrors
/// `interpreter.rs`'s `Expr::Connect`: `u16::try_from(port)` (an
/// out-of-range port is a failure, not a silent truncation) then
/// `TcpStream::connect`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_tcp_connect(host_ptr: *const u8, host_len: i64, port: i64) -> i64 {
    let host = unsafe { std::slice::from_raw_parts(host_ptr, host_len as usize) };
    let Ok(host) = std::str::from_utf8(host) else { return -1 };
    let Ok(port) = u16::try_from(port) else { return -1 };
    match TcpStream::connect((host, port)) {
        Ok(stream) => handle_of_stream(stream),
        Err(_) => -1,
    }
}

/// Binds `0.0.0.0:port` (all interfaces, matching `interpreter.rs`'s
/// `Expr::Listen` — not just loopback). Returns the listener's handle on
/// success, `-1` on failure (including an out-of-range port).
#[unsafe(no_mangle)]
pub extern "C" fn nir_tcp_listen(port: i64) -> i64 {
    let Ok(port) = u16::try_from(port) else { return -1 };
    match TcpListener::bind(("0.0.0.0", port)) {
        Ok(listener) => handle_of_listener(listener),
        Err(_) => -1,
    }
}

/// Blocks for the next connection on `listener_handle`. Returns the
/// accepted connection's own handle on success, `-1` on failure.
/// `listener_handle` itself is left open and reusable (`accept` doesn't
/// consume the listener, `ownership.rs`'s `touch_expr(listener, false)`)
/// — `ManuallyDrop` stops the temporary `TcpListener` view constructed
/// here from closing it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_tcp_accept(listener_handle: i64) -> i64 {
    let listener = ManuallyDrop::new(unsafe { listener_from_handle(listener_handle) });
    match listener.accept() {
        Ok((stream, _addr)) => handle_of_stream(stream),
        Err(_) => -1,
    }
}

/// Sends `buf` in full over `handle` — `write_all`, matching
/// `interpreter.rs`'s `write_tcp` exactly (it loops internally until
/// every byte is written, not a single partial-write return). Returns
/// `buf_len` on success, `-1` on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_tcp_send(handle: i64, buf_ptr: *const u8, buf_len: i64) -> i64 {
    let mut stream = ManuallyDrop::new(unsafe { stream_from_handle(handle) });
    let buf = unsafe { std::slice::from_raw_parts(buf_ptr, buf_len as usize) };
    match stream.write_all(buf) {
        Ok(()) => buf_len,
        Err(_) => -1,
    }
}

/// One read syscall into `buf_cap` bytes of caller-provided `buf_ptr` —
/// matches `interpreter.rs`'s `read_tcp`: one chunk, not a loop until a
/// message boundary. Returns bytes read, or `-1` on error. Note: unlike a
/// typical Unix `read`, a `0` return (peer closed) is *not* distinguished
/// from a short read here — `codegen.rs`'s caller (`guard_recv_ok`) traps
/// on `<= 0` the same way `read_tcp` treats `n == 0` as an error, not a
/// valid empty read.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_tcp_recv(handle: i64, buf_ptr: *mut u8, buf_cap: i64) -> i64 {
    let mut stream = ManuallyDrop::new(unsafe { stream_from_handle(handle) });
    let buf = unsafe { std::slice::from_raw_parts_mut(buf_ptr, buf_cap as usize) };
    match stream.read(buf) {
        Ok(n) => n as i64,
        Err(_) => -1,
    }
}

/// Closes `handle` — serves both `tcp` and `tcp_listener` uniformly (both
/// are plain sockets at the OS level, so one raw-handle close path is
/// correct for either). `ownership.rs`'s affine-typing already proves
/// this runs at most once per handle in a well-typed program (this
/// file's module doc's "the checker is the real gate" convention) —
/// reconstructing an *owned* handle (not `ManuallyDrop`) and letting it
/// drop is what actually closes the socket.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_tcp_stop(handle: i64) -> i32 {
    #[cfg(unix)]
    drop(unsafe { OwnedFd::from_raw_fd(handle as RawFd) });
    #[cfg(windows)]
    drop(unsafe { OwnedSocket::from_raw_socket(handle as RawSocket) });
    0
}

// ---- file kernels ---------------------------------------------------------
//
// `open`/`file` (docs/PROTOLANG_PORT.md's "Locked design 2") reuses `send`/
// `recv`/`stop` verbatim, the same way `tcp` itself reuses them from `chan`
// (`examples/file_io.nir`'s own doc comment) -- so this section mirrors the
// `tcp` kernels above almost exactly, differing only where a file's real
// semantics genuinely differ from a socket's: `nir_file_read`'s `0` return
// is valid EOF (`interpreter.rs::read_file`'s own doc comment: "a file
// simply running out of bytes to read is the normal, expected way a file
// ends"), never a trap the way `nir_tcp_recv`'s `0` (peer closed) is —
// `codegen.rs` dispatches to `guard_io_ok` (traps only on negative) for
// `Ty::File`'s `recv`, not `guard_recv_ok` (traps on `<= 0`), to match.
//
// On Unix a file descriptor and a socket descriptor are the same `RawFd`
// type, so `handle_of_stream`/`stream_from_handle`'s *traits*
// (`IntoRawFd`/`FromRawFd`, already imported above) apply to
// `std::fs::File` unchanged -- only Windows genuinely needs its own
// conversion, since a Win32 file `HANDLE` and a `SOCKET` are different,
// differently-APIed types (`IntoRawHandle`/`FromRawHandle`, not
// `IntoRawSocket`/`FromRawSocket`). Same "believed-correct, untested on
// real Windows hardware" disclosure as every other Windows-conditional
// kernel in this file.
#[cfg(unix)]
fn handle_of_file(f: std::fs::File) -> i64 {
    f.into_raw_fd() as i64
}
#[cfg(windows)]
fn handle_of_file(f: std::fs::File) -> i64 {
    use std::os::windows::io::IntoRawHandle;
    f.into_raw_handle() as i64
}

#[cfg(unix)]
unsafe fn file_from_handle(h: i64) -> std::fs::File {
    unsafe { std::fs::File::from_raw_fd(h as RawFd) }
}
#[cfg(windows)]
unsafe fn file_from_handle(h: i64) -> std::fs::File {
    use std::os::windows::io::{FromRawHandle, RawHandle};
    unsafe { std::fs::File::from_raw_handle(h as RawHandle) }
}

/// `open(path, mode)` — matches `interpreter.rs`'s `Expr::Open` exactly:
/// `"r"` opens for reading, `"w"` creates/truncates for writing, `"a"`
/// creates/appends; any other mode string is `-1`, the same "invalid mode"
/// failure a real I/O error already collapses into (`codegen.rs`'s
/// `guard_io_ok` can't distinguish *why* `open` failed, only that it did —
/// same limitation `nir_tcp_connect`'s own bad-port/bad-host cases already
/// have). `path`/`mode` are `{ptr, len}` UTF-8 buffers, same convention
/// `nir_tcp_connect`'s `host` argument already uses.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_file_open(path_ptr: *const u8, path_len: i64, mode_ptr: *const u8, mode_len: i64) -> i64 {
    let path = unsafe { std::slice::from_raw_parts(path_ptr, path_len as usize) };
    let Ok(path) = std::str::from_utf8(path) else { return -1 };
    let mode = unsafe { std::slice::from_raw_parts(mode_ptr, mode_len as usize) };
    let Ok(mode) = std::str::from_utf8(mode) else { return -1 };
    let opened = match mode {
        "r" => std::fs::File::open(path),
        "w" => std::fs::File::create(path),
        "a" => std::fs::OpenOptions::new().append(true).create(true).open(path),
        _ => return -1,
    };
    match opened {
        Ok(file) => handle_of_file(file),
        Err(_) => -1,
    }
}

/// `send(file, s)` — `write_all`, matching `interpreter.rs::write_file`
/// exactly (loops internally until every byte is written). Returns
/// `buf_len` on success, `-1` on failure — same convention as
/// `nir_tcp_send`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_file_write(handle: i64, buf_ptr: *const u8, buf_len: i64) -> i64 {
    let mut file = ManuallyDrop::new(unsafe { file_from_handle(handle) });
    let buf = unsafe { std::slice::from_raw_parts(buf_ptr, buf_len as usize) };
    match file.write_all(buf) {
        Ok(()) => buf_len,
        Err(_) => -1,
    }
}

/// `recv(file)` — one read syscall into a fixed 64KiB buffer, matching
/// `interpreter.rs::read_file` exactly (same buffer size, same "one
/// chunk" scope). Returns bytes read (`0` is valid EOF, not an error —
/// see this section's own module doc), or `-1` on a real I/O failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_file_read(handle: i64, buf_ptr: *mut u8, buf_cap: i64) -> i64 {
    let mut file = ManuallyDrop::new(unsafe { file_from_handle(handle) });
    let buf = unsafe { std::slice::from_raw_parts_mut(buf_ptr, buf_cap as usize) };
    match file.read(buf) {
        Ok(n) => n as i64,
        Err(_) => -1,
    }
}

/// Closes `handle` — `ownership.rs`'s affine typing already proves this
/// runs at most once per handle in a well-typed program, same "the
/// checker is the real gate" convention `nir_tcp_stop` already documents.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_file_stop(handle: i64) -> i32 {
    #[cfg(unix)]
    drop(unsafe { OwnedFd::from_raw_fd(handle as RawFd) });
    #[cfg(windows)]
    {
        use std::os::windows::io::{FromRawHandle, OwnedHandle, RawHandle};
        drop(unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) });
    }
    0
}

// ---- sha256_hex / constant_time_str_eq extern boundary --------------------
//
// Backs the `sha256_hex(s)`/`sha256_hex(prev_hash, payload)` and
// `constant_time_str_eq(a, b)` builtins. `codegen.rs`'s caller passes
// `b_len: 0` for the 1-arg `sha256_hex` form -- `b_len == 0` never
// dereferences `b_ptr` (`&[]` instead of `slice::from_raw_parts` on it),
// so whatever codegen happens to pass as `b_ptr` in that case (a null
// pointer constant is fine) never has to be a real, valid pointer.
//
// **`sha256_hex`'s output buffer is heap-allocated via `nir_alloc` and
// never freed.** `Ty::Str` isn't affine (`Ty::is_affine`'s doc comment)
// -- nothing in `ownership.rs`'s `FreeMap` tracks a `str` binding's last
// use the way it does for `box`, so there's no scope-closing point for
// codegen to hook a matching `nir_free` onto even if this function
// wanted one. A real, disclosed, permanent leak (one 64-byte allocation
// per `sha256_hex` call), not a silent one -- the same "state it here
// rather than leave it implicit" discipline `box`'s own allocator used
// before its own free-hookup phase existed, except here there is no
// later phase that closes this one: making `str` affine to fix it would
// be a real, unrelated language change (every existing `str` use --
// literals, params, returns, `print` -- currently assumes freely-
// copyable, unowned `str` values), not a small follow-up.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_sha256_hex(a_ptr: *const u8, a_len: i64, b_ptr: *const u8, b_len: i64, out: *mut u8) {
    let a = unsafe { std::slice::from_raw_parts(a_ptr, a_len as usize) };
    let b: &[u8] = if b_len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(b_ptr, b_len as usize) } };
    let digest = sha256(a, b);
    let out = unsafe { std::slice::from_raw_parts_mut(out, 64) };
    hex_encode(&digest, out);
}

/// `1` if the two buffers are equal, `0` otherwise -- see
/// `constant_time_eq`'s own doc comment for exactly what "constant-time"
/// does and doesn't mean here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_constant_time_str_eq(a_ptr: *const u8, a_len: i64, b_ptr: *const u8, b_len: i64) -> i32 {
    let a = unsafe { std::slice::from_raw_parts(a_ptr, a_len as usize) };
    let b = unsafe { std::slice::from_raw_parts(b_ptr, b_len as usize) };
    constant_time_eq(a, b) as i32
}

// ---- rand_seed/rand_f64/rand_gaussian kernel -------------------------------
//
// `interpreter.rs`'s own `RngState`, line-for-line (SplitMix64 for the
// underlying stream, Box-Muller for `rand_gaussian`) -- deliberately
// re-derived here rather than shared, for the same reason every other
// kernel in this file is: no access to `interpreter.rs` across the
// isolated-staticlib compilation boundary (`build.rs`'s doc comment).
// Verified bit-for-bit against the interpreter's actual output for a
// fixed seed, not just against a re-reading of the same algorithm
// description (`crates/compiler/tests/codegen.rs`'s `rand_*` tests).
//
// **Process-wide state, not per-"instance."** The interpreter's own
// `RngState` is deliberately *not* a global (its doc comment: "carried
// in the interpreter environment, not a global," so independent/
// concurrent interpreter runs never share a stream) -- but a compiled
// Nirdosha binary has exactly one logical owner for this state per
// process: `thread`/`spawn` aren't compiled yet (`docs/LANGUAGE.md` §10), so
// there's only ever one thread to own it, making a process-wide static
// the honest equivalent of "this process's one `Interpreter` instance,"
// not a shortcut around the interpreter's own stated reasoning.
static RAND_SEEDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RAND_STATE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn splitmix64_next(state: &std::sync::atomic::AtomicU64) -> u64 {
    use std::sync::atomic::Ordering;
    let mut s = state.load(Ordering::Relaxed).wrapping_add(0x9E3779B97F4A7C15);
    state.store(s, Ordering::Relaxed);
    s = (s ^ (s >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    s = (s ^ (s >> 27)).wrapping_mul(0x94D049BB133111EB);
    s ^ (s >> 31)
}

fn rand_next_f64() -> f64 {
    (splitmix64_next(&RAND_STATE) >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
}

/// Seeds the process-wide stream. `codegen.rs` compiles `rand_seed(n)`
/// for any integer type `n` (`typeck.rs`'s `t.is_integer()` check) --
/// every one of them arrives here already widened to `i64` (this
/// backend's own internal convention, `widen_to_i64`'s doc comment), so
/// this only ever needs to accept one width.
#[unsafe(no_mangle)]
pub extern "C" fn nir_rand_seed(seed: i64) {
    use std::sync::atomic::Ordering;
    RAND_STATE.store(seed as u64, Ordering::Relaxed);
    RAND_SEEDED.store(true, Ordering::Relaxed);
}

/// Aborts if called before `nir_rand_seed` -- the same "the checker
/// can't catch this statically, so trap at runtime rather than return a
/// silently-wrong value" treatment every other unrecoverable runtime
/// condition in this backend gets (div-by-zero, integer overflow,
/// `nir_alloc` failure), enforced in Rust here rather than threading an
/// extra codegen-side branch-and-trap sequence through every call site:
/// `interpreter.rs`'s own `ErrorKind::RngNotSeeded` is a real, catchable
/// `RuntimeError` there because the interpreter *can* return one; a
/// compiled binary's equivalent of "stop, this precondition was
/// violated" is `abort()`, same as `nir_alloc`'s allocation-failure path
/// already uses.
#[unsafe(no_mangle)]
pub extern "C" fn nir_rand_f64() -> f64 {
    if !RAND_SEEDED.load(std::sync::atomic::Ordering::Relaxed) {
        std::process::abort();
    }
    rand_next_f64()
}

/// Same not-yet-seeded guard as `nir_rand_f64`, then the interpreter's
/// exact Box-Muller transform (`next_f64()` clamped away from `0.0`
/// before `.ln()`, same sharp-edge note as `RngState::next_gaussian`'s
/// own doc comment).
#[unsafe(no_mangle)]
pub extern "C" fn nir_rand_gaussian(mean: f64, stddev: f64) -> f64 {
    if !RAND_SEEDED.load(std::sync::atomic::Ordering::Relaxed) {
        std::process::abort();
    }
    let u1 = rand_next_f64().max(f64::MIN_POSITIVE);
    let u2 = rand_next_f64();
    let z0 = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
    mean + stddev * z0
}

// ---- box's heap allocator -------------------------------------------------
//
// `nir_free(ptr)` takes only a pointer, no size — `codegen.rs`'s
// `ty_byte_size` computes a box's allocation size at the `box e` call
// site, but by the time (a later phase's) `nir_free` runs at last-use, the
// *static* type is still known there too in principle, but threading it
// through would mean every free call site needs to redo that computation
// and match it exactly against what the alloc site used. Simpler and more
// robust: `nir_alloc` writes its own size into a small header immediately
// before the returned pointer, and `nir_free` reads it back — the
// allocator is the only thing that ever needs to agree with itself.
// `size + HEADER_BYTES` is over-allocated by exactly enough to fit that
// header; `align(16)` is generous enough for every `Ty` this backend can
// box (nothing here needs more than 8-byte alignment, `f64`/`ptr`
// included, but 16 costs nothing and leaves headroom).
const NIR_ALLOC_HEADER_BYTES: usize = 16;
const NIR_ALLOC_ALIGN: usize = 16;

/// Heap-allocates `size` bytes for `box e`, returning a pointer to the
/// usable region (the header lives just before it, invisible to the
/// caller). Aborts on allocation failure — `panic=abort` (`build.rs`)
/// turns `handle_alloc_error`'s abort into the same "the process just
/// stops" behavior every other unrecoverable condition in this backend
/// already has (the div-by-zero/overflow/bounds traps), not a new failure
/// mode.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_alloc(size: i64) -> *mut u8 {
    let size = size as usize;
    let total = size + NIR_ALLOC_HEADER_BYTES;
    let layout = std::alloc::Layout::from_size_align(total, NIR_ALLOC_ALIGN)
        .expect("box allocation size is always a small, codegen-computed constant");
    let base = unsafe { std::alloc::alloc(layout) };
    if base.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    unsafe {
        (base as *mut usize).write(size);
        base.add(NIR_ALLOC_HEADER_BYTES)
    }
}

/// Frees a pointer previously returned by `nir_alloc`. Called for real
/// by every compiled program that boxes a value: `codegen.rs`'s
/// `emit_frees_for_names`/`emit_box_free`, driven by `ownership.rs`'s
/// `FreeMap` (which binding's last use is where), emit this at every
/// scope-closing point a boxed binding's last use falls in — confirmed
/// in generated IR (`nirdosha emit-llvm`) for a simple `let`-bound box,
/// not just assumed from this comment.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_free(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let base = ptr.sub(NIR_ALLOC_HEADER_BYTES);
        let size = (base as *const usize).read();
        let layout = std::alloc::Layout::from_size_align(size + NIR_ALLOC_HEADER_BYTES, NIR_ALLOC_ALIGN)
            .expect("matches the layout nir_alloc used to allocate this same pointer");
        std::alloc::dealloc(base, layout);
    }
}

// ---- dec128 kernels ---------------------------------------------------
//
// The actual point of this crate's split into a real Cargo package
// (`Cargo.toml`'s own doc comment): `rust_decimal::Decimal` is a real
// dependency here, reachable for the first time. `Ty::Dec128` stays a
// plain two-word *value* (not an aggregate — `ast::Ty::is_aggregate()`
// deliberately excludes it, since `transact_log.rs`'s slot-eligibility
// check already depends on `dec128` being a plain scalar it can
// serialize directly), so every kernel here takes/returns `Dec128Bits`
// by value — a `#[repr(C)]` two-`u64` struct, the same "return in two
// registers" ABI shape `codegen.rs` already relies on for `str`'s own
// `{ptr, i64}` value (LLVM `{i64, i64}` on the caller side; see
// `codegen.rs`'s own `Ty::Dec128` doc comment for the exact LLVM type
// string this pairs with).
//
// `Decimal::serialize()`/`deserialize()` (stable, public API, not an
// internal-layout assumption -- `Cargo.toml`'s own doc comment) is the
// actual boundary every kernel crosses: `Dec128Bits`'s two `u64`s are
// exactly that 16-byte buffer, split at the midpoint, little-endian
// (matching `serialize()`'s own byte order).
use rust_decimal::Decimal;

#[repr(C)]
pub struct Dec128Bits {
    pub lo: u64,
    pub hi: u64,
}

fn bits_to_decimal(bits: Dec128Bits) -> Decimal {
    let mut bytes = [0u8; 16];
    bytes[0..8].copy_from_slice(&bits.lo.to_le_bytes());
    bytes[8..16].copy_from_slice(&bits.hi.to_le_bytes());
    Decimal::deserialize(bytes)
}

fn decimal_to_bits(d: Decimal) -> Dec128Bits {
    let bytes = d.serialize();
    let lo = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
    let hi = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    Dec128Bits { lo, hi }
}

/// `dec_from_i64(v, scale)` — matches `interpreter.rs`'s own
/// `dec_from_i64` arm exactly, `Decimal::new`'s own call included:
/// `Decimal::new` panics past `scale > 28` (the representation's own
/// limit — `docs/LANGUAGE.md` §5), which is exactly the right behavior
/// here too, unchanged, since this crate's own `panic = "abort"`
/// profile (`Cargo.toml`) turns that panic into a clean process abort
/// at the FFI boundary rather than an unwind — the same "checker can't
/// see this coming, so trap at runtime" treatment every other Tier-2
/// guard in this codebase already gets.
#[unsafe(no_mangle)]
pub extern "C" fn nir_dec128_from_i64(value: i64, scale: u32) -> Dec128Bits {
    decimal_to_bits(Decimal::new(value, scale))
}

/// `dec_to_str(d)` — writes `d`'s canonical `Display` string into the
/// caller-provided `out_ptr`/`out_cap` buffer, same "fixed buffer,
/// return actual length" convention `nir_tcp_recv`/`nir_file_read`
/// already use for a variable-length result. A `dec128`'s longest
/// possible representation (a sign, up to 29 decimal digits for the
/// 96-bit mantissa, one decimal point) is well under 64 bytes —
/// `codegen.rs` allocates exactly that; `-1` here (never expected in
/// practice, kept as a real, checked failure mode rather than an
/// assumed-safe `unwrap`) means the caller's buffer was too small.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_dec128_to_str(value: Dec128Bits, out_ptr: *mut u8, out_cap: i64) -> i64 {
    let d = bits_to_decimal(value);
    let s = d.to_string();
    let bytes = s.as_bytes();
    if bytes.len() as i64 > out_cap {
        return -1;
    }
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_ptr, bytes.len()) };
    bytes.len() as i64
}

/// `dec_from_str(s)` — matches `interpreter.rs`'s `Decimal::from_str`
/// call exactly. Returns `Dec128Bits` by value plus an `i32` success
/// flag (`1` ok, `0` malformed) via `ok_ptr`, the same "packed result,
/// no `Result` type at this ABI layer" shape `nir_inv`/`nir_solve`
/// already use for their own fallible linear-algebra kernels.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_dec128_from_str(s_ptr: *const u8, s_len: i64, ok_ptr: *mut i32) -> Dec128Bits {
    use std::str::FromStr;
    let bytes = unsafe { std::slice::from_raw_parts(s_ptr, s_len as usize) };
    let parsed = std::str::from_utf8(bytes).ok().and_then(|s| Decimal::from_str(s).ok());
    match parsed {
        Some(d) => {
            unsafe { *ok_ptr = 1 };
            decimal_to_bits(d)
        }
        None => {
            unsafe { *ok_ptr = 0 };
            decimal_to_bits(Decimal::ZERO)
        }
    }
}

/// `a + b` — matches `interpreter.rs`'s `scalar_binop` `Dec128` arm:
/// `x + y` via `rust_decimal`'s own `Add` impl, which panics on genuine
/// overflow (the 96-bit mantissa's own limit) -- same abort-at-the-FFI-
/// boundary reasoning as `nir_dec128_from_i64`.
#[unsafe(no_mangle)]
pub extern "C" fn nir_dec128_add(a: Dec128Bits, b: Dec128Bits) -> Dec128Bits {
    decimal_to_bits(bits_to_decimal(a) + bits_to_decimal(b))
}

#[unsafe(no_mangle)]
pub extern "C" fn nir_dec128_sub(a: Dec128Bits, b: Dec128Bits) -> Dec128Bits {
    decimal_to_bits(bits_to_decimal(a) - bits_to_decimal(b))
}

#[unsafe(no_mangle)]
pub extern "C" fn nir_dec128_mul(a: Dec128Bits, b: Dec128Bits) -> Dec128Bits {
    decimal_to_bits(bits_to_decimal(a) * bits_to_decimal(b))
}

/// `a / b` — matches `interpreter.rs`'s own `Div`/`ElemDiv` arm: a zero
/// divisor is `ErrorKind::DivByZero` there (a real, catchable Nirdosha
/// `RuntimeError`, not a Rust panic) — the compiled path has no
/// equivalent catchable-error channel (same category as integer
/// division's own Tier-2 div-by-zero guard, which `codegen.rs` compiles
/// to an unconditional `abort()`, never a language-visible `Result`),
/// so this traps too, via a genuine Rust panic (`panic = "abort"` turns
/// it into exactly that `abort()`), for the same reason.
#[unsafe(no_mangle)]
pub extern "C" fn nir_dec128_div(a: Dec128Bits, b: Dec128Bits) -> Dec128Bits {
    let (a, b) = (bits_to_decimal(a), bits_to_decimal(b));
    if b.is_zero() {
        std::process::abort();
    }
    decimal_to_bits(a / b)
}
