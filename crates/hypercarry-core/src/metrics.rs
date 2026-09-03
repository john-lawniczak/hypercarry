//! Pure funding and basis calculations.
//!
//! The calculations use exact decimal arithmetic and accept settlement
//! intervals explicitly so rates from different venues remain comparable.

use rust_decimal::Decimal;
use std::num::NonZeroU32;

const HOURS_PER_YEAR: u32 = 365 * 24;

/// Stable schema version for serialized outputs derived from this metric layer.
pub const METRICS_SCHEMA_VERSION: u32 = 1;

/// A funding settlement interval measured in hours.
///
/// The private [`NonZeroU32`] makes a zero-hour interval unrepresentable after
/// construction, so metric functions do not need a divide-by-zero error path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FundingInterval(NonZeroU32);

impl FundingInterval {
    /// Construct an interval, returning `None` when `hours` is zero.
    pub fn from_hours(hours: u32) -> Option<Self> {
        NonZeroU32::new(hours).map(Self)
    }

    /// Return the interval as an ordinary number of hours.
    pub const fn hours(self) -> u32 {
        self.0.get()
    }

    /// Normalize a whole-settlement rate to a per-hour rate.
    ///
    /// A `0.0008` rate that settles every 8 hours is `0.0001` per hour. The
    /// division is always safe because the interval is guaranteed nonzero.
    pub fn to_hourly(self, rate: Decimal) -> Decimal {
        rate / Decimal::from(self.hours())
    }
}

/// Annualize one per-settlement funding rate.
///
/// For hourly funding, this is `rate * 8_760`. For an eight-hour rate, it is
/// `rate * (8_760 / 8)`.
///
/// # Examples
///
/// ```
/// use hypercarry_core::metrics::{FundingInterval, funding_apr};
/// use rust_decimal::Decimal;
///
/// let hourly = FundingInterval::from_hours(1).expect("one hour is non-zero");
/// assert_eq!(funding_apr(Decimal::new(1, 4), hourly), Decimal::new(876, 3));
/// ```
pub fn funding_apr(rate: Decimal, interval: FundingInterval) -> Decimal {
    let settlements_per_year = Decimal::from(HOURS_PER_YEAR) / Decimal::from(interval.hours());

    settlements_per_year * rate
}

/// Perp-vs-spot basis at a single instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Basis {
    /// `perp_mark - spot_mid`, in quote-currency units.
    pub absolute: Decimal,
    /// `absolute / spot_mid`, a unitless ratio (`0.01` means the perp trades 1%
    /// above spot).
    pub ratio: Decimal,
}

/// Error returned by [`basis`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BasisError {
    /// The spot mid price is not strictly positive, which makes the ratio
    /// meaningless (division by zero or a sign inversion), so it is rejected
    /// at the boundary rather than propagated.
    NonPositiveSpot {
        /// Rejected spot-mid value.
        spot_mid: Decimal,
    },
    /// The absolute or ratio basis exceeds `Decimal`'s representable range.
    Overflow,
}

/// Compute the perp-vs-spot [`Basis`].
///
/// A negative or zero `perp_mark` is accepted: perpetual mark prices are not
/// mathematically required to be positive (a distressed or corner-case market
/// can trade at or below zero), and the basis/ratio remain well-defined as
/// long as `spot_mid` is strictly positive.
///
/// # Errors
///
/// Returns [`BasisError::NonPositiveSpot`] when `spot_mid <= 0`, and
/// [`BasisError::Overflow`] when the absolute or ratio basis would exceed the
/// representable `Decimal` range.
///
/// # Examples
///
/// ```
/// use hypercarry_core::metrics::basis;
/// use rust_decimal::Decimal;
///
/// let result = basis(Decimal::from(101), Decimal::from(100))?;
/// assert_eq!(result.absolute, Decimal::ONE);
/// assert_eq!(result.ratio, Decimal::new(1, 2));
/// # Ok::<(), hypercarry_core::metrics::BasisError>(())
/// ```
pub fn basis(perp_mark: Decimal, spot_mid: Decimal) -> Result<Basis, BasisError> {
    if spot_mid <= Decimal::ZERO {
        return Err(BasisError::NonPositiveSpot { spot_mid });
    }

    let absolute = perp_mark
        .checked_sub(spot_mid)
        .ok_or(BasisError::Overflow)?;
    let ratio = absolute.checked_div(spot_mid).ok_or(BasisError::Overflow)?;

    Ok(Basis { absolute, ratio })
}

/// Cross-venue hourly funding spread: `venue_a - venue_b`, each normalized to a
/// per-hour rate first so venues with different settlement schedules compare
/// like-for-like.
///
/// A positive result means venue A pays more funding per hour than venue B.
pub fn hourly_spread(
    a_rate: Decimal,
    a_interval: FundingInterval,
    b_rate: Decimal,
    b_interval: FundingInterval,
) -> Decimal {
    a_interval.to_hourly(a_rate) - b_interval.to_hourly(b_rate)
}

/// Summary statistics over a window of settled *per-hour* funding rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FundingStats {
    /// Arithmetic mean of the window.
    pub mean: Decimal,
    /// Smallest hourly rate in the window.
    pub min: Decimal,
    /// Largest hourly rate in the window.
    pub max: Decimal,
    /// Number of adjacent pairs whose sign strictly flipped (+ to - or - to +).
    /// Zeros never count as a flip.
    pub sign_flips: usize,
    /// `mean * HOURS_PER_YEAR` — the APR the window's average rate implies.
    pub realized_apr: Decimal,
}

impl FundingStats {
    /// Aggregate a window of per-hour funding rates.
    ///
    /// Returns `None` for an empty window, where mean, min, and max are all
    /// undefined.
    pub fn from_hourly_rates(rates: &[Decimal]) -> Option<Self> {
        if rates.is_empty() {
            return None;
        }

        let sum = rates.iter().sum::<Decimal>();
        let mean = sum / Decimal::from(rates.len());
        let min = rates.iter().copied().min()?;
        let max = rates.iter().copied().max()?;
        let sign_flips = rates
            .windows(2)
            .filter(|window| {
                (window[0] > Decimal::ZERO && window[1] < Decimal::ZERO)
                    || (window[0] < Decimal::ZERO && window[1] > Decimal::ZERO)
            })
            .count();
        let realized_apr = mean * Decimal::from(HOURS_PER_YEAR);

        Some(Self {
            mean,
            min,
            max,
            sign_flips,
            realized_apr,
        })
    }
}
