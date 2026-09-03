use hypercarry_core::predictor::{hourly_funding_from_average_premium, premium_index};
use proptest::prelude::*;
use rust_decimal::Decimal;

proptest! {
    #[test]
    fn hourly_baseline_never_exceeds_documented_cap(mantissa in -1_000_000_i64..=1_000_000) {
        let premium = Decimal::new(mantissa, 4);
        let funding = hourly_funding_from_average_premium(premium).expect("bounded input is representable");
        let cap = Decimal::new(4, 2);
        prop_assert!(funding >= -cap);
        prop_assert!(funding <= cap);
    }

    #[test]
    fn premium_is_antisymmetric_for_mirrored_impact_prices(
        oracle_units in 1_i64..=1_000_000,
        offset_units in 0_i64..=100_000,
    ) {
        let oracle = Decimal::from(oracle_units);
        let offset = Decimal::new(offset_units, 4);
        let positive = premium_index(oracle, oracle + offset, oracle + offset)
            .expect("positive mirrored prices are representable");
        let negative_impact = oracle - offset;
        prop_assume!(negative_impact > Decimal::ZERO);
        let negative = premium_index(oracle, negative_impact, negative_impact)
            .expect("negative mirrored prices are representable");
        prop_assert_eq!(positive, -negative);
    }
}
