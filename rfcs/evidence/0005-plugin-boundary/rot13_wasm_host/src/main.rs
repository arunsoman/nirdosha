//! The host side of the Kind-C spike: loads `rot13_wasm_guest.wasm`
//! through `wasmtime` and measures real per-call overhead against
//! Kind A's same-process `Arc<dyn Fn>` dispatch (measured separately in
//! plugin_bench). Every call here does the real, honest cross-boundary
//! work a genuine WASM-sandboxed plugin call requires: allocate guest
//! memory, copy the input bytes in, call, copy the output bytes out,
//! free the guest buffer -- no shortcuts.

use std::time::Instant;
use wasmtime::*;

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
    println!(
        "{label:<55} best of 5: {:>10.3} ms total   {:>9.2} ns/call   (checksum {checksum})",
        best.as_secs_f64() * 1000.0,
        ns_per_iter
    );
}

fn main() -> Result<()> {
    let wasm_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "rot13_wasm_guest.wasm".to_string());

    let engine = Engine::default();
    let module = Module::from_file(&engine, &wasm_path)?;
    let mut store: Store<()> = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[])?;

    let alloc = instance.get_typed_func::<u32, u32>(&mut store, "alloc")?;
    let dealloc = instance.get_typed_func::<(u32, u32), ()>(&mut store, "dealloc")?;
    let rot13 = instance.get_typed_func::<(u32, u32), ()>(&mut store, "rot13_inplace")?;
    let memory = instance.get_memory(&mut store, "memory").expect("guest must export linear memory");

    let input = b"Hello, Nirdosha! this is a representative short string.";
    let len = input.len() as u32;

    let iters: u64 = 2_000_000; // 10x fewer than the in-process bench -- a real WASM call is
                                // orders of magnitude slower per the numbers below, so this
                                // still runs in a reasonable time while staying a large,
                                // noise-resistant sample.

    println!("Guest module: {wasm_path}");
    println!("Payload: {len} bytes, {iters} iterations, best of 5\n");

    // 1. Full realistic round trip: alloc + copy in + call + copy out + dealloc --
    //    exactly what a real WASM-sandboxed plugin call requires every time,
    //    with no memory reuse across calls (the honest worst case for a
    //    one-shot call convention).
    bench("1. full round trip: alloc+copy-in+call+copy-out+dealloc", iters, || {
        let ptr = alloc.call(&mut store, len).unwrap();
        memory.write(&mut store, ptr as usize, input).unwrap();
        rot13.call(&mut store, (ptr, len)).unwrap();
        let mut out = vec![0u8; len as usize];
        memory.read(&store, ptr as usize, &mut out).unwrap();
        dealloc.call(&mut store, (ptr, len)).unwrap();
        out.len() as u64 ^ out[0] as u64
    });

    // 2. Call overhead only, buffer reused across calls (a plugin author
    //    who caches a scratch buffer instead of alloc/dealloc-ing every
    //    time -- the best realistic case, isolating the WASM call
    //    mechanism itself from allocation/copy cost).
    let ptr = alloc.call(&mut store, len).unwrap();
    memory.write(&mut store, ptr as usize, input).unwrap();
    bench("2. call only, buffer pre-allocated + reused (no copy)", iters, || {
        rot13.call(&mut store, (ptr, len)).unwrap();
        1
    });
    dealloc.call(&mut store, (ptr, len)).unwrap();

    // 3. Copy-in + copy-out only, no call -- isolates memcpy-across-the-
    //    boundary cost from the WASM call-dispatch cost.
    let ptr = alloc.call(&mut store, len).unwrap();
    bench("3. copy-in + copy-out only (no call)", iters, || {
        memory.write(&mut store, ptr as usize, input).unwrap();
        let mut out = vec![0u8; len as usize];
        memory.read(&store, ptr as usize, &mut out).unwrap();
        out.len() as u64
    });
    dealloc.call(&mut store, (ptr, len)).unwrap();

    // 4-6. Repeat with a 64KB payload -- shows the copy cost (unlike
    //      Kind A's Arc-sharing) scaling with payload size.
    let big_payload = "The quick brown fox jumps over the lazy dog. ".repeat(1400);
    let big_len = big_payload.len() as u32;
    println!("\n-- repeated with a {} KB payload --", big_len / 1024);
    let big_iters: u64 = 200_000; // 10x fewer -- each call now does real O(n) work + O(n) copies

    bench("4. full round trip, 64KB payload", big_iters, || {
        let ptr = alloc.call(&mut store, big_len).unwrap();
        memory.write(&mut store, ptr as usize, big_payload.as_bytes()).unwrap();
        rot13.call(&mut store, (ptr, big_len)).unwrap();
        let mut out = vec![0u8; big_len as usize];
        memory.read(&store, ptr as usize, &mut out).unwrap();
        dealloc.call(&mut store, (ptr, big_len)).unwrap();
        out.len() as u64 ^ out[0] as u64
    });

    let ptr = alloc.call(&mut store, big_len).unwrap();
    memory.write(&mut store, ptr as usize, big_payload.as_bytes()).unwrap();
    bench("5. call only, 64KB payload, buffer reused (no copy)", big_iters, || {
        rot13.call(&mut store, (ptr, big_len)).unwrap();
        1
    });
    dealloc.call(&mut store, (ptr, big_len)).unwrap();

    let ptr = alloc.call(&mut store, big_len).unwrap();
    bench("6. copy-in + copy-out only, 64KB payload (no call)", big_iters, || {
        memory.write(&mut store, ptr as usize, big_payload.as_bytes()).unwrap();
        let mut out = vec![0u8; big_len as usize];
        memory.read(&store, ptr as usize, &mut out).unwrap();
        out.len() as u64
    });
    dealloc.call(&mut store, (ptr, big_len)).unwrap();

    Ok(())
}
