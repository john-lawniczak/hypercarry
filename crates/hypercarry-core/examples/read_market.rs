//! Fetch typed public market context without credentials or execution features.
//! Usage: `cargo run -p hypercarry-core --example read_market -- mainnet`

use hypercarry_core::info::{InfoClient, MetaAndAssetCtxsRequest, Network, ReqwestInfoTransport};
use std::{error::Error, io};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let network: Network = args
        .next()
        .ok_or_else(|| io::Error::other("usage: read_market <mainnet|testnet>"))?
        .parse()?;
    if args.next().is_some() {
        return Err(io::Error::other("usage: read_market <mainnet|testnet>").into());
    }
    let client = InfoClient::new(ReqwestInfoTransport::for_network(network)?);
    let context = client.execute(&MetaAndAssetCtxsRequest).await?;
    if context.0.universe.len() != context.1.len() {
        return Err(io::Error::other("metadata/context length mismatch").into());
    }
    let markets: Vec<_> = context
        .0
        .universe
        .iter()
        .zip(&context.1)
        .map(|(asset, observation)| {
            serde_json::json!({
                "coin": asset.name,
                "delisted": asset.is_delisted,
                "hourly_funding_rate": observation.funding.to_string(),
                "mark_price": observation.mark_px.to_string(),
                "oracle_price": observation.oracle_px.to_string(),
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "network": network,
            "source": network.info_endpoint(),
            "markets": markets,
        }))?
    );
    Ok(())
}
