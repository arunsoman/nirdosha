// Self-contained -- reproduce with a bare `rustc counterexamples/
// verify_dangling_ref.rs`. Same substitution as verify_double_send.rs:
// `std::sync::mpsc::Sender` stands in for a `Mailbox`'s sender.
struct Iso<T>(T);

fn send_a_borrow(tx: &std::sync::mpsc::Sender<Iso<&i64>>) {
    let local = 5;
    tx.send(Iso(&local)).unwrap();
}

fn main() {}
