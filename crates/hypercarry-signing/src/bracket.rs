//! Extended wire codec for the proposed local trade workflow.
//! Deliberately separate from `Action`: the deployed canary policy stays narrow.
//! Encoding/signing support is not execution authorization or release readiness.
use crate::ExpiryMs;
use crate::{
    Address, B256, Chain, DigestSigner, Signature, SignatureRecovery, WireSignature, l1_prehash,
};
use anyhow::{Result, ensure};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum BracketAction {
    UpdateLeverage {
        asset: u32,
        is_cross: bool,
        leverage: u32,
    },
    UpdateIsolatedMargin {
        asset: u32,
        is_buy: bool,
        ntli: i64,
    },
    Order {
        orders: Vec<BracketOrder>,
        grouping: Grouping,
    },
    Cancel {
        cancels: Vec<Cancel>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Grouping {
    Na,
    NormalTpsl,
    PositionTpsl,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cancel {
    pub a: u32,
    pub o: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BracketOrder {
    pub a: u32,
    pub b: bool,
    #[serde(with = "crate::decimal")]
    pub p: Decimal,
    #[serde(with = "crate::decimal")]
    pub s: Decimal,
    pub r: bool,
    pub t: OrderType,
    pub c: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum OrderType {
    Limit {
        tif: TimeInForce,
    },
    Trigger {
        #[serde(rename = "isMarket")]
        is_market: bool,
        #[serde(rename = "triggerPx", with = "crate::decimal")]
        trigger_px: Decimal,
        tpsl: TriggerKind,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Alo,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TriggerKind {
    Tp,
    Sl,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BracketRequest {
    pub action: BracketAction,
    pub nonce: u64,
    pub signature: WireSignature,
    pub vault_address: Option<Address>,
    pub expires_after: u64,
}
impl BracketAction {
    /// Structural checks only. Caller policy must independently bound market,
    /// side, notional, account, leverage, margin changes and allowed operations.
    /// # Errors
    /// Rejects unsupported counts, malformed IDs and structurally invalid protection.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::UpdateLeverage { leverage, .. } => {
                ensure!((1..=50).contains(leverage), "invalid leverage");
            }
            Self::UpdateIsolatedMargin { ntli, .. } => {
                ensure!(*ntli != 0, "zero margin adjustment");
            }
            Self::Cancel { cancels } => ensure!(
                !cancels.is_empty() && cancels.len() <= 3,
                "invalid cancel count"
            ),
            Self::Order { orders, grouping } => {
                ensure!(
                    !orders.is_empty() && orders.len() <= 3,
                    "invalid order count"
                );
                let mut ids = std::collections::BTreeSet::new();
                for order in orders {
                    ensure!(
                        order.p > Decimal::ZERO && order.s > Decimal::ZERO,
                        "invalid price or quantity"
                    );
                    ensure!(
                        order.c.len() == 34
                            && order.c.starts_with("0x")
                            && order.c[2..].bytes().all(|b| b.is_ascii_hexdigit()),
                        "invalid client order id"
                    );
                    ensure!(
                        ids.insert(order.c.to_lowercase()),
                        "duplicate client order id"
                    );
                    if let OrderType::Trigger { trigger_px, .. } = order.t {
                        ensure!(
                            trigger_px > Decimal::ZERO && order.r,
                            "trigger must be positive and reduce-only"
                        );
                    }
                }
                match grouping {
                    Grouping::Na => {}
                    Grouping::NormalTpsl => {
                        ensure!(
                            orders.len() == 3
                                && !orders[0].r
                                && matches!(orders[0].t, OrderType::Limit { .. }),
                            "parent bracket must begin with one entry"
                        );
                        let entry = &orders[0];
                        for exit in &orders[1..] {
                            ensure!(
                                exit.r
                                    && exit.b != entry.b
                                    && exit.a == entry.a
                                    && exit.s == entry.s,
                                "bracket exits must match entry exposure"
                            );
                        }
                        ensure!(
                            matches!(
                                orders[1].t,
                                OrderType::Trigger {
                                    tpsl: TriggerKind::Tp,
                                    ..
                                }
                            ) && matches!(
                                orders[2].t,
                                OrderType::Trigger {
                                    tpsl: TriggerKind::Sl,
                                    ..
                                }
                            ),
                            "bracket requires TP then SL"
                        );
                    }
                    Grouping::PositionTpsl => ensure!(
                        orders
                            .iter()
                            .all(|o| o.r && matches!(o.t, OrderType::Trigger { .. })),
                        "position exits must be reduce-only triggers"
                    ),
                }
            }
        }
        Ok(())
    }
    /// Compute the network-bound Hyperliquid L1 signing hash.
    ///
    /// # Errors
    /// Rejects invalid actions, out-of-range timestamps or expiry beyond 60 seconds.
    pub fn prehash(&self, nonce: u64, expiry: u64, chain: Chain) -> Result<B256> {
        self.validate()?;
        ensure!(
            expiry > nonce && expiry - nonce <= 60_000,
            "expiry must be within 60s of nonce"
        );
        let time = ExpiryMs::from_timestamp_millis(i64::try_from(expiry)?)
            .ok_or_else(|| anyhow::anyhow!("invalid expiry"))?;
        l1_prehash(self, nonce, Some(time), chain)
    }
    /// Sign a validated action using the supplied digest signer.
    ///
    /// # Errors
    /// Propagates encoding/signing failures and rejects recovered identity mismatches.
    pub fn sign<S: DigestSigner>(
        &self,
        signer: &S,
        nonce: u64,
        expiry: u64,
        chain: Chain,
    ) -> Result<BracketRequest> {
        let hash = self.prehash(nonce, expiry, chain)?;
        let signature = signer.sign_hash_sync(&hash)?;
        ensure!(
            signature.recover_address_from_prehash(&hash)? == signer.address(),
            "signer identity mismatch"
        );
        Ok(BracketRequest {
            action: self.clone(),
            nonce,
            signature: signature.into(),
            vault_address: None,
            expires_after: expiry,
        })
    }
}
impl BracketRequest {
    /// Recover the signer under the explicitly selected network.
    ///
    /// # Errors
    /// Rejects vault routing, invalid expiry and noncanonical signatures.
    pub fn recover(&self, chain: Chain) -> Result<Address> {
        ensure!(
            self.vault_address.is_none(),
            "vaults are outside local bracket scope"
        );
        ensure!(
            matches!(self.signature.v, 27 | 28),
            "invalid signature parity"
        );
        Signature::new(self.signature.r, self.signature.s, self.signature.v == 28)
            .recover_address_from_prehash(&self.action.prehash(
                self.nonce,
                self.expires_after,
                chain,
            )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn new_actions_do_not_expand_canary_codec() {
        for value in [
            json!({"type":"updateLeverage","asset":2,"isCross":false,"leverage":1}),
            json!({"type":"updateIsolatedMargin","asset":2,"isBuy":true,"ntli":1_000_000}),
        ] {
            assert!(serde_json::from_value::<crate::Action>(value.clone()).is_err());
            let action: BracketAction = serde_json::from_value(value).unwrap();
            action.validate().unwrap();
            let mut bytes = [0; 32];
            bytes[31] = 1; // public unfunded fixture
            let key = crate::SigningKey::from_slice(&bytes).unwrap();
            let request = action.sign(&key, 1000, 2000, Chain::Testnet).unwrap();
            assert_eq!(request.recover(Chain::Testnet).unwrap(), key.address());
            assert_ne!(request.recover(Chain::Mainnet).unwrap(), key.address());
        }
    }
    #[test]
    fn transfer_and_unknown_fields_rejected() {
        for value in [
            json!({"type":"withdraw3","amount":"1"}),
            json!({"type":"updateLeverage","asset":2,"isCross":false,"leverage":1,"destination":"anything"}),
        ] {
            assert!(serde_json::from_value::<BracketAction>(value).is_err());
        }
    }
    #[test]
    fn unprotected_trigger_is_rejected() {
        let value = json!({"type":"order","orders":[{"a":2,"b":true,"p":"77","s":"1","r":false,"t":{"trigger":{"isMarket":true,"triggerPx":"70","tpsl":"tp"}},"c":"0x11111111111111111111111111111111"}],"grouping":"positionTpsl"});
        let action: BracketAction = serde_json::from_value(value).unwrap();
        assert!(action.validate().is_err());
    }
}
