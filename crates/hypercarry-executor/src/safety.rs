use crate::config::{RuntimeConfig, read_json};
use anyhow::{Context, Result, ensure};
use hypercarry_execution::{
    ExecutionError, OperationalHealth, OperationalHealthSource, RiskSnapshot, RiskSnapshotSource,
    ValidatedOrder,
};
use hypercarry_hyperliquid::{Clock, HyperliquidAssetResolver, SystemClock};
use hypercarry_mainnet_config::health::HealthEnvelope;
use reqwest::blocking::Client;
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::{
    io::Read,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct SafetySource(pub RuntimeConfig, pub i64);

impl SafetySource {
    fn envelope(&self) -> Result<HealthEnvelope> {
        let e: HealthEnvelope = read_json(&self.0.health_file)?;
        // Identity, digest and single-instant checks are part of the contract
        // and are enforced by the type both processes share.
        e.validate_binding(&self.0)?;
        ensure!(
            e.account_flat,
            "independent supervisor has not established a flat account across all DEXs"
        );
        Ok(e)
    }
}

impl OperationalHealthSource for SafetySource {
    fn health(&self) -> Result<OperationalHealth, ExecutionError> {
        let result = (|| -> Result<OperationalHealth> {
            let mut e = self.envelope()?;
            let start = Instant::now();
            check_account(&self.0, e.risk.account_equity)?;
            e.health.rest_latency_ms = u64::try_from(start.elapsed().as_millis())?;
            let now = SystemClock.now_ms()?;
            ensure!(
                e.health.observed_at_ms <= now && now - e.health.observed_at_ms <= self.1,
                "health became stale during live account checks"
            );
            Ok(e.health)
        })();
        result.map_err(|e| ExecutionError::Policy(e.to_string()))
    }
}

impl RiskSnapshotSource for SafetySource {
    fn snapshot(&self, _: &ValidatedOrder) -> Result<RiskSnapshot, ExecutionError> {
        let e = self
            .envelope()
            .map_err(|e| ExecutionError::Policy(e.to_string()))?;
        let now = SystemClock.now_ms()?;
        if e.risk.observed_at_ms > now
            || now - e.risk.observed_at_ms > self.0.risk.max_market_data_age_ms
        {
            return Err(ExecutionError::Policy(
                "risk snapshot is stale or future-dated".into(),
            ));
        }
        Ok(RiskSnapshot {
            observed_at_ms: now,
            ..e.risk
        })
    }
}

impl HyperliquidAssetResolver for SafetySource {
    fn asset_id(&self, order: &ValidatedOrder) -> Result<u32, ExecutionError> {
        verify_asset(&self.0, order).map_err(|e| ExecutionError::Metadata(e.to_string()))
    }
}

fn client() -> Result<Client> {
    Ok(Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()?)
}

fn info(client: &Client, request: &Value) -> Result<Value> {
    let response = client
        .post("https://api.hyperliquid.xyz/info")
        .json(request)
        .send()
        .context("mainnet account info transport failed")?
        .error_for_status()
        .context("mainnet account info status failed")?;
    let mut bytes = Vec::new();
    response.take(1_048_577).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1_048_576, "info response exceeds bound");
    Ok(serde_json::from_slice(&bytes)?)
}

fn check_account(config: &RuntimeConfig, required_equity: Decimal) -> Result<()> {
    let client = client()?;
    let role = info(
        &client,
        &json!({"type":"userRole","user":config.signer_address}),
    )?;
    ensure!(
        role["role"] == "agent"
            && role
                .pointer("/data/user")
                .and_then(Value::as_str)
                .is_some_and(|a| a.eq_ignore_ascii_case(&config.account_address)),
        "agent is not authorized for configured account"
    );
    let orders = info(
        &client,
        &json!({"type":"openOrders","user":config.account_address}),
    )?;
    ensure!(
        orders.as_array().is_some_and(Vec::is_empty),
        "canary requires zero default-DEX and spot open orders"
    );
    let state = info(
        &client,
        &json!({"type":"clearinghouseState","user":config.account_address}),
    )?;
    let positions = state["assetPositions"]
        .as_array()
        .context("missing account positions")?;
    for position in positions {
        ensure!(
            decimal(position.pointer("/position/szi"))? == Decimal::ZERO,
            "canary requires a flat default-perp account"
        );
    }
    let mode = info(
        &client,
        &json!({"type":"userAbstraction","user":config.account_address}),
    )?;
    let available = match mode.as_str() {
        Some("disabled") => decimal(state.get("withdrawable"))?,
        Some("unifiedAccount") => {
            let spot = info(
                &client,
                &json!({"type":"spotClearinghouseState","user":config.account_address}),
            )?;
            let pairs = spot["tokenToAvailableAfterMaintenance"]
                .as_array()
                .context("missing unified collateral")?;
            let usdc = pairs
                .iter()
                .find(|p| p[0].as_u64() == Some(0))
                .context("missing USDC")?;
            decimal(usdc.get(1))?
        }
        _ => anyhow::bail!("unsupported account abstraction mode"),
    };
    ensure!(
        required_equity > Decimal::ZERO && available >= required_equity,
        "live collateral below risk snapshot"
    );
    Ok(())
}

fn decimal(value: Option<&Value>) -> Result<Decimal> {
    value
        .and_then(Value::as_str)
        .context("missing decimal string")?
        .parse()
        .context("invalid decimal")
}

fn verify_asset(config: &RuntimeConfig, order: &ValidatedOrder) -> Result<u32> {
    ensure!(
        order.venue == "hyperliquid" && order.market == config.order.market,
        "asset identity mismatch"
    );
    let meta = info(&client()?, &json!({"type":"meta"}))?;
    let asset = meta["universe"]
        .as_array()
        .and_then(|u| u.get(config.asset_id as usize))
        .context("asset index missing")?;
    ensure!(
        asset["name"] == order.market
            && asset["szDecimals"].as_u64() == Some(u64::from(config.size_decimals)),
        "live metadata differs from reviewed mapping"
    );
    ensure!(
        !asset
            .get("isDelisted")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "asset is delisted"
    );
    validate_precision(order, config.size_decimals)?;
    Ok(config.asset_id)
}

fn validate_precision(order: &ValidatedOrder, size_decimals: u32) -> Result<()> {
    ensure!(
        size_decimals <= 6 && order.quantity.normalize().scale() <= size_decimals,
        "invalid size precision"
    );
    let price = order.limit_price.normalize();
    if price.scale() != 0 {
        ensure!(
            price.scale() <= 6 - size_decimals,
            "invalid price decimal places"
        );
        ensure!(
            price.mantissa().unsigned_abs().to_string().len() <= 5,
            "price exceeds five significant figures"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hypercarry_execution::{MarketMetadata, OrderIntent, Side};
    #[test]
    fn health_file_must_bind_account_config_flatness_and_fresh_risk() {
        let temp = tempfile::tempdir().unwrap();
        let mut runtime: RuntimeConfig = serde_json::from_str(include_str!(
            "../../../docs/mainnet-runtime-config.example.json"
        ))
        .unwrap();
        runtime.health_file = temp.path().join("health.json");
        let source = SafetySource(runtime.clone(), 1000);
        let mut envelope = json!({"schema_version":1,"network":"mainnet","account_address":runtime.account_address,
            "integration_config_digest":runtime.digest().unwrap(),"account_flat":true,
            "health":{"observed_at_ms":1,"startup_reconciliation":"not_ready","continuous_reconciliation":"not_ready",
                "unmanaged_orders":0,"unresolved_submissions":0,"rest_latency_ms":1,"private_latency_ms":1,
                "private_stream":"not_ready","alerts":"not_ready","audit_journal":"not_ready","rollback":"not_ready"},
            "risk":{"observed_at_ms":1,"market_data_at_ms":1,"reference_price":"100000","aggregate_notional":"0",
                "account_equity":"100","open_order_count":0,"recent_order_times_ms":[],"rolling_pnl":"0"}});
        let write = |value: &Value| {
            std::fs::write(&runtime.health_file, serde_json::to_vec(value).unwrap()).unwrap();
        };
        write(&envelope);
        assert!(source.envelope().is_ok()); // structurally valid, but not live readiness
        assert!(source.snapshot(&runtime.order).is_err()); // stale risk never reaches policy
        envelope["account_flat"] = false.into();
        write(&envelope);
        assert!(source.envelope().is_err());
        envelope["account_flat"] = true.into();
        envelope["network"] = "testnet".into();
        write(&envelope);
        assert!(source.envelope().is_err());
        envelope["network"] = "mainnet".into();
        envelope["integration_config_digest"] = "0".repeat(64).into();
        write(&envelope);
        assert!(source.envelope().is_err());
    }

    #[test]
    fn venue_precision_rejects_fractional_six_significant_digits() {
        let mut order = ValidatedOrder::resolve(
            &OrderIntent::limit(
                "precision",
                "hyperliquid",
                "BTC",
                Side::Buy,
                Decimal::ONE,
                "1234.5".parse().unwrap(),
                1,
            )
            .unwrap(),
            &MarketMetadata::new(
                "hyperliquid",
                "BTC",
                "0.01".parse().unwrap(),
                "0.001".parse().unwrap(),
                "0.001".parse().unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(validate_precision(&order, 3).is_ok());
        order.limit_price = "1234.56".parse().unwrap();
        assert!(validate_precision(&order, 3).is_err());
        order.limit_price = "123456".parse().unwrap();
        assert!(validate_precision(&order, 3).is_ok());
    }
}
