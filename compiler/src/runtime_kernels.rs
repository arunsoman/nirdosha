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
//! **This file is compiled as its own freestanding crate** by
//! `compiler/build.rs` (a direct `rustc --crate-type staticlib`
//! invocation, not part of the `nirdosha` lib's own crate graph — see
//! that file for why), so it cannot `use` anything from `interpreter.rs`
//! across that compilation-unit boundary. Every algorithm here is
//! therefore a deliberate line-for-line mirror of the corresponding
//! `&[f64]`-taking function in `interpreter.rs` (`matrix_det`/
//! `matrix_inv`/`matrix_solve`/`matrix_rank`/`kf_update`/`mat_mul_f64`/
//! `mat_vec_mul_f64`/`mat_transpose_f64`/`vec_add_f64`/`vec_sub_f64`) —
//! if you change the algorithm in one place, change it in the other, and
//! `compiler/tests/codegen.rs`'s interpreter-parity tests will catch a
//! divergence immediately if you forget.
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
// A `tcp`/`tcp_listener` handle is a raw OS file descriptor, not a Rust
// `TcpStream`/`TcpListener` kept alive across calls the way
// `interpreter.rs`'s `Value::Tcp`'s `Arc<Mutex<Option<..>>>` does — the
// kernel already tracks everything a "handle" needs, so `codegen.rs`
// lowers `Ty::Tcp`/`Ty::TcpListener` straight to `i64`. Every kernel below
// reconstructs a `std`-level view of that fd for the duration of one call
// via `from_raw_fd`, wrapped in `ManuallyDrop` wherever the fd must stay
// open afterward (only `nir_tcp_stop` actually wants the real `Drop`/close
// to run). This mirrors `interpreter.rs`'s exact error/port-validation
// behavior (`Expr::Connect`/`Expr::Listen`/`read_tcp`/`write_tcp`) — see
// each fn's doc comment for the specific line it matches.

use std::io::{Read, Write};
use std::mem::ManuallyDrop;
use std::net::{TcpListener, TcpStream};
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd, RawFd};

/// Connects to `host:port` (`host` a `{ptr, len}` UTF-8 buffer). Returns
/// the new connection's fd on success, `-1` on failure — mirrors
/// `interpreter.rs`'s `Expr::Connect`: `u16::try_from(port)` (an
/// out-of-range port is a failure, not a silent truncation) then
/// `TcpStream::connect`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_tcp_connect(host_ptr: *const u8, host_len: i64, port: i64) -> i64 {
    let host = unsafe { std::slice::from_raw_parts(host_ptr, host_len as usize) };
    let Ok(host) = std::str::from_utf8(host) else { return -1 };
    let Ok(port) = u16::try_from(port) else { return -1 };
    match TcpStream::connect((host, port)) {
        Ok(stream) => stream.into_raw_fd() as i64,
        Err(_) => -1,
    }
}

/// Binds `0.0.0.0:port` (all interfaces, matching `interpreter.rs`'s
/// `Expr::Listen` — not just loopback). Returns the listener's fd on
/// success, `-1` on failure (including an out-of-range port).
#[unsafe(no_mangle)]
pub extern "C" fn nir_tcp_listen(port: i64) -> i64 {
    let Ok(port) = u16::try_from(port) else { return -1 };
    match TcpListener::bind(("0.0.0.0", port)) {
        Ok(listener) => listener.into_raw_fd() as i64,
        Err(_) => -1,
    }
}

/// Blocks for the next connection on `listener_fd`. Returns the accepted
/// connection's own fd on success, `-1` on failure. `listener_fd` itself
/// is left open and reusable (`accept` doesn't consume the listener,
/// `ownership.rs`'s `touch_expr(listener, false)`) — `ManuallyDrop` stops
/// the temporary `TcpListener` view constructed here from closing it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_tcp_accept(listener_fd: i64) -> i64 {
    let listener = ManuallyDrop::new(unsafe { TcpListener::from_raw_fd(listener_fd as RawFd) });
    match listener.accept() {
        Ok((stream, _addr)) => stream.into_raw_fd() as i64,
        Err(_) => -1,
    }
}

/// Sends `buf` in full over `fd` — `write_all`, matching
/// `interpreter.rs`'s `write_tcp` exactly (it loops internally until
/// every byte is written, not a single partial-write return). Returns
/// `buf_len` on success, `-1` on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_tcp_send(fd: i64, buf_ptr: *const u8, buf_len: i64) -> i64 {
    let mut stream = ManuallyDrop::new(unsafe { TcpStream::from_raw_fd(fd as RawFd) });
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
pub unsafe extern "C" fn nir_tcp_recv(fd: i64, buf_ptr: *mut u8, buf_cap: i64) -> i64 {
    let mut stream = ManuallyDrop::new(unsafe { TcpStream::from_raw_fd(fd as RawFd) });
    let buf = unsafe { std::slice::from_raw_parts_mut(buf_ptr, buf_cap as usize) };
    match stream.read(buf) {
        Ok(n) => n as i64,
        Err(_) => -1,
    }
}

/// Closes `fd` — serves both `tcp` and `tcp_listener` uniformly (both are
/// plain sockets at the OS level, so one raw-fd close path is correct for
/// either). `ownership.rs`'s affine-typing already proves this runs at
/// most once per handle in a well-typed program (this file's module doc's
/// "the checker is the real gate" convention) — reconstructing an
/// *owned* `OwnedFd` (not `ManuallyDrop`) and letting it drop is what
/// actually closes the socket.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nir_tcp_stop(fd: i64) -> i32 {
    drop(unsafe { OwnedFd::from_raw_fd(fd as RawFd) });
    0
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

/// Frees a pointer previously returned by `nir_alloc`. Not called by any
/// codegen'd program yet (`codegen.rs`'s `Expr::Box` doc comment — a
/// later phase wires the call sites once `ownership.rs`'s move data is
/// threaded into codegen), but implemented now, correctly, so that later
/// phase is a pure call-site change against an already-working allocator.
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
