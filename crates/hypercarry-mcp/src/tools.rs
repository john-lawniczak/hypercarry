use crate::{
    config::{Config, Scope, read_json},
    process,
};
use anyhow::{Context, Result, bail, ensure};
use hypercarry_storage::dataset::SettledFundingDataset;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::File,
    sync::atomic::AtomicBool,
    time::{SystemTime, UNIX_EPOCH},
};

pub const CONFIDENCE: &str = "Prediction confidence is data completeness (coverage multiplied by elapsed hour fraction), not probability of profit or statistical confidence.";

pub fn now_ms() -> Result<i64> {
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
        .context("clock overflow")
}

#[allow(clippy::needless_pass_by_value)] // JSON constructors own their inputs.
fn schema(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
#[allow(clippy::needless_pass_by_value)] // JSON constructors own their inputs.
fn tool(name: &str, description: &str, input: Value, write: bool, external: bool) -> Value {
    json!({"name":name,"description":description,"inputSchema":input,
        "outputSchema":{"type":"object","required":["schema_version","network","observed_at_ms","units","coverage","valuation_assumptions","provenance"],"properties":{"schema_version":{"type":"integer"},"network":{"type":"string"},"observed_at_ms":{"type":["integer","null"]},"units":{"type":"object"},"coverage":{"type":"object"},"valuation_assumptions":{"type":"array"},"provenance":{"type":"object"}}},
        "annotations":{"readOnlyHint":!write,"destructiveHint":write,"idempotentHint":!write,"openWorldHint":external}})
}

pub fn list(scopes: &BTreeSet<Scope>) -> Vec<Value> {
    let coin = json!({"coin":{"type":"string"}});
    let mut tools = Vec::new();
    if scopes.contains(&Scope::Read) {
        tools.push(tool(
            "get_funding_snapshot",
            "Live public funding snapshot; no wallet access.",
            schema(coin.clone(), &["coin"]),
            false,
            true,
        ));
        tools.push(tool(
            "get_funding_apr",
            "Latest stored settlement annualized; this is not a forecast.",
            schema(coin.clone(), &["coin"]),
            false,
            false,
        ));
        tools.push(tool(
            "get_dataset_health",
            "Stored funding freshness and contiguous history.",
            schema(coin, &["coin"]),
            false,
            false,
        ));
        tools.push(tool("get_funding_history","Stored funding, inclusive time range, at most 1000 rows per page; no implicit backfill.",schema(json!({"coin":{"type":"string"},"start_ms":{"type":"integer","minimum":0},"end_ms":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":1000}}), &["coin","start_ms","end_ms"]),false,false));
        tools.push(tool("evaluate_carry_trade","Evaluate an inline trade document OR a saved trade_id. Funding valuation assumptions are explicit.",schema(json!({"coin":{"type":"string"},"trade":{"type":"object"},"trade_id":{"type":"string"}}), &["coin"]),false,false));
        tools.push(tool("get_prediction",CONFIDENCE,schema(json!({"coin":{"type":"string"},"capture_id":{"type":"string"},"settlement_ms":{"type":"integer"},"as_of_ms":{"type":"integer"}}), &["coin","capture_id","settlement_ms","as_of_ms"]),false,false));
    }
    if scopes.contains(&Scope::Write) {
        tools.push(tool("backfill_funding","Fetch settled funding and update the configured dataset. Does not trade.",schema(json!({"coin":{"type":"string"},"days":{"type":"integer","minimum":1,"maximum":365}}), &["coin","days"]),true,true));
        tools.push(tool("save_trade","Validate and persist a new named trade document; never overwrites an existing record.",schema(json!({"coin":{"type":"string"},"trade_id":{"type":"string"},"trade":{"type":"object"}}), &["coin","trade_id","trade"]),true,false));
    }
    if scopes.contains(&Scope::Trade) {
        for (name, description) in [
            (
                "submit_reviewed_canary",
                "Consume a one-use operator approval for the exact configured mainnet canary. Cannot change account, market, side, price or size. Ambiguous outcomes require recovery, never blind retry.",
            ),
            (
                "recover_reviewed_canary",
                "Consume a recovery approval to cancel/reconcile the existing reviewed journal. Does not place a replacement entry; fills require watchdog/operator recovery.",
            ),
        ] {
            tools.push(tool(
                name,
                description,
                schema(json!({"approval_id":{"type":"string"}}), &["approval_id"]),
                true,
                true,
            ));
        }
    }
    tools
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Coin {
    coin: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct History {
    coin: String,
    start_ms: i64,
    end_ms: i64,
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    100
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TradeArgs {
    coin: String,
    #[serde(default)]
    trade: Option<Value>,
    #[serde(default)]
    trade_id: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedTrade {
    schema_version: u32,
    network: hypercarry_core::info::Network,
    coin: String,
    trade: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prediction {
    coin: String,
    capture_id: String,
    settlement_ms: i64,
    as_of_ms: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Backfill {
    coin: String,
    days: u32,
}

fn cli(
    config: &Config,
    command: &str,
    coin: &str,
    extra: &[String],
    stop: &AtomicBool,
) -> Result<Value> {
    config.coin(coin)?;
    let mut args = vec![
        command.into(),
        "--network".into(),
        config.network.to_string(),
        "--coin".into(),
        coin.into(),
        "--output".into(),
        "json".into(),
    ];
    if command != "snapshot" {
        args.extend([
            "--dataset".into(),
            config.dataset.to_string_lossy().into_owned(),
        ]);
    }
    args.extend_from_slice(extra);
    process::run(
        &config.analytics_bin,
        &config.analytics_sha256,
        &args,
        config.timeout_ms,
        stop,
    )
}

pub fn call(
    config: &Config,
    scopes: &BTreeSet<Scope>,
    name: &str,
    args: Value,
    stop: &AtomicBool,
) -> Result<Value> {
    ensure!(
        list(scopes).iter().any(|tool| tool["name"] == name),
        "tool is unknown or outside the granted scope"
    );
    match name {
        "get_funding_snapshot" | "get_funding_apr" | "get_dataset_health" => {
            let args: Coin = serde_json::from_value(args)?;
            cli(
                config,
                if name == "get_funding_snapshot" {
                    "snapshot"
                } else {
                    "apr"
                },
                &args.coin,
                &[],
                stop,
            )
        }
        "get_funding_history" => history(config, &serde_json::from_value(args)?),
        "evaluate_carry_trade" => evaluate(config, serde_json::from_value(args)?, stop, false),
        "save_trade" => evaluate(config, serde_json::from_value(args)?, stop, true),
        "get_prediction" => {
            let args: Prediction = serde_json::from_value(args)?;
            let capture = config
                .captures
                .get(&args.capture_id)
                .context("capture is not registered")?;
            ensure!(
                std::fs::symlink_metadata(capture)?.file_type().is_file(),
                "capture must remain a regular file"
            );
            cli(
                config,
                "predict",
                &args.coin,
                &[
                    "--capture".into(),
                    capture.to_string_lossy().into_owned(),
                    "--settlement-ms".into(),
                    args.settlement_ms.to_string(),
                    "--as-of-ms".into(),
                    args.as_of_ms.to_string(),
                ],
                stop,
            )
        }
        "backfill_funding" => {
            let args: Backfill = serde_json::from_value(args)?;
            ensure!((1..=365).contains(&args.days), "days must be 1..365");
            cli(
                config,
                "backfill",
                &args.coin,
                &["--days".into(), args.days.to_string()],
                stop,
            )
        }
        "submit_reviewed_canary" | "recover_reviewed_canary" => {
            trade(config, name, &serde_json::from_value(args)?, stop)
        }
        _ => bail!("unsupported tool"),
    }
}

fn evaluate(config: &Config, args: TradeArgs, stop: &AtomicBool, save: bool) -> Result<Value> {
    config.coin(&args.coin)?;
    let trade = match (args.trade, args.trade_id.as_deref(), save) {
        (Some(trade), None, false) | (Some(trade), Some(_), true) => trade,
        (None, Some(id), false) => {
            let saved: SavedTrade = read_json(&config.trade_path(id)?)?;
            ensure!(
                saved.schema_version == 1
                    && saved.network == config.network
                    && saved.coin == args.coin,
                "saved trade identity mismatch"
            );
            saved.trade
        }
        _ => bail!(
            "provide either inline trade or trade_id for evaluation; both are required for saving"
        ),
    };
    let mut temporary = tempfile::NamedTempFile::new()?;
    serde_json::to_writer(&mut temporary, &trade)?;
    let mut result = cli(
        config,
        "pnl",
        &args.coin,
        &[
            "--trade".into(),
            temporary.path().to_string_lossy().into_owned(),
        ],
        stop,
    )?;
    result["trade_document_sha256"] = json!(process::hash(&serde_json::to_vec(&trade)?));
    if save {
        let path = config.trade_path(args.trade_id.as_deref().context("trade ID required")?)?;
        ensure!(
            std::fs::canonicalize(&config.trade_directory)? == config.trade_directory,
            "trade directory changed"
        );
        let saved = SavedTrade {
            schema_version: 1,
            network: config.network,
            coin: args.coin,
            trade,
        };
        let mut file = tempfile::NamedTempFile::new_in(&config.trade_directory)?;
        serde_json::to_writer(&mut file, &saved)?;
        file.as_file().sync_all()?;
        file.persist_noclobber(path)?;
        File::open(&config.trade_directory)?.sync_all()?;
        result["saved_trade_id"] = json!(args.trade_id);
    }
    Ok(result)
}

fn history(config: &Config, args: &History) -> Result<Value> {
    config.coin(&args.coin)?;
    ensure!(
        args.start_ms >= 0 && args.end_ms >= args.start_ms && (1..=1000).contains(&args.limit),
        "invalid history bounds"
    );
    let dataset = SettledFundingDataset::new(&config.dataset);
    let rows = dataset.stream_records(config.network, "hyperliquid", &args.coin)?;
    let matching: Vec<_> = rows
        .iter()
        .filter(|r| {
            let t = r.identity.settlement_time.as_i64();
            t >= args.start_ms && t <= args.end_ms
        })
        .collect();
    let more = matching.len() > args.limit;
    let selected = &matching[..matching.len().min(args.limit)];
    let records:Vec<_> = selected.iter().map(|r| json!({"settlement_time_ms":r.identity.settlement_time.as_i64(),"funding_rate":r.funding_rate.normalize().to_string(),"premium":r.premium.normalize().to_string(),
        "provenance":{"ingested_at_ms":r.provenance.ingestion_time.as_i64(),"request_start_ms":r.provenance.request_window.start().as_i64(),"request_end_ms":r.provenance.request_window.end().as_i64(),"software_version":r.provenance.software_version,"source_endpoint_class":format!("{:?}",r.provenance.source_endpoint_class)}})).collect();
    let times: Vec<_> = rows
        .iter()
        .map(|r| r.identity.settlement_time.as_i64())
        .collect();
    let complete = window_covered(&times, args.start_ms, args.end_ms);
    Ok(
        json!({"coin":args.coin,"records":records,"has_more":more,"next_start_ms":if more { selected.last().and_then(|r| r.identity.settlement_time.as_i64().checked_add(1)) } else { None },
        "window_fully_covered":complete,"requested_start_ms":args.start_ms,"requested_end_ms":args.end_ms,
        "observed_at_ms":selected.last().map(|r| r.identity.settlement_time.as_i64()),"valuation":"funding rates only; no cash valuation"}),
    )
}

/// All tool results, including failures, have explicit metadata. A response time
/// is separate from the source observation time; unknown coverage is never true.
pub fn envelope(config: &Config, name: &str, result: Result<Value>) -> Value {
    let (data, error) = match result {
        Ok(data) => (data, None),
        Err(error) => (Value::Null, Some(error.to_string())),
    };
    let observation = data
        .get("observed_at_ms")
        .or_else(|| data.get("settlement_time_ms"))
        .or_else(|| data.get("window_end_time_ms"))
        .or_else(|| data.pointer("/prediction/generated_at_ms"))
        .or_else(|| data.get("dataset_last_settlement_ms"))
        .cloned()
        .unwrap_or(Value::Null);
    let coverage = json!({"window_fully_covered":data.get("window_fully_covered"),"contiguous_history_hours":data.get("contiguous_history_hours"),"prediction":data.pointer("/prediction/coverage"),"has_more":data.get("has_more"),"unknown_fields_mean":"not established, not complete"});
    let assumptions = if let Some(assumption) = data.get("valuation_assumption") {
        json!([assumption])
    } else if data.get("settlement_valuation").is_some() {
        json!([
            format!(
                "Funding valuation: {}; price: {}. Fixed/entry-price valuation is an approximation, not account payment reconciliation.",
                data["settlement_valuation"], data["valuation_price"]
            ),
            "Fees are supplied by the trade document; returns are on perp entry notional, not total capital."
        ])
    } else {
        json!([
            "Rates are decimal ratios; simple APR annualizes a rate and is not promised return.",
            CONFIDENCE
        ])
    };
    json!({"schema_version":1,"network":config.network,"observed_at_ms":observation,"responded_at_ms":now_ms().ok(),
        "units":{"money":"USDC","funding_rate":"decimal ratio per hour","apr":"decimal ratio per year, simple","time":"Unix milliseconds","size":"base asset units"},
        "coverage":coverage,"valuation_assumptions":assumptions,"provenance":{"tool":name,"venue":"hyperliquid","analytics_binary_sha256":config.analytics_sha256,"data_source":if name=="get_funding_snapshot" || name=="backfill_funding" {"official venue API"} else {"configured local dataset/capture and caller inputs"},"trade_document_sha256":data.get("trade_document_sha256"),"executor_binary_sha256":data.get("executor_sha256"),"approval_id":data.get("approval_id")},
        "data":data,"error":error})
}

fn window_covered(times: &[i64], start: i64, end: i64) -> bool {
    let Some(first) = times.partition_point(|t| *t <= start).checked_sub(1) else {
        return false;
    };
    let last = times.partition_point(|t| *t < end);
    last < times.len()
        && first <= last
        && times[first..=last].windows(2).all(|w| {
            w[1].checked_sub(w[0])
                .is_some_and(|delta| (3_540_000..=3_660_000).contains(&delta))
        })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionArgs {
    approval_id: String,
}
fn trade(config: &Config, name: &str, args: &ExecutionArgs, stop: &AtomicBool) -> Result<Value> {
    crate::config::validate_id(&args.approval_id)?;
    ensure!(
        config.network == hypercarry_core::info::Network::Mainnet,
        "execution requires mainnet"
    );
    let execution = config
        .execution
        .as_ref()
        .context("execution is not configured")?;
    let approval = execution
        .approval_directory
        .join(format!("{}.json", args.approval_id));
    ensure!(
        std::fs::symlink_metadata(&approval)?.file_type().is_file(),
        "approval must be an existing regular file"
    );
    let operation = if name == "submit_reviewed_canary" {
        "run"
    } else {
        "recover"
    };
    let mut result = process::run(
        &execution.executor_bin,
        &execution.executor_sha256,
        &[
            "run-approved".into(),
            "--config".into(),
            execution.config.to_string_lossy().into_owned(),
            "--evidence".into(),
            execution.evidence.to_string_lossy().into_owned(),
            "--approval".into(),
            approval.to_string_lossy().into_owned(),
            "--operation".into(),
            operation.into(),
            "--enable-mainnet".into(),
        ],
        config.timeout_ms,
        stop,
    )?;
    result["executor_sha256"] = json!(execution.executor_sha256);
    result["approval_id"] = json!(args.approval_id);
    result["observed_at_ms"] = json!(now_ms()?);
    result["valuation_assumption"] = json!(
        "Execution report only. Order cancellation is not proof of a flat position; inspect filled quantity and watchdog recovery."
    );
    Ok(result)
}
