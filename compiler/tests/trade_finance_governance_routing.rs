//! Machine-checkable form of `US-TRDPAY-002`'s acceptance criteria
//! (scratch/extracted_userstories_v2.json — "Automatic Risk-Based
//! Governance Routing at Payment Initiation"), whose `post_logic` states
//! `routed_to_six_eyes == (payment_amount > high_value_threshold)`.
//!
//! `.nir` has no general Hoare-style `requires`/`ensures` — `requires(...)`
//! is RBAC-only (LANGUAGE.md §6a). So this predicate isn't checked by a
//! contract annotation; it's checked by actually running the real
//! `.nir` routing rule (mirrored here from
//! `examples/trade-finance/trade_finance.nir:1733-1735`'s
//! `required_eyes_for_amount`, since that file's own `fn main() {}` and
//! DB-backed dependencies make it unusable as an `include_str!` test
//! entry point — same reason `constant_time_str_eq.rs` inlines its own
//! minimal `.nir` snippets rather than including the real file) through
//! `nirdosha::run` and asserting on the returned `Value`.
//!
//! `nir_scenario!`'s Given/When/Then grammar is enforced by `rustc` at
//! compile time (wrong keyword order, or a `Then` whose type doesn't
//! match `Value`, is a compile error) — but that only covers the Rust
//! harness shape. The `.nir` logic inside `when` is parsed/type-checked
//! by Nirdosha's own `typeck` when the test *runs*, not by `rustc` when
//! the crate builds; a malformed `.nir` scenario fails a `#[test]`, it
//! doesn't fail `cargo build`.
//!
//! Two discrepancies against the PRD-derived story surfaced while
//! writing these, both left as-is (not "fixed" here) since only a human
//! call can settle which side — the doc's example or the shipped
//! `.nir` rule — is authoritative:
//! 1. Threshold value: the story's GWTs use "$1,000,000" as illustrative
//!    (its own module excerpt hedges "e.g."); the shipped rule uses
//!    $50,000 (5,000,000 cents) — flagged in `trade_finance.nir`'s own
//!    comment as "a fixed illustrative cutoff" pending per-tenant config.
//!    These scenarios test the shipped $50,000 figure, not the story's.
//! 2. Boundary operator: the story's `post_logic` and its second GWT
//!    ("at or below the configured threshold" -> Maker-Checker) both
//!    imply a strict `>` for six-eyes, i.e. exactly-at-threshold should
//!    be Maker-Checker. The shipped rule uses `>=`, i.e.
//!    exactly-at-threshold is six-eyes. `boundary_case` below asserts
//!    the shipped `>=` behavior (six-eyes at exactly the threshold), the
//!    opposite of what the story's prose says — a real spec/code mismatch,
//!    not a test bug.

use nirdosha::interpreter::Value;
use nirdosha::run;

/// `.nir` source for the routing rule under test, mirrored verbatim from
/// `examples/trade-finance/trade_finance.nir:1733-1735`.
const REQUIRED_EYES_FOR_AMOUNT_NIR: &str = r#"
    fn required_eyes_for_amount(amount_cents: i64) -> i64 {
        return if amount_cents >= 5000000 { 2 } else { 1 }
    }
"#;

macro_rules! nir_scenario {
    (
        $name:ident:
        Given $fn_src:expr;
        When  $call_expr:expr;
        Then  $expect:expr;
    ) => {
        #[test]
        fn $name() {
            let src = format!(
                "{}\nfn main() -> i64 {{ return {} }}",
                $fn_src, $call_expr
            );
            let expected: Value = $expect;
            let actual = run(&src).unwrap_or_else(|e| {
                panic!("scenario `{}`: `.nir` run failed: {e:?}", stringify!($name))
            });
            assert_eq!(
                actual, expected,
                "BDD scenario `{}` failed",
                stringify!($name)
            );
        }
    };
}

// GWT 1: "a submitted trade payment with an amount above the configured
// high-value threshold ... is routed to the full 6-Eyes workflow rather
// than Maker-Checker" -> required_eyes == 2.
nir_scenario!(above_threshold_routes_to_six_eyes:
    Given REQUIRED_EYES_FOR_AMOUNT_NIR;
    When  "required_eyes_for_amount(5000001)";
    Then  Value::Int(2);
);

// GWT 2: "a submitted trade payment at or below the configured threshold
// ... is routed to Maker-Checker" -> required_eyes == 1 (below case).
nir_scenario!(below_threshold_routes_to_maker_checker:
    Given REQUIRED_EYES_FOR_AMOUNT_NIR;
    When  "required_eyes_for_amount(4999999)";
    Then  Value::Int(1);
);

// Boundary: exactly at the threshold. The shipped rule's `>=` makes this
// six-eyes -- the story's own "at or below -> Maker-Checker" wording
// says the opposite (see module doc comment above). Asserts actual
// shipped behavior, not the story's prose.
nir_scenario!(boundary_case_at_exact_threshold_is_six_eyes_per_shipped_code:
    Given REQUIRED_EYES_FOR_AMOUNT_NIR;
    When  "required_eyes_for_amount(5000000)";
    Then  Value::Int(2);
);
