//! Exact UTC rendering for CLI output and frozen daily partition names.
use time::{OffsetDateTime, macros::format_description};

/// Render Unix milliseconds as RFC3339 UTC with exactly three fractional digits.
/// Out-of-range calendar dates return `None`; no rounding or timezone lookup.
pub fn format_millis(ms: i64) -> Option<String> {
    let time = OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()?;
    time.format(format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
    ))
    .ok()
}

/// Render the UTC date used in settled-funding partition paths.
pub fn date_millis(ms: i64) -> Option<String> {
    let time = OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()?;
    time.format(format_description!("[year]-[month]-[day]"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utc_milliseconds_and_day_boundaries_are_exact() {
        for (ms, expected) in [
            (0, "1970-01-01T00:00:00.000Z"),
            (-1, "1969-12-31T23:59:59.999Z"),
            (1_709_164_800_123, "2024-02-29T00:00:00.123Z"),
            (1_709_164_799_999, "2024-02-28T23:59:59.999Z"),
        ] {
            assert_eq!(format_millis(ms).as_deref(), Some(expected));
            assert_eq!(date_millis(ms).as_deref(), Some(&expected[..10]));
        }
        assert!(format_millis(i64::MAX).is_none());
        assert!(date_millis(i64::MIN).is_none());
    }
}
