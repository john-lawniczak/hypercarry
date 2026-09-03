//! Representative exact-decimal metric examples.

use hypercarry_core::{
    metrics::{
        Basis, BasisError, FundingInterval, FundingStats, basis, funding_apr, hourly_spread,
    },
    types::TimestampMs,
};
use rust_decimal::Decimal;
use std::str::FromStr;

fn dec(value: &str) -> Decimal {
    Decimal::from_str(value).expect("valid decimal literal in test")
}

#[test]
fn constructs_and_deserializes_timestamp_ms() {
    let timestamp = TimestampMs::new(1_683_849_600_048);
    let decoded: TimestampMs =
        serde_json::from_str("1683849600048").expect("integer timestamp parses");

    assert_eq!(timestamp.as_i64(), 1_683_849_600_048);
    assert_eq!(decoded, timestamp);
}

#[test]
fn funding_interval_rejects_zero() {
    assert_eq!(FundingInterval::from_hours(0), None);

    let hourly = FundingInterval::from_hours(1).expect("one hour is nonzero");
    let eight_hour = FundingInterval::from_hours(8).expect("eight hours is nonzero");

    assert_eq!(hourly.hours(), 1);
    assert_eq!(eight_hour.hours(), 8);
}

#[test]
fn annualizes_funding_rates() {
    let hourly = FundingInterval::from_hours(1).expect("one hour is nonzero");
    let eight_hour = FundingInterval::from_hours(8).expect("eight hours is nonzero");

    assert_eq!(funding_apr(dec("0.0001"), hourly), dec("0.876"));
    assert_eq!(funding_apr(dec("0.0008"), eight_hour), dec("0.876"));
    assert_eq!(funding_apr(dec("-0.0001"), hourly), dec("-0.876"));
    assert_eq!(funding_apr(Decimal::ZERO, hourly), Decimal::ZERO);
}

#[test]
fn computes_basis() {
    // Non-positive spot is rejected at the boundary.
    assert_eq!(
        basis(dec("100"), Decimal::ZERO),
        Err(BasisError::NonPositiveSpot {
            spot_mid: Decimal::ZERO
        })
    );
    assert!(basis(dec("100"), dec("-5")).is_err());

    // Overflowing the representable Decimal range fails closed instead of
    // panicking; a negative perp mark alone (no overflow) is accepted.
    assert_eq!(basis(Decimal::MIN, Decimal::ONE), Err(BasisError::Overflow));
    assert_eq!(
        basis(dec("-5"), dec("100")).expect("negative perp mark is a valid input"),
        Basis {
            absolute: dec("-105"),
            ratio: dec("-1.05"),
        }
    );

    // Equal prices -> zero basis.
    let flat = basis(dec("100"), dec("100")).expect("positive spot");
    assert_eq!(
        flat,
        Basis {
            absolute: Decimal::ZERO,
            ratio: Decimal::ZERO,
        }
    );

    // Perp above spot -> positive basis and ratio.
    let pos = basis(dec("101"), dec("100")).expect("positive spot");
    assert_eq!(pos.absolute, dec("1"));
    assert_eq!(pos.ratio, dec("0.01"));

    // Perp below spot -> the sign is preserved.
    let neg = basis(dec("99"), dec("100")).expect("positive spot");
    assert_eq!(neg.absolute, dec("-1"));
    assert_eq!(neg.ratio, dec("-0.01"));

    // Scale invariance: multiplying both prices by the same constant leaves the
    // ratio basis unchanged (only the absolute basis scales).
    let base = basis(dec("101"), dec("100")).expect("positive spot");
    let scaled = basis(dec("1010"), dec("1000")).expect("positive spot");
    assert_eq!(base.ratio, scaled.ratio);
    assert_eq!(scaled.absolute, dec("10"));
}

#[test]
fn normalizes_and_spreads_across_venues() {
    let hourly = FundingInterval::from_hours(1).expect("one hour is nonzero");
    let eight = FundingInterval::from_hours(8).expect("eight hours is nonzero");

    // A whole-settlement rate normalizes to its per-hour equivalent.
    assert_eq!(eight.to_hourly(dec("0.0008")), dec("0.0001"));
    assert_eq!(hourly.to_hourly(dec("0.0001")), dec("0.0001"));

    // Same effective hourly rate on both venues -> zero spread.
    assert_eq!(
        hourly_spread(dec("0.0001"), hourly, dec("0.0008"), eight),
        Decimal::ZERO
    );

    // Venue A richer per hour -> positive spread (0.0002/h vs 0.0001/h).
    assert_eq!(
        hourly_spread(dec("0.0002"), hourly, dec("0.0008"), eight),
        dec("0.0001")
    );
}

#[test]
fn aggregates_a_funding_window() {
    // An empty window has no defined mean/min/max.
    assert_eq!(FundingStats::from_hourly_rates(&[]), None);

    let rates = [dec("0.0001"), dec("-0.0002"), dec("0.0003"), Decimal::ZERO];
    let stats = FundingStats::from_hourly_rates(&rates).expect("non-empty window");

    // sum = 0.0002, mean = 0.0002 / 4 = 0.00005
    assert_eq!(stats.mean, dec("0.00005"));
    assert_eq!(stats.min, dec("-0.0002"));
    assert_eq!(stats.max, dec("0.0003"));
    // 0.0001 -> -0.0002 (flip), -0.0002 -> 0.0003 (flip), 0.0003 -> 0 (no flip)
    assert_eq!(stats.sign_flips, 2);
    // realized_apr = mean * 8760 = 0.00005 * 8760 = 0.438
    assert_eq!(stats.realized_apr, dec("0.438"));
}
