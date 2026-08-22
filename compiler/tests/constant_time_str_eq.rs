//! Tests for `constant_time_str_eq` — a red-team finding, fixed: `.nir`
//! code comparing two secret-derived strings (e.g. a decision-link token
//! against its stored value, `examples/trade-finance/trade_finance.nir`)
//! had no way to do it other than `==`, a short-circuiting comparison
//! that's a timing side channel on whatever secret comparison the
//! program was trying to do. Backed by the same constant-time comparison
//! `interpreter.rs`'s own JWT signature check now uses internally.

use nirdosha::interpreter::Value;
use nirdosha::run;

#[test]
fn equal_strings_compare_equal() {
    let src = r#"
        fn main() -> bool {
            return constant_time_str_eq("same-token-value", "same-token-value")
        }
    "#;
    match run(src) {
        Ok(Value::Bool(b)) => assert!(b),
        other => panic!("expected Ok(Bool(true)), got {other:?}"),
    }
}

#[test]
fn different_strings_compare_unequal() {
    let src = r#"
        fn main() -> bool {
            return constant_time_str_eq("token-a", "token-b")
        }
    "#;
    match run(src) {
        Ok(Value::Bool(b)) => assert!(!b),
        other => panic!("expected Ok(Bool(false)), got {other:?}"),
    }
}

#[test]
fn different_length_strings_compare_unequal() {
    let src = r#"
        fn main() -> bool {
            return constant_time_str_eq("short", "much-longer-string")
        }
    "#;
    match run(src) {
        Ok(Value::Bool(b)) => assert!(!b),
        other => panic!("expected Ok(Bool(false)), got {other:?}"),
    }
}

#[test]
fn empty_strings_compare_equal() {
    let src = r#"
        fn main() -> bool {
            return constant_time_str_eq("", "")
        }
    "#;
    match run(src) {
        Ok(Value::Bool(b)) => assert!(b),
        other => panic!("expected Ok(Bool(true)), got {other:?}"),
    }
}
