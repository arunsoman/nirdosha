//! The WASM guest side of the Kind-C spike (rfcs/0004's "a narrow spike
//! compiling rot13 to WASM and measuring call overhead through wasmtime").
//! Same algorithm as crates/plugin-example-rot13/src/lib.rs's
//! `rot13_call`, exposed through a minimal, explicit linear-memory
//! calling convention (the host allocates via `alloc`, writes bytes in,
//! calls `rot13_inplace`, reads bytes back, then calls `dealloc`) --
//! deliberately not the full WASM Component Model/wit-bindgen tooling,
//! scoped to exactly what's needed to measure real cross-boundary copy
//! + call overhead.

/// Host calls this first to get a buffer of `len` bytes it can write
/// the input string into. Returns a raw pointer into guest linear
/// memory -- valid until `dealloc` is called with the same (ptr, len).
#[no_mangle]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut buf: Vec<u8> = Vec::with_capacity(len);
    #[allow(clippy::uninit_vec)]
    unsafe {
        buf.set_len(len);
    }
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Frees a buffer previously returned by `alloc` (or left in place by
/// `rot13_inplace`, which never reallocates -- same ptr/len both ways).
#[no_mangle]
pub extern "C" fn dealloc(ptr: *mut u8, len: usize) {
    unsafe {
        drop(Vec::from_raw_parts(ptr, len, len));
    }
}

/// Rotates the ASCII letters in the `len` bytes at `ptr` in place --
/// identical transform to `rot13_call`, byte-oriented instead of
/// `char`-oriented since a WASM linear-memory contract has no `str`
/// concept, only bytes the host already knows are ASCII/UTF-8.
#[no_mangle]
pub extern "C" fn rot13_inplace(ptr: *mut u8, len: usize) {
    let bytes = unsafe { std::slice::from_raw_parts_mut(ptr, len) };
    for b in bytes.iter_mut() {
        *b = match *b {
            b'a'..=b'z' => ((*b - b'a' + 13) % 26) + b'a',
            b'A'..=b'Z' => ((*b - b'A' + 13) % 26) + b'A',
            other => other,
        };
    }
}
