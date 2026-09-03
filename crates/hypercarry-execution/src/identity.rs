use crate::{ExecutionError, ValidatedOrder};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::{fmt, str::FromStr};

const DOMAIN: &[u8] = b"hypercarry/client-order-id/v1\0";

/// Hyperliquid-compatible, deterministic 128-bit client order identifier.
///
/// The identifier is derived from the complete durable, *quantized* order
/// identity. It can be safely persisted and reused while reconciling an
/// uncertain submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientOrderId([u8; 16]);

impl ClientOrderId {
    /// Derives a domain-separated identifier from an already validated,
    /// venue-quantized order.
    ///
    /// Hashing the resolved order (not the pre-quantization intent) ensures
    /// the identifier is a faithful function of exactly what was risk-checked
    /// and submitted: two intents that quantize to the same order collide and
    /// dedupe, and one intent quantized under different venue metadata never
    /// silently reuses a stale identifier for a different actual size/price.
    ///
    /// # Errors
    ///
    /// Returns an error when the order is invalid.
    pub fn derive(order: &ValidatedOrder) -> Result<Self, ExecutionError> {
        order.validate()?;
        let mut hasher = Sha256::new();
        hasher.update(DOMAIN);
        update_field(&mut hasher, order.correlation_id.as_bytes());
        update_field(&mut hasher, order.venue.as_bytes());
        update_field(&mut hasher, order.market.as_bytes());
        hasher.update([match order.side {
            crate::Side::Buy => 0,
            crate::Side::Sell => 1,
        }]);
        update_field(
            &mut hasher,
            order.quantity.normalize().to_string().as_bytes(),
        );
        update_field(
            &mut hasher,
            order.limit_price.normalize().to_string().as_bytes(),
        );
        hasher.update(order.created_at_ms.to_be_bytes());
        let digest = hasher.finalize();
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        Ok(Self(bytes))
    }

    /// Returns the raw 128-bit identifier.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for ClientOrderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("0x")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for ClientOrderId {
    type Err = ExecutionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let digits = value.strip_prefix("0x").ok_or_else(|| {
            ExecutionError::Validation("client order ID must start with `0x`".to_owned())
        })?;
        if digits.len() != 32 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ExecutionError::Validation(
                "client order ID must contain exactly 32 hexadecimal digits".to_owned(),
            ));
        }
        let mut bytes = [0_u8; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            let start = index * 2;
            let chunk = &digits.as_bytes()[start..start + 2];
            let text = std::str::from_utf8(chunk).expect("ASCII hex is valid UTF-8");
            *byte = u8::from_str_radix(text, 16).map_err(|_| {
                ExecutionError::Validation("client order ID contains invalid hex".to_owned())
            })?;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for ClientOrderId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ClientOrderId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

fn update_field(hasher: &mut Sha256, field: &[u8]) {
    hasher.update(u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(field);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    #[test]
    fn identity_is_stable_and_hyperliquid_compatible() {
        let id = ClientOrderId::derive(&intent("intent-1", "1.00")).unwrap();
        assert_eq!(id.to_string().len(), 34);
        assert_eq!(id.to_string().parse::<ClientOrderId>().unwrap(), id);
        assert_eq!(
            id,
            ClientOrderId::derive(&intent("intent-1", "1.0")).unwrap()
        );
    }

    #[test]
    fn distinct_durable_intents_do_not_alias() {
        assert_ne!(
            ClientOrderId::derive(&intent("intent-1", "1")).unwrap(),
            ClientOrderId::derive(&intent("intent-2", "1")).unwrap()
        );
    }

    #[test]
    fn malformed_identifier_is_rejected() {
        assert!("1234".parse::<ClientOrderId>().is_err());
        assert!(
            "0xzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
                .parse::<ClientOrderId>()
                .is_err()
        );
    }

    fn intent(correlation_id: &str, quantity: &str) -> ValidatedOrder {
        let intent = crate::OrderIntent::limit(
            correlation_id,
            "hyperliquid",
            "BTC",
            crate::Side::Buy,
            quantity.parse().unwrap(),
            Decimal::from(100),
            1_000,
        )
        .unwrap();
        let metadata =
            crate::MarketMetadata::new("hyperliquid", "BTC", dec("0.01"), dec("0.01"), dec("0.01"))
                .unwrap();
        ValidatedOrder::resolve(&intent, &metadata).unwrap()
    }

    fn dec(value: &str) -> Decimal {
        value.parse().unwrap()
    }
}
