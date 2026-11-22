//! Credential-free account observations and isolated maintenance-margin math.
//! A batch is timestamped, not atomic. Native perps only; no all-DEX claim.
#![allow(missing_docs)]

use crate::info::{InfoClient, InfoRequest, InfoTransport, Network};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{error::Error, fmt, time::SystemTime};

#[derive(Debug)]
pub struct AccountError(pub String);
impl fmt::Display for AccountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl Error for AccountError {}
pub type Result<T> = std::result::Result<T, AccountError>;
pub(crate) fn fail(message: &str) -> AccountError {
    AccountError(message.into())
}
pub(crate) fn checked(value: Option<Decimal>) -> Result<Decimal> {
    value.ok_or_else(|| fail("decimal overflow or division by zero"))
}
/// Return the current Unix time in milliseconds.
///
/// # Errors
/// Fails for clocks before the epoch or timestamp overflow.
pub fn now_ms() -> Result<u64> {
    u64::try_from(
        SystemTime::UNIX_EPOCH
            .elapsed()
            .map_err(|_| fail("clock before epoch"))?
            .as_millis(),
    )
    .map_err(|_| fail("timestamp overflow"))
}
/// Validate a public EVM account address.
///
/// # Errors
/// Rejects malformed length, prefix or hexadecimal encoding.
pub fn validate_address(address: &str) -> Result<()> {
    if address.len() != 42
        || !address.starts_with("0x")
        || !address[2..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(fail("account must be a 0x-prefixed 20-byte address"));
    }
    Ok(())
}

/// Only supported public read requests can be constructed here.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum AccountRequest {
    #[serde(rename = "clearinghouseState")]
    Positions { user: String },
    #[serde(rename = "spotClearinghouseState")]
    Balances { user: String },
    #[serde(rename = "frontendOpenOrders")]
    Orders { user: String },
    #[serde(rename = "userFills")]
    Fills { user: String },
    #[serde(rename = "userFunding")]
    Funding {
        user: String,
        #[serde(rename = "startTime")]
        start_time: u64,
        #[serde(rename = "endTime")]
        end_time: u64,
    },
    #[serde(rename = "meta")]
    Meta,
    #[serde(rename = "metaAndAssetCtxs")]
    Contexts,
}
impl crate::info::private::Sealed for AccountRequest {}
impl InfoRequest for AccountRequest {
    type Response = Value;
    const TYPE: &'static str = "account_read";
    fn encode(&self) -> std::result::Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}
/// Decode an unsigned info response using the caller-selected account schema.
///
/// # Errors
/// Propagates transport and schema failures; missing data never becomes zero.
pub async fn read<T: InfoTransport, R: serde::de::DeserializeOwned>(
    client: &InfoClient<T>,
    request: AccountRequest,
) -> Result<R> {
    let value = client
        .execute(&request)
        .await
        .map_err(|e| AccountError(e.to_string()))?;
    serde_json::from_value(value)
        .map_err(|e| AccountError(format!("invalid account response: {e}")))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarginTier {
    #[serde(with = "rust_decimal::serde::str")]
    pub lower_bound: Decimal,
    pub max_leverage: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarginTable {
    pub margin_tiers: Vec<MarginTier>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Market {
    pub name: String,
    pub sz_decimals: u32,
    pub max_leverage: u32,
    pub margin_table_id: u32,
    #[serde(default)]
    pub is_delisted: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountMeta {
    pub universe: Vec<Market>,
    #[serde(default)]
    pub margin_tables: Vec<(u32, MarginTable)>,
}
impl AccountMeta {
    /// Resolve a native market and its maintenance tiers.
    ///
    /// # Errors
    /// Rejects unknown, delisted or malformed markets and missing tier tables.
    pub fn market(&self, coin: &str) -> Result<(u32, &Market, MarginTable)> {
        let (index, market) = self
            .universe
            .iter()
            .enumerate()
            .find(|(_, m)| m.name == coin)
            .ok_or_else(|| fail("market not found in native perpetual metadata"))?;
        if market.is_delisted || market.sz_decimals > 6 || market.max_leverage == 0 {
            return Err(fail("invalid or delisted market"));
        }
        let table = if market.margin_table_id < 50 {
            if market.margin_table_id == 0 {
                return Err(fail("invalid margin table"));
            }
            MarginTable {
                margin_tiers: vec![MarginTier {
                    lower_bound: Decimal::ZERO,
                    max_leverage: market.margin_table_id,
                }],
            }
        } else {
            self.margin_tables
                .iter()
                .find(|(id, _)| *id == market.margin_table_id)
                .ok_or_else(|| fail("margin table missing; refusing to guess maintenance"))?
                .1
                .clone()
        };
        table.validate()?;
        Ok((
            u32::try_from(index).map_err(|_| fail("asset index overflow"))?,
            market,
            table,
        ))
    }
}
impl MarginTable {
    /// Validate ordered maintenance tiers with decreasing maximum leverage.
    ///
    /// # Errors
    /// Rejects a missing zero tier, zero leverage, or non-monotonic tiers.
    pub fn validate(&self) -> Result<()> {
        if self
            .margin_tiers
            .first()
            .is_none_or(|t| t.lower_bound != Decimal::ZERO)
            || self.margin_tiers.iter().any(|t| t.max_leverage == 0)
            || self.margin_tiers.windows(2).any(|t| {
                t[0].lower_bound >= t[1].lower_bound || t[0].max_leverage < t[1].max_leverage
            })
        {
            return Err(fail("invalid maintenance margin tiers"));
        }
        Ok(())
    }
    /// Integrate each tier's rate: equivalent to value * rate - deduction.
    /// Integrate maintenance requirements across notional tiers.
    ///
    /// # Errors
    /// Rejects invalid tiers, negative notional and arithmetic overflow.
    pub fn maintenance(&self, notional: Decimal) -> Result<Decimal> {
        self.validate()?;
        if notional < Decimal::ZERO {
            return Err(fail("negative notional"));
        }
        let mut total = Decimal::ZERO;
        for (i, tier) in self.margin_tiers.iter().enumerate() {
            if notional <= tier.lower_bound {
                break;
            }
            let upper = self
                .margin_tiers
                .get(i + 1)
                .map_or(notional, |t| notional.min(t.lower_bound));
            let width = checked(upper.checked_sub(tier.lower_bound))?;
            let amount =
                checked(width.checked_div(Decimal::from(tier.max_leverage) * Decimal::TWO))?;
            total = checked(total.checked_add(amount))?;
        }
        Ok(total)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Leverage {
    Isolated {
        value: u32,
        #[serde(rename = "rawUsd", with = "rust_decimal::serde::str")]
        raw_usd: Decimal,
    },
    Cross {
        value: u32,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    pub coin: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub szi: Decimal,
    #[serde(with = "rust_decimal::serde::str_option")]
    pub entry_px: Option<Decimal>,
    pub leverage: Leverage,
    #[serde(with = "rust_decimal::serde::str")]
    pub position_value: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub unrealized_pnl: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub margin_used: Decimal,
    #[serde(with = "rust_decimal::serde::str_option")]
    pub liquidation_px: Option<Decimal>,
    pub cum_funding: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionEnvelope {
    pub position: Position,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionState {
    pub asset_positions: Vec<PositionEnvelope>,
    pub time: u64,
    pub cross_margin_summary: Value,
    #[serde(with = "rust_decimal::serde::str")]
    pub cross_maintenance_margin_used: Decimal,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Balance {
    pub coin: String,
    pub token: u32,
    #[serde(with = "rust_decimal::serde::str")]
    pub total: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub hold: Decimal,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotState {
    pub balances: Vec<Balance>,
    /// Null means availability was not reported, never implicitly zero.
    pub token_to_available_after_maintenance: Option<Vec<(u32, Decimal)>>,
}
impl SpotState {
    /// Read reported free Unified USDC without substituting the total balance.
    ///
    /// # Errors
    /// Rejects unavailable or ambiguous USDC availability.
    pub fn available_usdc(&self) -> Result<Decimal> {
        let values = self
            .token_to_available_after_maintenance
            .as_ref()
            .ok_or_else(|| fail("Unified availability missing"))?;
        let matches: Vec<_> = values.iter().filter(|(id, _)| *id == 0).collect();
        if matches.len() != 1 {
            return Err(fail("expected exactly one USDC availability value"));
        }
        Ok(matches[0].1)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenOrder {
    pub coin: String,
    pub side: String,
    pub oid: u64,
    #[serde(with = "rust_decimal::serde::str")]
    pub sz: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub limit_px: Decimal,
    pub is_trigger: bool,
    #[serde(with = "rust_decimal::serde::str")]
    pub trigger_px: Decimal,
    pub reduce_only: bool,
    pub order_type: String,
    pub is_position_tpsl: bool,
    /// Client correlation and timing are evidence, not signing authority.
    #[serde(default)]
    pub cloid: Option<String>,
    #[serde(default)]
    pub timestamp: Option<u64>,
    #[serde(default)]
    pub trigger_condition: Option<String>,
    /// Pending children are retained but never counted as independent open exits.
    #[serde(default)]
    pub children: Vec<Value>,
}
#[derive(Debug, Clone, Serialize)]
pub struct PositionHealth {
    #[serde(with = "rust_decimal::serde::str")]
    pub mark_price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub isolated_equity: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub maintenance_margin: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub equity_over_maintenance: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub excess_equity: Decimal,
}
impl Position {
    /// Derived ratio, not a protocol field. Cross positions have no isolated HF.
    /// # Errors
    /// Rejects invalid position value, maintenance tables and arithmetic overflow.
    pub fn health(&self, table: &MarginTable) -> Result<Option<PositionHealth>> {
        if self.szi.is_zero() {
            return Ok(None);
        }
        if self.position_value <= Decimal::ZERO {
            return Err(fail("nonzero position has invalid value"));
        }
        let mark = checked(self.position_value.checked_div(self.szi.abs()))?;
        self.health_at(mark, table)
    }

    /// Calculate a price scenario holding collateral, quantity and funding fixed.
    ///
    /// # Errors
    /// Rejects nonpositive prices, invalid tiers and arithmetic overflow.
    pub fn health_at(&self, mark: Decimal, table: &MarginTable) -> Result<Option<PositionHealth>> {
        if mark <= Decimal::ZERO {
            return Err(fail("scenario price must be positive"));
        }
        if self.szi.is_zero() {
            return Ok(None);
        }
        let Leverage::Isolated { raw_usd, .. } = self.leverage else {
            return Ok(None);
        };
        let equity = checked(raw_usd.checked_add(checked(self.szi.checked_mul(mark))?))?;
        let maintenance = table.maintenance(checked(self.szi.abs().checked_mul(mark))?)?;
        Ok(Some(PositionHealth {
            mark_price: mark,
            isolated_equity: equity,
            maintenance_margin: maintenance,
            equity_over_maintenance: checked(equity.checked_div(maintenance))?,
            excess_equity: checked(equity.checked_sub(maintenance))?,
        }))
    }
}

#[derive(Debug, Serialize)]
pub struct AccountSnapshot {
    pub schema_version: u32,
    pub network: Network,
    pub account: String,
    pub scope: String,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub positions: PositionState,
    pub balances: SpotState,
    pub orders: Vec<OpenOrder>,
    pub health: Vec<(String, Option<PositionHealth>)>,
}
/// Collect sequential, credential-free account observations.
///
/// # Errors
/// Rejects invalid accounts, unavailable observations and invalid margin metadata.
pub async fn snapshot<T: InfoTransport>(
    client: &InfoClient<T>,
    address: &str,
) -> Result<AccountSnapshot> {
    validate_address(address)?;
    let start = now_ms()?;
    let positions: PositionState = read(
        client,
        AccountRequest::Positions {
            user: address.into(),
        },
    )
    .await?;
    let balances = read(
        client,
        AccountRequest::Balances {
            user: address.into(),
        },
    )
    .await?;
    let orders = read(
        client,
        AccountRequest::Orders {
            user: address.into(),
        },
    )
    .await?;
    let meta: AccountMeta = read(client, AccountRequest::Meta).await?;
    let mut health = Vec::new();
    for envelope in &positions.asset_positions {
        let p = &envelope.position;
        let (_, _, table) = meta.market(&p.coin)?;
        health.push((p.coin.clone(), p.health(&table)?));
    }
    Ok(AccountSnapshot {
        schema_version: 1,
        network: client.network(),
        account: address.into(),
        scope: "native_perps_and_shared_spot; sequential_not_atomic".into(),
        started_at_ms: start,
        completed_at_ms: now_ms()?,
        positions,
        balances,
        orders,
        health,
    })
}

/// Bounded funding pagination; a full stalled timestamp fails, never claims completeness.
/// # Errors
/// Fails on malformed rows, conflicting duplicates, stalled pagination or transport errors.
pub async fn funding<T: InfoTransport>(
    client: &InfoClient<T>,
    address: &str,
    start: u64,
    end: u64,
) -> Result<Vec<Value>> {
    validate_address(address)?;
    if start > end {
        return Err(fail("reversed funding window"));
    }
    let mut cursor = start;
    let mut rows = std::collections::BTreeMap::new();
    for _ in 0..100 {
        let page: Vec<Value> = read(
            client,
            AccountRequest::Funding {
                user: address.into(),
                start_time: cursor,
                end_time: end,
            },
        )
        .await?;
        let mut last = cursor;
        for row in &page {
            let time = row["time"]
                .as_u64()
                .ok_or_else(|| fail("funding timestamp missing"))?;
            if time < cursor || time > end {
                return Err(fail("funding row outside requested window"));
            }
            last = last.max(time);
            let key = (
                time,
                row["hash"]
                    .as_str()
                    .ok_or_else(|| fail("funding identity missing"))?
                    .to_owned(),
                row["delta"]["coin"]
                    .as_str()
                    .ok_or_else(|| fail("funding coin missing"))?
                    .to_owned(),
            );
            if let Some(previous) = rows.insert(key, row.clone())
                && previous != *row
            {
                return Err(fail("conflicting funding records"));
            }
        }
        if page.len() < 500 {
            return Ok(rows.into_values().collect());
        }
        if last == cursor {
            return Err(fail("funding pagination stalled; history incomplete"));
        }
        cursor = last;
    }
    Err(fail("funding pagination budget exhausted"))
}

pub fn recent_fills_envelope(rows: &[Value]) -> Value {
    let possibly_truncated = rows.len() >= 2000;
    json!({"scope":"most_recent_fills_not_full_history","possibly_truncated":possibly_truncated,"fills":rows})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }
    fn table() -> MarginTable {
        MarginTable {
            margin_tiers: vec![
                MarginTier {
                    lower_bound: Decimal::ZERO,
                    max_leverage: 20,
                },
                MarginTier {
                    lower_bound: d("1000"),
                    max_leverage: 10,
                },
            ],
        }
    }
    #[test]
    fn maintenance_is_continuous_across_tiers() {
        assert_eq!(table().maintenance(d("1000")).unwrap(), d("25"));
        assert_eq!(table().maintenance(d("1200")).unwrap(), d("35"));
        assert!(table().maintenance(d("-1")).is_err());
    }
    #[test]
    fn unknown_table_and_missing_positions_are_not_zero_risk() {
        let m:AccountMeta=serde_json::from_value(json!({"universe":[{"name":"EXAMPLE","szDecimals":2,"maxLeverage":20,"marginTableId":54}]})).unwrap();
        assert!(m.market("EXAMPLE").is_err());
        assert!(serde_json::from_value::<PositionState>(json!({})).is_err());
    }
    #[test]
    fn isolated_equity_includes_signed_exposure() {
        let p:Position=serde_json::from_value(json!({"coin":"EXAMPLE","szi":"-2","entryPx":"50","leverage":{"type":"isolated","value":1,"rawUsd":"200"},"positionValue":"120","unrealizedPnl":"-20","marginUsed":"80","liquidationPx":"97.56","cumFunding":{}})).unwrap();
        let h = p.health(&table()).unwrap().unwrap();
        assert_eq!(h.isolated_equity, d("80"));
        assert_eq!(h.maintenance_margin, d("3"));
        assert_eq!(h.excess_equity, d("77"));
    }
    #[test]
    fn availability_never_uses_total_as_free_collateral() {
        let s:SpotState=serde_json::from_value(json!({"balances":[{"coin":"USDC","token":0,"total":"100","hold":"80"}],"tokenToAvailableAfterMaintenance":[[0,"20"]]})).unwrap();
        assert_eq!(s.available_usdc().unwrap(), d("20"));
        let missing: SpotState = serde_json::from_value(json!({"balances":[]})).unwrap();
        assert!(missing.available_usdc().is_err());
    }

    struct Pages(std::sync::Mutex<std::collections::VecDeque<Vec<Value>>>);
    impl InfoTransport for Pages {
        fn network(&self) -> Network {
            Network::Testnet
        }
        // In-memory fixture implements the production async transport contract.
        #[allow(clippy::unused_async_trait_impl)]
        async fn post(&self, body: &[u8]) -> std::result::Result<Vec<u8>, crate::info::BoxError> {
            let request: Value = serde_json::from_slice(body)?;
            assert_eq!(request["type"], "userFunding");
            Ok(serde_json::to_vec(
                &self
                    .0
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("unexpected request"),
            )?)
        }
    }
    fn funding_row(time: u64) -> Value {
        json!({"time":time,"hash":format!("fixture-{time}"),"delta":{"coin":"EXAMPLE","usdc":"-0.01"}})
    }
    #[tokio::test]
    async fn funding_pagination_deduplicates_inclusive_boundary() {
        let c = InfoClient::new(Pages(std::sync::Mutex::new(
            std::collections::VecDeque::from([
                (1..=500).map(funding_row).collect(),
                vec![funding_row(500), funding_row(501)],
            ]),
        )));
        let rows = funding(&c, &format!("0x{}", "12".repeat(20)), 1, 1000)
            .await
            .unwrap();
        assert_eq!(rows.len(), 501);
        assert_eq!(rows.last().unwrap()["time"], 501);
    }
    #[tokio::test]
    async fn funding_stall_and_conflicting_duplicates_are_errors() {
        let address = format!("0x{}", "12".repeat(20));
        let c = InfoClient::new(Pages(std::sync::Mutex::new(
            std::collections::VecDeque::from([vec![funding_row(1); 500]]),
        )));
        assert!(funding(&c, &address, 1, 1000).await.is_err());
        let mut conflict = funding_row(1);
        conflict["delta"]["usdc"] = json!("100");
        let c = InfoClient::new(Pages(std::sync::Mutex::new(
            std::collections::VecDeque::from([vec![funding_row(1), conflict]]),
        )));
        assert!(funding(&c, &address, 1, 1000).await.is_err());
    }
}
