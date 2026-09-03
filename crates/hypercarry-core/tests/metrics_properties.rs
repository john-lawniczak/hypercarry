use hypercarry_core::metrics::{FundingInterval, FundingStats, basis, hourly_spread};
use proptest::prelude::*;
use rust_decimal::Decimal;

fn decimal(coefficient: i64) -> Decimal {
    Decimal::new(coefficient, 6)
}

proptest! {
    #[test]
    fn basis_preserves_the_direction_of_price_difference(
        spot_coefficient in 1_i64..=1_000_000_000,
        difference_coefficient in -1_000_000_000_i64..=1_000_000_000,
    ) {
        let spot = decimal(spot_coefficient);
        let difference = decimal(difference_coefficient);
        let result = basis(spot + difference, spot).expect("generated spot is positive");

        prop_assert_eq!(result.absolute, difference);
        prop_assert_eq!(result.ratio.cmp(&Decimal::ZERO), difference.cmp(&Decimal::ZERO));
    }

    #[test]
    fn basis_ratio_is_scale_invariant(
        spot_coefficient in 1_i64..=1_000_000_000,
        perp_coefficient in -1_000_000_000_i64..=1_000_000_000,
        scale in 1_i64..=1_000,
    ) {
        let spot = decimal(spot_coefficient);
        let perp = decimal(perp_coefficient);
        let scale = Decimal::from(scale);

        let unscaled = basis(perp, spot).expect("generated spot is positive");
        let scaled = basis(perp * scale, spot * scale).expect("scaled spot is positive");

        prop_assert_eq!(scaled.ratio, unscaled.ratio);
        prop_assert_eq!(scaled.absolute, unscaled.absolute * scale);
    }

    #[test]
    fn funding_window_mean_stays_between_extrema(
        coefficients in prop::collection::vec(-1_000_000_i64..=1_000_000, 1..=256),
    ) {
        let rates: Vec<_> = coefficients.into_iter().map(decimal).collect();
        let stats = FundingStats::from_hourly_rates(&rates).expect("generated window is non-empty");

        prop_assert!(stats.min <= stats.mean);
        prop_assert!(stats.mean <= stats.max);
    }

    #[test]
    fn equivalent_hourly_rates_have_zero_spread(
        coefficient in -1_000_000_i64..=1_000_000,
        hours in 1_u32..=24,
    ) {
        let hourly = FundingInterval::from_hours(1).expect("one hour is nonzero");
        let interval = FundingInterval::from_hours(hours).expect("generated interval is nonzero");
        let hourly_rate = decimal(coefficient);

        prop_assert_eq!(
            hourly_spread(hourly_rate, hourly, hourly_rate * Decimal::from(hours), interval),
            Decimal::ZERO,
        );
    }
}
