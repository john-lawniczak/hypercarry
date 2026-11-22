//! Unsigned, discovered all-perp-DEX observations. Never a trading authorization.
//! Balances remain separate from per-DEX margin; there is no inferred equity sum.
#![allow(missing_docs)]

use crate::{
    account::{
        AccountError, OpenOrder, Position, PositionState, Result, SpotState, checked, fail, now_ms,
        validate_address,
    },
    info::{InfoClient, InfoRequest, InfoTransport, Network},
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

pub(crate) mod stream;

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum Request<'a> {
    #[serde(rename = "perpDexs")]
    Dexs,
    #[serde(rename = "spotClearinghouseState")]
    Balances { user: &'a str },
    #[serde(rename = "clearinghouseState")]
    Positions {
        user: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        dex: Option<&'a str>,
    },
    #[serde(rename = "frontendOpenOrders")]
    Orders {
        user: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        dex: Option<&'a str>,
    },
}
impl crate::info::private::Sealed for Request<'_> {}
impl InfoRequest for Request<'_> {
    type Response = Value;
    const TYPE: &'static str = "portfolio_read";
    fn encode(&self) -> std::result::Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}
async fn read<T: InfoTransport, R: serde::de::DeserializeOwned>(
    c: &InfoClient<T>,
    request: Request<'_>,
) -> Result<R> {
    let value = c
        .execute(&request)
        .await
        .map_err(|e| AccountError(e.to_string()))?;
    serde_json::from_value(value)
        .map_err(|e| AccountError(format!("invalid portfolio response: {e}")))
}

#[derive(Deserialize)]
struct Dex {
    name: String,
}
fn dex_names(entries: &[Option<Dex>]) -> Result<Vec<Option<String>>> {
    if entries.first().is_none_or(Option::is_some) || entries.len() > 512 {
        return Err(fail(
            "missing native DEX or discovery exceeds 512-DEX budget",
        ));
    }
    let mut seen = BTreeSet::new();
    let mut result = vec![None];
    for entry in &entries[1..] {
        let name = &entry
            .as_ref()
            .ok_or_else(|| fail("unexpected null builder DEX"))?
            .name;
        if name.is_empty()
            || name.len() > 64
            || name.chars().any(char::is_control)
            || !seen.insert(name)
        {
            return Err(fail("empty or duplicate builder DEX"));
        }
        result.push(Some(name.clone()));
    }
    Ok(result)
}

#[derive(Debug, Serialize)]
pub struct ExitCoverage {
    pub coin: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    /// Only single orders with enough reported size count; split exits are not summed.
    pub stop_order_ids: Vec<u64>,
    pub take_order_ids: Vec<u64>,
}
impl ExitCoverage {
    pub fn full_pair_observed(&self) -> bool {
        !self.stop_order_ids.is_empty() && !self.take_order_ids.is_empty()
    }
}

/// Conservative evidence of exit coverage, not a guarantee of protection.
/// Zero-size dynamic position TP/SL, unknown order kinds and already-crossed
/// triggers do not count. A partial-size exit never becomes full coverage.
///
/// # Errors
/// Rejects zero size, nonpositive position value and decimal overflow.
pub fn exit_coverage(p: &Position, orders: &[OpenOrder]) -> Result<ExitCoverage> {
    if p.szi.is_zero() || p.position_value <= Decimal::ZERO {
        return Err(fail("exit coverage requires a nonzero valid position"));
    }
    let mark = checked(p.position_value.checked_div(p.szi.abs()))?;
    let short = p.szi < Decimal::ZERO;
    let mut result = ExitCoverage {
        coin: p.coin.clone(),
        quantity: p.szi.abs(),
        stop_order_ids: vec![],
        take_order_ids: vec![],
    };
    for o in orders.iter().filter(|o| o.coin == p.coin) {
        if !o.reduce_only
            || !o.is_trigger
            || o.side != if short { "B" } else { "A" }
            || o.sz < p.szi.abs()
            || o.trigger_px <= Decimal::ZERO
            || o.limit_px <= Decimal::ZERO
        {
            continue;
        }
        // A bound on the wrong side of the trigger is not immediately marketable
        // even at the trigger. Both market and limit exits can still fail later.
        if (short && o.limit_px < o.trigger_px) || (!short && o.limit_px > o.trigger_px) {
            continue;
        }
        let stop_side = if short {
            o.trigger_px > mark
        } else {
            o.trigger_px < mark
        };
        match o.order_type.as_str() {
            "Stop Market" | "Stop Limit" if stop_side => result.stop_order_ids.push(o.oid),
            "Take Profit Market" | "Take Profit Limit" if !stop_side && o.trigger_px != mark => {
                result.take_order_ids.push(o.oid);
            }
            _ => {}
        }
    }
    Ok(result)
}

#[derive(Debug, Serialize)]
pub struct DexSnapshot {
    /// Empty string means native; builder names match discovery exactly.
    pub dex: String,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    /// Pre-order position read; financial identity must match the post-order read.
    pub positions_before: PositionState,
    pub positions: PositionState,
    /// Includes spot orders when the venue returns them with the native DEX.
    pub orders: Vec<OpenOrder>,
    pub exits: Vec<ExitCoverage>,
    pub orphan_trigger_ids: Vec<u64>,
}
#[derive(Debug, Serialize)]
pub struct PortfolioSnapshot {
    pub schema_version: u32,
    pub network: Network,
    pub account: String,
    pub scope: String,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub balances: SpotState,
    pub dexs: Vec<DexSnapshot>,
}
impl PortfolioSnapshot {
    pub fn exit_coverage_observed(&self) -> bool {
        self.dexs.iter().all(|d| {
            d.orphan_trigger_ids.is_empty() && d.exits.iter().all(ExitCoverage::full_pair_observed)
        })
    }
}

/// Read all discovered DEXs or return an error; missing state never means flat.
/// No credentials, writes, repairs, equity aggregation or margin transfers.
///
/// # Errors
/// Rejects incomplete discovery/state, duplicate identities, stale active positions,
/// changed DEX discovery and observations taking longer than two minutes.
pub async fn snapshot<T: InfoTransport>(
    client: &InfoClient<T>,
    address: &str,
) -> Result<PortfolioSnapshot> {
    with_deadline(client, address, Duration::from_secs(120)).await
}

async fn with_deadline<T: InfoTransport>(
    client: &InfoClient<T>,
    address: &str,
    budget: Duration,
) -> Result<PortfolioSnapshot> {
    validate_address(address)?;
    tokio::time::timeout(budget, snapshot_inner(client, address))
        .await
        .map_err(|_| fail("portfolio observation deadline exceeded; retry read"))?
}

fn scoped_coin(coin: &str, dex: Option<&str>) -> bool {
    !coin.is_empty()
        && !coin.chars().any(char::is_control)
        && match dex {
            Some(name) => coin
                .strip_prefix(name)
                .is_some_and(|suffix| suffix.starts_with(':') && suffix.len() > 1),
            None => !coin.contains(':'),
        }
}

fn identities<'a>(
    state: &'a PositionState,
    dex: Option<&str>,
) -> Result<BTreeMap<&'a str, &'a Position>> {
    let mut result = BTreeMap::new();
    for p in state.asset_positions.iter().map(|e| &e.position) {
        if !scoped_coin(&p.coin, dex) || result.insert(p.coin.as_str(), p).is_some() {
            return Err(fail("duplicate or incorrectly scoped position"));
        }
    }
    Ok(result)
}

fn stable_positions(
    before: &PositionState,
    after: &PositionState,
    dex: Option<&str>,
) -> Result<()> {
    let before = identities(before, dex)?;
    let after = identities(after, dex)?;
    if before.len() != after.len()
        || before.iter().any(|(coin, p)| {
            after.get(coin).is_none_or(|q| {
                p.szi != q.szi || p.entry_px != q.entry_px || p.leverage != q.leverage
            })
        })
    {
        return Err(fail(
            "position quantity, entry or margin changed while reading exits; retry read",
        ));
    }
    Ok(())
}

async fn read_dex<T: InfoTransport>(
    client: &InfoClient<T>,
    address: &str,
    name: Option<&str>,
) -> Result<DexSnapshot> {
    let start = now_ms()?;
    let positions_before: PositionState = read(
        client,
        Request::Positions {
            user: address,
            dex: name,
        },
    )
    .await?;
    let orders: Vec<OpenOrder> = read(
        client,
        Request::Orders {
            user: address,
            dex: name,
        },
    )
    .await?;
    let positions: PositionState = read(
        client,
        Request::Positions {
            user: address,
            dex: name,
        },
    )
    .await?;
    assemble_dex(name, start, now_ms()?, positions_before, positions, orders)
}

fn assemble_dex(
    name: Option<&str>,
    start: u64,
    end: u64,
    positions_before: PositionState,
    positions: PositionState,
    orders: Vec<OpenOrder>,
) -> Result<DexSnapshot> {
    stable_positions(&positions_before, &positions, name)?;
    let mut ids = BTreeSet::new();
    for order in &orders {
        if !ids.insert(order.oid) || !scoped_coin(&order.coin, name) {
            return Err(fail("duplicate or incorrectly scoped open order"));
        }
    }
    let exits: Vec<_> = positions
        .asset_positions
        .iter()
        .map(|p| &p.position)
        .filter(|p| !p.szi.is_zero())
        .map(|p| exit_coverage(p, &orders))
        .collect::<Result<_>>()?;
    let orphan_trigger_ids = orders
        .iter()
        .filter(|o| o.is_trigger && !exits.iter().any(|e| e.coin == o.coin))
        .map(|o| o.oid)
        .collect();
    Ok(DexSnapshot {
        dex: name.unwrap_or_default().into(),
        started_at_ms: start,
        completed_at_ms: end,
        positions_before,
        positions,
        orders,
        exits,
        orphan_trigger_ids,
    })
}

async fn snapshot_inner<T: InfoTransport>(
    client: &InfoClient<T>,
    address: &str,
) -> Result<PortfolioSnapshot> {
    let started_at_ms = now_ms()?;
    let names = dex_names(&read::<_, Vec<Option<Dex>>>(client, Request::Dexs).await?)?;
    let balances = read(client, Request::Balances { user: address }).await?;
    let mut dexs = Vec::new();
    if names.len() > 20 {
        dexs = streamed_dexs(client, address, &names).await?;
    } else {
        for name in &names {
            dexs.push(read_dex(client, address, name.as_deref()).await?);
            check_duration(started_at_ms, now_ms()?)?;
        }
    }
    let final_names = dex_names(&read::<_, Vec<Option<Dex>>>(client, Request::Dexs).await?)?;
    if names != final_names {
        return Err(fail("DEX discovery changed during observation; retry read"));
    }
    let completed_at_ms = now_ms()?;
    check_duration(started_at_ms, completed_at_ms)?;
    for dex in &dexs {
        for state in [&dex.positions_before, &dex.positions] {
            if state.time > completed_at_ms.saturating_add(5_000)
                || completed_at_ms.saturating_sub(state.time) > 60_000
            {
                return Err(fail(
                    "position timestamp (including flat accounts) is stale or in the future; retry read",
                ));
            }
        }
    }
    Ok(PortfolioSnapshot {
        schema_version: 2,
        network: client.network(),
        account: address.into(),
        scope: format!(
            "all_discovered_perp_dexs_and_shared_spot; position_reads_bracket_orders; sequential_not_atomic; no_equity_aggregation; {}",
            if names.len() > 20 {
                "fresh_stream_bootstrap_and_active_rest"
            } else {
                "all_dex_rest"
            }
        ),
        started_at_ms,
        completed_at_ms,
        balances,
        dexs,
    })
}

#[derive(Deserialize)]
struct StreamDex {
    dex: String,
    before: PositionState,
    state: PositionState,
    // Only empty streamed order sets are used directly. Active DEXs require
    // frontend REST orders with the full trigger/protection schema.
    orders: Vec<Value>,
}

async fn streamed_dexs<T: InfoTransport>(
    client: &InfoClient<T>,
    address: &str,
    names: &[Option<String>],
) -> Result<Vec<DexSnapshot>> {
    let start = now_ms()?;
    let expected: Vec<_> = names
        .iter()
        .map(|n| n.clone().unwrap_or_default())
        .collect();
    let raw = client
        .transport()
        .portfolio_scope(address, &expected)
        .await
        .map_err(|e| AccountError(e.to_string()))?;
    let rows: Vec<StreamDex> = serde_json::from_value(raw)
        .map_err(|e| AccountError(format!("invalid portfolio stream: {e}")))?;
    let end = now_ms()?;
    let mut seen = BTreeSet::new();
    if rows.len() != expected.len() {
        return Err(fail("incomplete portfolio stream scope"));
    }
    let mut result = BTreeMap::new();
    for row in rows {
        if !expected.contains(&row.dex) || !seen.insert(row.dex.clone()) {
            return Err(fail("unexpected or duplicate portfolio stream DEX"));
        }
        let name = if row.dex.is_empty() {
            None
        } else {
            Some(row.dex.as_str())
        };
        stable_positions(&row.before, &row.state, name)?;
        for state in [&row.before, &row.state] {
            if state.time < start.saturating_sub(5_000) || state.time > end.saturating_add(1_000) {
                return Err(fail("stale portfolio stream state"));
            }
        }
        let ids = stream::order_ids(&row.orders, name)?;
        let dex = if name.is_none()
            || !row.orders.is_empty()
            || row
                .state
                .asset_positions
                .iter()
                .any(|p| !p.position.szi.is_zero())
        {
            let rest = read_dex(client, address, name).await?;
            stable_positions(&row.state, &rest.positions_before, name)?;
            let rest_ids: BTreeSet<_> = rest.orders.iter().map(|o| o.oid).collect();
            if ids != rest_ids {
                return Err(fail("stream/REST open orders differ; retry read"));
            }
            rest
        } else {
            assemble_dex(name, start, end, row.before, row.state, vec![])?
        };
        result.insert(row.dex, dex);
    }
    expected
        .iter()
        .map(|name| result.remove(name).ok_or_else(|| fail("missing DEX")))
        .collect()
}

fn check_duration(start: u64, end: u64) -> Result<()> {
    if end < start || end - start > 120_000 {
        return Err(fail(
            "portfolio observation clock reversed or exceeded two minutes; retry read",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
