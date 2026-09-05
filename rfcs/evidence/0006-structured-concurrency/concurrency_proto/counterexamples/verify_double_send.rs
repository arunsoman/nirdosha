// Self-contained (no dependency on the `concurrency_proto` crate, or
// any other) -- reproduce with a bare `rustc counterexamples/
// verify_double_send.rs`. `Iso<T>`'s definition here is copied
// verbatim from `src/lib.rs`; `std::sync::mpsc` (not
// `crossbeam_channel`) stands in for `Mailbox` since the mechanism
// under test -- a non-`Copy` value moved into a channel `send` cannot
// be used again -- is the exact same `rustc` move-checking regardless
// of which channel implementation receives it.
struct Iso<T>(T);

fn main() {
    let (tx, _rx) = std::sync::mpsc::channel::<Iso<i64>>();
    let v = Iso(42);
    tx.send(v).unwrap();
    tx.send(v).unwrap();
}
