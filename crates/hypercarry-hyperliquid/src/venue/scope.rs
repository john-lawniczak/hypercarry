//! Fresh testnet account scope; no signer, cached stream or exchange transport.
use super::{
    BTreeSet, Clock, Duration, ExecutionError, HyperliquidWebSocketTransport, MAX_RESPONSE_BYTES,
    MarketConnection, MarketTransport, Network, SystemClock, TransportMessage, Value, json,
};
use std::collections::BTreeMap;
fn invalid() -> ExecutionError {
    ExecutionError::Validation("incomplete, stale or inconsistent recovery stream scope".into())
}
fn states(
    value: &Value,
    account: &str,
    dexes: &[String],
) -> Result<BTreeMap<String, Value>, ExecutionError> {
    if value["user"] != account {
        return Err(invalid());
    }
    let rows = value["clearinghouseStates"]
        .as_array()
        .ok_or_else(invalid)?;
    let mut result = BTreeMap::new();
    for row in rows {
        let pair = row
            .as_array()
            .filter(|r| r.len() == 2)
            .ok_or_else(invalid)?;
        let name = pair[0].as_str().ok_or_else(invalid)?;
        if !dexes.iter().any(|d| d == name) || result.insert(name.into(), pair[1].clone()).is_some()
        {
            return Err(invalid());
        }
    }
    if result.len() != dexes.len() {
        return Err(invalid());
    }
    Ok(result)
}
pub(super) fn identities(state: &Value) -> Result<BTreeMap<String, Value>, ExecutionError> {
    let mut result = BTreeMap::new();
    for row in state["assetPositions"].as_array().ok_or_else(invalid)? {
        let p = &row["position"];
        let coin = p["coin"].as_str().ok_or_else(invalid)?;
        if result
            .insert(coin.into(), json!([p["szi"], p["entryPx"], p["leverage"]]))
            .is_some()
        {
            return Err(invalid());
        }
    }
    Ok(result)
}
async fn send(
    c: &mut impl MarketConnection,
    method: &str,
    kind: &str,
    account: &str,
    dex: Option<&str>,
) -> Result<(), ExecutionError> {
    let mut sub = json!({"type":kind,"user":account});
    if let Some(dex) = dex {
        sub["dex"] = json!(dex);
    }
    c.send_text(json!({"method":method,"subscription":sub}).to_string())
        .await
        .map_err(|_| invalid())
}
pub(super) async fn bootstrap(account: &str, dexes: &[String]) -> Result<Value, ExecutionError> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let transport = HyperliquidWebSocketTransport::for_network(Network::Testnet);
        let mut c = transport.connect().await.map_err(|_| invalid())?;
        let result = collect(&mut c, account, dexes).await;
        let _ = c.close().await;
        result
    })
    .await
    .map_err(|_| invalid())?
}
#[allow(clippy::too_many_lines)]
async fn collect(
    c: &mut impl MarketConnection,
    account: &str,
    dexes: &[String],
) -> Result<Value, ExecutionError> {
    let started = SystemClock.now_ms()?;
    send(c, "subscribe", "allDexsClearinghouseState", account, None).await?;
    let mut before = None;
    let mut orders = BTreeMap::new();
    let mut requested = false;
    let mut acknowledged = false;
    for _ in 0..10_000 {
        let event = match c.next_message().await.map_err(|_| invalid())? {
            Some(TransportMessage::Text(t)) if t.len() <= 16 * MAX_RESPONSE_BYTES => {
                serde_json::from_str::<Value>(&t)?
            }
            Some(TransportMessage::Ping(p)) => {
                c.send_pong(p).await.map_err(|_| invalid())?;
                continue;
            }
            Some(TransportMessage::Pong) => continue,
            _ => return Err(invalid()),
        };
        let data = &event["data"];
        match event["channel"].as_str() {
            Some("subscriptionResponse")
                if requested
                    && data["method"] == "subscribe"
                    && data["subscription"]["type"] == "allDexsClearinghouseState" =>
            {
                acknowledged = true;
            }
            Some("allDexsClearinghouseState") => {
                let current = states(data, account, dexes)?;
                if let Some(previous) = &before {
                    if acknowledged {
                        let completed = SystemClock.now_ms()?;
                        let previous: &BTreeMap<String, Value> = previous;
                        let mut rows = Vec::new();
                        for name in dexes {
                            if identities(&previous[name])? != identities(&current[name])? {
                                return Err(invalid());
                            }
                            for state in [&previous[name], &current[name]] {
                                let at = state["time"].as_i64().ok_or_else(invalid)?;
                                if at < started.saturating_sub(5000) || at > completed + 1000 {
                                    return Err(invalid());
                                }
                            }
                            rows.push(json!({"dex":name,"state":current[name],"orders":orders.get(name).ok_or_else(invalid)?}));
                        }
                        return Ok(
                            json!({"started_at_ms":started,"completed_at_ms":completed,"dexs":rows}),
                        );
                    }
                } else {
                    before = Some(current);
                    for name in dexes {
                        send(c, "subscribe", "openOrders", account, Some(name)).await?;
                    }
                }
            }
            Some("openOrders") => {
                let dex = data["dex"].as_str().ok_or_else(invalid)?;
                let rows = data["orders"].as_array().ok_or_else(invalid)?;
                if data["user"] != account || !dexes.iter().any(|d| d == dex) {
                    return Err(invalid());
                }
                let mut ids = BTreeSet::new();
                for row in rows {
                    if !ids.insert(row["oid"].as_u64().ok_or_else(invalid)?) {
                        return Err(invalid());
                    }
                }
                orders.insert(dex.to_owned(), rows.clone());
            }
            Some("error") => return Err(invalid()),
            _ => {}
        }
        if orders.len() == dexes.len() && !requested {
            send(c, "unsubscribe", "allDexsClearinghouseState", account, None).await?;
            send(c, "subscribe", "allDexsClearinghouseState", account, None).await?;
            requested = true;
        }
    }
    Err(invalid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::venue::{HyperliquidTransport, ReqwestHyperliquidTransport, validate_address};
    use hypercarry_recorder::transport::TransportError;
    use std::collections::VecDeque;
    struct Connection {
        messages: VecDeque<Value>,
        sent: Vec<Value>,
    }
    #[allow(clippy::unused_async_trait_impl)] // Immediate scripted transport.
    impl MarketConnection for Connection {
        async fn send_text(&mut self, text: String) -> Result<(), TransportError> {
            self.sent.push(serde_json::from_str(&text).unwrap());
            Ok(())
        }
        async fn send_pong(&mut self, _: Vec<u8>) -> Result<(), TransportError> {
            Ok(())
        }
        async fn next_message(&mut self) -> Result<Option<TransportMessage>, TransportError> {
            Ok(self
                .messages
                .pop_front()
                .map(|v| TransportMessage::Text(v.to_string())))
        }
        async fn close(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
    }
    fn fixture() -> Connection {
        let state = json!({"assetPositions":[],"time":SystemClock.now_ms().unwrap()});
        let states = json!({"channel":"allDexsClearinghouseState","data":{"user":"account",
            "clearinghouseStates":[["",state],["test",state]]}});
        Connection {
            sent: vec![],
            messages: VecDeque::from(vec![
                states.clone(),
                json!({"channel":"openOrders","data":{"user":"account","dex":"","orders":[]}}),
                json!({"channel":"openOrders","data":{"user":"account","dex":"test","orders":[]}}),
                json!({"channel":"subscriptionResponse","data":{"method":"subscribe","subscription":{"type":"allDexsClearinghouseState"}}}),
                states,
            ]),
        }
    }
    #[tokio::test]
    async fn complete_snapshot_requires_second_subscription_and_all_orders() {
        let mut c = fixture();
        let result = collect(&mut c, "account", &[String::new(), "test".into()])
            .await
            .unwrap();
        assert_eq!(result["dexs"].as_array().unwrap().len(), 2);
        assert_eq!(c.sent.len(), 5);
        assert_eq!(c.sent[3]["method"], "unsubscribe");
    }
    #[tokio::test]
    async fn missing_wrong_account_changed_or_stale_snapshots_fail_closed() {
        for fault in 0..4 {
            let mut c = fixture();
            match fault {
                0 => {
                    c.messages.remove(2);
                }
                1 => {
                    c.messages.back_mut().unwrap()["data"]["user"] = json!("other");
                }
                2 => {
                    c.messages.back_mut().unwrap()["data"]["clearinghouseStates"][0][1]["assetPositions"] = json!([{"position":{"coin":"BTC","szi":"1","entryPx":"100","leverage":{"type":"isolated","value":1}}}]);
                }
                _ => {
                    c.messages.back_mut().unwrap()["data"]["clearinghouseStates"][0][1]["time"] =
                        json!(0);
                }
            }
            assert!(
                collect(&mut c, "account", &[String::new(), "test".into()])
                    .await
                    .is_err()
            );
        }
    }
    #[test]
    #[ignore = "requires explicit public testnet account and live read-only network access"]
    fn live_complete_testnet_scope() {
        let account = std::env::var("HYPERCARRY_TESTNET_ACCOUNT").expect("public testnet account");
        validate_address(&account).unwrap();
        let mut transport =
            ReqwestHyperliquidTransport::new(Duration::from_secs(2), Duration::from_secs(3))
                .unwrap();
        let discovery = transport.info(&json!({"type":"perpDexs"})).unwrap();
        let mut dexes = vec![String::new()];
        for row in &discovery.as_array().unwrap()[1..] {
            dexes.push(row["name"].as_str().unwrap().into());
        }
        let scope = transport.recovery_scope(&account, &dexes).unwrap().unwrap();
        assert_eq!(scope["dexs"].as_array().unwrap().len(), dexes.len());
    }
}
