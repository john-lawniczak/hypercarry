//! What the venue says about the account, and what that means.
//!
//! The parsing and decision logic here is pure and takes already-fetched JSON,
//! so every claim the supervisor makes can be exercised offline against exact
//! venue payloads. The transport is a thin shell around it.

use anyhow::{Context, Result, ensure};
use rust_decimal::Decimal;
use serde_json::Value;

/// Names of every perp DEX an account can hold a position on.
///
/// The response lists the default DEX as a literal `null` at index 0, with
/// builder-deployed DEXs following it as objects. `None` here means the default
/// DEX, which is queried by omitting the `dex` field rather than by naming it.
///
/// # Errors
///
/// Returns an error if the response is not an array, or if an entry is neither
/// null nor an object with a name.
pub fn perp_dex_names(response: &Value) -> Result<Vec<Option<String>>> {
    let entries = response.as_array().context("perpDexs must be an array")?;
    entries
        .iter()
        .map(|entry| match entry {
            Value::Null => Ok(None),
            Value::Object(_) => entry
                .get("name")
                .and_then(Value::as_str)
                .map(|name| Some(name.to_owned()))
                .context("perp DEX entry has no name"),
            _ => anyhow::bail!("perp DEX entry must be null or an object"),
        })
        .collect()
}

/// Whether one clearinghouse response shows no open position.
///
/// A missing `assetPositions` array is an error rather than an absence. The
/// difference between "this account holds nothing" and "this response did not
/// say" is the whole value of the flatness claim, and only one of them is safe
/// to report as flat.
///
/// # Errors
///
/// Returns an error for a missing positions array or an unparseable size.
pub fn is_flat(state: &Value) -> Result<bool> {
    let positions = state["assetPositions"]
        .as_array()
        .context("clearinghouse state has no assetPositions")?;
    for position in positions {
        if decimal(position.pointer("/position/szi"))? != Decimal::ZERO {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Signed realized `PnL` over the trailing window. Fills never count as orders.
///
/// Fills are the venue's own record of what the account did, so they are the
/// only source that survives this process restarting. Loss is negative, which
/// is the sign convention the risk policy compares against.
///
/// # Errors
///
/// Returns an error for a response that is not an array of fills, or a fill
/// with an unparseable time or realized `PnL`.
pub fn rolling_realized_pnl(fills: &Value, now_ms: i64, window_ms: i64) -> Result<Decimal> {
    ensure!(window_ms > 0, "rolling window must be positive");
    let entries = fills.as_array().context("userFills must be an array")?;
    let horizon = now_ms.checked_sub(window_ms).context("window underflows")?;
    let mut total = Decimal::ZERO;
    ensure!(
        entries.len() < 2_000,
        "fill history is truncated; cannot establish rolling PnL"
    );
    for fill in entries {
        let time = fill
            .get("time")
            .and_then(Value::as_i64)
            .context("fill has no time")?;
        if time < horizon || time > now_ms {
            continue;
        }
        total = total
            .checked_add(decimal(fill.get("closedPnl"))?)
            .context("realized PnL overflows")?;
    }
    Ok(total)
}

/// Submission timestamps, deduplicated by venue order ID rather than fill ID.
/// Includes canceled, rejected and still-open orders. Status-update timestamps
/// must never extend an old order's frequency window. A full response is refused
/// because the venue caps history at 2,000 records and completeness is unknown.
fn submissions_from_history(history: &Value, now_ms: i64, window_ms: i64) -> Result<Vec<i64>> {
    ensure!(window_ms > 0, "order window must be positive");
    let horizon = now_ms
        .checked_sub(window_ms)
        .context("order window underflows")?;
    let entries = history
        .as_array()
        .context("historicalOrders must be an array")?;
    ensure!(
        entries.len() < 2_000,
        "order history is truncated; frequency is unknown"
    );
    let mut orders = std::collections::BTreeMap::new();
    for entry in entries {
        let order = entry.get("order").context("historical order is missing")?;
        let id = order
            .get("oid")
            .and_then(Value::as_u64)
            .context("order has no ID")?;
        let time = order
            .get("timestamp")
            .and_then(Value::as_i64)
            .context("order has no submission time")?;
        ensure!(
            time >= 0 && time <= now_ms,
            "invalid or future order timestamp"
        );
        if let Some(previous) = orders.insert(id, time) {
            ensure!(
                previous == time,
                "conflicting submission times for one order"
            );
        }
    }
    let mut times: Vec<_> = orders
        .into_values()
        .filter(|time| *time >= horizon)
        .collect();
    times.sort_unstable();
    Ok(times)
}

/// Returns venue-acknowledged submissions in the risk policy's frequency window.
/// Transport-uncertain local attempts must additionally be reconciled before
/// readiness can be granted; an empty venue history never proves their absence.
pub fn recent_order_submissions(history: &Value, now_ms: i64, window_ms: i64) -> Result<Vec<i64>> {
    submissions_from_history(history, now_ms, window_ms)
}

/// Open order count and aggregate notional from an `openOrders` response.
///
/// # Errors
///
/// Returns an error for a response that is not an array, or an order with an
/// unparseable price or size.
pub fn open_orders(response: &Value) -> Result<(usize, Decimal)> {
    let orders = response.as_array().context("openOrders must be an array")?;
    let mut notional = Decimal::ZERO;
    for order in orders {
        let price = decimal(order.get("limitPx"))?;
        let size = decimal(order.get("sz"))?;
        notional = price
            .checked_mul(size)
            .and_then(|value| notional.checked_add(value))
            .context("aggregate notional overflows")?;
    }
    Ok((orders.len(), notional))
}

/// Mid price for one market from an `allMids` response.
///
/// # Errors
///
/// Returns an error if the market is absent or its price is unparseable.
pub fn mid_price(response: &Value, market: &str) -> Result<Decimal> {
    decimal(response.get(market)).with_context(|| format!("no mid price for {market}"))
}

fn decimal(value: Option<&Value>) -> Result<Decimal> {
    value
        .and_then(Value::as_str)
        .context("missing decimal string")?
        .parse()
        .context("invalid decimal")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The default DEX is a literal null, not an object with a default name.
    #[test]
    fn the_default_perp_dex_is_the_null_entry() {
        let response = json!([
            null,
            {"name": "test", "fullName": "test dex", "deployer": "0x00"},
        ]);

        assert_eq!(
            perp_dex_names(&response).unwrap(),
            vec![None, Some("test".to_owned())]
        );
    }

    /// A position on any DEX makes the account not flat. Checking only the
    /// default DEX is exactly the gap the supervisor exists to close.
    #[test]
    fn a_position_on_any_dex_defeats_flatness() {
        let flat = json!({"assetPositions": []});
        let zeroed = json!({"assetPositions": [{"position": {"szi": "0.0"}}]});
        let held = json!({"assetPositions": [{"position": {"szi": "0.0335"}}]});
        let short = json!({"assetPositions": [{"position": {"szi": "-1.0"}}]});

        assert!(is_flat(&flat).unwrap());
        assert!(is_flat(&zeroed).unwrap());
        assert!(!is_flat(&held).unwrap());
        assert!(!is_flat(&short).unwrap());
    }

    /// "Holds nothing" and "did not say" are different claims, and only one of
    /// them is safe to report as flat.
    #[test]
    fn a_response_that_does_not_report_positions_is_an_error_not_a_flat_account() {
        assert!(is_flat(&json!({})).is_err());
        assert!(is_flat(&json!({"assetPositions": "none"})).is_err());
    }

    #[test]
    fn realized_pnl_sums_only_fills_inside_the_window() {
        let now = 1_700_000_000_000_i64;
        let hour = 3_600_000_i64;
        let fills = json!([
            {"time": now - 3 * hour, "closedPnl": "100.0"},
            {"time": now - hour,     "closedPnl": "-25.5"},
            {"time": now - 60_000,   "closedPnl": "10.0"},
            {"time": now + hour,     "closedPnl": "999.0"},
        ]);

        let pnl = rolling_realized_pnl(&fills, now, 2 * hour).unwrap();

        // The three-hour-old fill is outside the window and the future-dated
        // one cannot have happened yet.
        assert_eq!(pnl, Decimal::new(-155, 1));
    }

    #[test]
    fn a_loss_is_reported_as_a_negative_number() {
        let now = 1_700_000_000_000_i64;
        let fills = json!([{"time": now - 1_000, "closedPnl": "-42.0"}]);

        let pnl = rolling_realized_pnl(&fills, now, 60_000).unwrap();

        assert!(pnl.is_sign_negative());
        assert_eq!(pnl, Decimal::new(-42, 0));
    }

    #[test]
    fn frequency_counts_unfilled_canceled_and_rejected_orders_once() {
        let history = json!([
            {"order":{"oid":1,"timestamp":1000},"status":"canceled","statusTimestamp":3000},
            {"order":{"oid":2,"timestamp":2000},"status":"open"},
            {"order":{"oid":2,"timestamp":2000},"status":"filled"},
            {"order":{"oid":3,"timestamp":3000},"status":"rejected"},
            {"order":{"oid":4,"timestamp":999},"status":"filled","statusTimestamp":3000}
        ]);
        assert_eq!(
            recent_order_submissions(&history, 3000, 2000).unwrap(),
            vec![1000, 2000, 3000]
        );
    }

    #[test]
    fn frequency_refuses_incomplete_or_conflicting_history() {
        for history in [
            json!([{"order":{"oid":1,"timestamp":3001}}]),
            json!([{"order":{"oid":1,"timestamp":1000}},{"order":{"oid":1,"timestamp":2000}}]),
            json!([{"order":{"oid":1}}]),
            json!(vec![json!({"order":{"oid":1,"timestamp":1000}}); 2000]),
        ] {
            assert!(recent_order_submissions(&history, 3000, 2000).is_err());
        }
        assert!(
            rolling_realized_pnl(
                &json!(vec![json!({"time":1000,"closedPnl":"0"}); 2000]),
                3000,
                2000
            )
            .is_err()
        );
    }

    #[test]
    fn open_orders_report_their_count_and_aggregate_notional() {
        let response = json!([
            {"limitPx": "80000.0", "sz": "0.001"},
            {"limitPx": "79000.0", "sz": "0.002"},
        ]);

        let (count, notional) = open_orders(&response).unwrap();

        assert_eq!(count, 2);
        assert_eq!(notional, Decimal::new(238, 0));
        assert_eq!(open_orders(&json!([])).unwrap(), (0, Decimal::ZERO));
    }

    #[test]
    fn a_missing_mid_price_is_an_error() {
        let response = json!({"BTC": "80000.0"});

        assert_eq!(
            mid_price(&response, "BTC").unwrap(),
            Decimal::new(80_000, 0)
        );
        assert!(mid_price(&response, "ETH").is_err());
    }
}
