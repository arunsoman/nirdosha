//! Tests for `sha256_hex` — a plain SHA-256 hex-digest builtin, exposing
//! the same hasher `validate_api_key` already uses internally
//! (`interpreter.rs`). Added to make a real hash-chained audit log
//! (`examples/trade-finance/`) possible from Nirdosha source; see
//! `examples/trade-finance/todo.md`'s Phase 0.

use nirdosha::interpreter::Value;
use nirdosha::run;

#[test]
fn hashes_a_known_string_to_the_known_digest() {
    // sha256("") -- a standard, independently-verifiable test vector.
    let src = r#"
        struct Text {
            value: str,
        }
        fn main() -> Text {
            return Text(sha256_hex(""))
        }
    "#;
    match run(src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
            other => panic!("expected Text(Str(digest)), got Text({other:?})"),
        },
        other => panic!("expected the known sha256(\"\") digest, got {other:?}"),
    }
}

#[test]
fn is_deterministic_and_input_sensitive() {
    let src = r#"
        fn main() -> bool {
            let a: str = sha256_hex("hello")
            let b: str = sha256_hex("hello")
            let c: str = sha256_hex("hellO")
            return a == b && a != c
        }
    "#;
    match run(src) {
        Ok(Value::Bool(b)) => assert!(b),
        other => panic!("expected Ok(Bool(true)), got {other:?}"),
    }
}

#[test]
fn two_arg_form_hashes_both_parts_without_needing_concatenation() {
    // Nirdosha `str` has no `+`/concatenation (docs/LANGUAGE.md §2), so a
    // hash-chained audit log (`examples/trade-finance/`) can't build
    // `sha256_hex(prev_hash + payload)` the way most languages would.
    // The 2-arg form does the combining in Rust instead:
    // `sha256_hex(prev_hash, payload)`. This test locks in that the
    // result is deterministic and sensitive to *both* inputs
    // independently -- swapping either one changes the hash, which is
    // the actual tamper-evidence property a hash chain needs.
    let src = r#"
        fn main() -> bool {
            let genesis: str = sha256_hex("genesis")
            let h1: str = sha256_hex(genesis, "entry one")
            let h1_again: str = sha256_hex(genesis, "entry one")
            let h1_tampered_payload: str = sha256_hex(genesis, "entry ONE")
            let h1_tampered_prev: str = sha256_hex(sha256_hex("different genesis"), "entry one")
            return h1 == h1_again && h1 != h1_tampered_payload && h1 != h1_tampered_prev
        }
    "#;
    match run(src) {
        Ok(Value::Bool(b)) => assert!(b, "hash chain must be deterministic and sensitive to both prev_hash and payload"),
        other => panic!("expected Ok(Bool(true)), got {other:?}"),
    }
}
