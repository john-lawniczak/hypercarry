//! Golden deserialize tests: parse recorded/representative Hyperliquid `info`
//! payloads into the domain types and assert on decoded fields. These catch
//! schema drift and confirm the string-decimal handling is wired correctly.

use hypercarry_core::types::*;
use rust_decimal::Decimal;
use std::str::FromStr;

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).expect("valid decimal literal in test")
}

#[test]
fn parses_funding_history() {
    let raw = include_str!("fixtures/funding_history.json");
    let fh: FundingHistory = serde_json::from_str(raw).expect("funding_history parses");

    assert_eq!(fh.len(), 3);
    assert_eq!(fh[0].coin, "BTC");
    assert_eq!(fh[0].funding_rate, dec("-0.0006133368"));
    assert_eq!(fh[0].premium, dec("-0.0009133368"));
    assert_eq!(fh[0].time.as_i64(), 1_683_849_600_048);
    assert_eq!(fh[2].funding_rate, dec("0.0000125"));
}

#[test]
fn parses_meta_and_asset_ctxs() {
    let raw = include_str!("fixtures/meta_and_asset_ctxs.json");
    let MetaAndAssetCtxs(meta, ctxs) = serde_json::from_str(raw).expect("metaAndAssetCtxs parses");

    // Universe and context arrays are positionally aligned.
    assert_eq!(meta.universe.len(), ctxs.len());
    assert_eq!(meta.universe[0].name, "BTC");
    assert_eq!(meta.universe[0].sz_decimals, 5);
    assert_eq!(meta.universe[0].max_leverage, 50);

    assert_eq!(ctxs[0].funding, dec("0.0000125"));
    assert_eq!(ctxs[0].mark_px, dec("64260.5"));
    assert_eq!(ctxs[0].oracle_px, dec("64250.0"));
    assert_eq!(ctxs[0].mid_px, Some(dec("64260.0")));
    assert_eq!(ctxs[0].premium, Some(dec("0.0001068963")));
    assert_eq!(ctxs[0].impact_pxs, vec![dec("64259.0"), dec("64261.0")]);
    assert_eq!(ctxs[1].funding, dec("-0.0000042"));
    assert_eq!(ctxs[1].premium, Some(dec("-0.0000210")));

    // Delisted assets (fixture entry copied from a live response): premium,
    // midPx, and impactPxs are explicit `null`s — not missing keys — and the
    // universe entry omits onlyIsolated and carries unknown keys
    // (marginTableId, dayBaseVlm), which must be ignored.
    assert_eq!(meta.universe[2].name, "MATIC");
    assert!(meta.universe[2].is_delisted);
    assert!(!meta.universe[2].only_isolated);
    assert_eq!(ctxs[2].premium, None);
    assert_eq!(ctxs[2].mid_px, None);
    assert!(ctxs[2].impact_pxs.is_empty());
    assert_eq!(ctxs[2].funding, dec("0.0"));
}

#[test]
fn parses_predicted_fundings() {
    let raw = include_str!("fixtures/predicted_fundings.json");
    let pf: PredictedFundings = serde_json::from_str(raw).expect("predictedFundings parses");

    assert_eq!(pf.len(), 2);

    let CoinPredictedFundings(coin, venues) = &pf[0];
    assert_eq!(coin, "BTC");
    assert_eq!(venues.len(), 3);

    let VenuePredictionEntry(venue, pred) = &venues[1];
    assert_eq!(venue, "HlPerp");
    let hl = pred.as_ref().expect("HlPerp prediction present");
    assert_eq!(hl.funding_rate, dec("0.0000058"));
    assert_eq!(hl.funding_interval_hours, Some(1));

    // Bybit entry omits fundingIntervalHours -> None.
    let VenuePredictionEntry(bybit_name, bybit) = &venues[2];
    assert_eq!(bybit_name, "BybitPerp");
    assert_eq!(bybit.as_ref().unwrap().funding_interval_hours, None);
    assert!(hl.next_funding_time.as_i64() > 0);
}

#[test]
fn joins_meta_and_asset_ctxs() {
    let raw = include_str!("fixtures/meta_and_asset_ctxs.json");
    let response: MetaAndAssetCtxs = serde_json::from_str(raw).expect("metaAndAssetCtxs parses");

    let snapshots = response.into_snapshots().expect("matching lengths join");

    assert_eq!(snapshots.len(), 3);
    assert_eq!(snapshots[0].meta.name, "BTC");
    assert_eq!(snapshots[0].ctx.mark_px, dec("64260.5"));
    assert_eq!(snapshots[2].meta.name, "MATIC");
    assert!(snapshots[2].meta.is_delisted);
}

#[test]
fn errors_when_meta_and_ctx_lengths_differ() {
    let response = MetaAndAssetCtxs(
        Meta {
            universe: vec![AssetMeta {
                name: "BTC".to_owned(),
                sz_decimals: 5,
                max_leverage: 50,
                only_isolated: false,
                is_delisted: false,
            }],
        },
        vec![],
    );

    let error = response
        .into_snapshots()
        .expect_err("mismatched lengths must not be truncated");

    assert_eq!(
        error,
        JoinError {
            universe_len: 1,
            ctxs_len: 0,
        }
    );
}
