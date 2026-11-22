//! Fresh stream bootstrap; no cached state or signing/exchange transport.
use super::{
    AccountError, BTreeMap, BTreeSet, Duration, Network, Result, Value, fail, scoped_coin,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

pub(super) fn order_ids(rows: &[Value], dex: Option<&str>) -> Result<BTreeSet<u64>> {
    let mut ids = BTreeSet::new();
    for row in rows {
        let id = row["oid"]
            .as_u64()
            .ok_or_else(|| fail("invalid streamed order ID"))?;
        if !ids.insert(id) || !row["coin"].as_str().is_some_and(|c| scoped_coin(c, dex)) {
            return Err(fail("duplicate or misrouted streamed order"));
        }
    }
    Ok(ids)
}

fn subscription(method: &str, kind: &str, account: &str, dex: Option<&str>) -> Value {
    let mut sub = json!({"type":kind,"user":account});
    if let Some(dex) = dex {
        sub["dex"] = json!(dex);
    }
    json!({"method":method,"subscription":sub})
}

struct Collector<'a> {
    account: &'a str,
    dexes: &'a [String],
    before: Option<BTreeMap<String, Value>>,
    orders: BTreeMap<String, Vec<Value>>,
    requested: bool,
    acknowledged: bool,
}
impl<'a> Collector<'a> {
    fn new(account: &'a str, dexes: &'a [String]) -> Self {
        Self {
            account,
            dexes,
            before: None,
            orders: BTreeMap::new(),
            requested: false,
            acknowledged: false,
        }
    }
    fn states(&self, data: &Value) -> Result<BTreeMap<String, Value>> {
        if !data["user"]
            .as_str()
            .is_some_and(|u| u.eq_ignore_ascii_case(self.account))
        {
            return Err(fail("stream account mismatch"));
        }
        let rows = data["clearinghouseStates"]
            .as_array()
            .ok_or_else(|| fail("invalid stream states"))?;
        let mut states = BTreeMap::new();
        for row in rows {
            let pair = row
                .as_array()
                .filter(|p| p.len() == 2)
                .ok_or_else(|| fail("invalid state pair"))?;
            let name = pair[0].as_str().ok_or_else(|| fail("invalid stream DEX"))?;
            if !self.dexes.iter().any(|d| d == name)
                || states.insert(name.to_owned(), pair[1].clone()).is_some()
            {
                return Err(fail("unexpected or duplicate stream DEX"));
            }
        }
        if states.len() != self.dexes.len() {
            return Err(fail("incomplete stream DEXs"));
        }
        Ok(states)
    }
    fn event(&mut self, event: &Value) -> Result<(Vec<Value>, Option<Value>)> {
        let data = &event["data"];
        let mut send = vec![];
        match event["channel"].as_str() {
            Some("error") => return Err(fail("portfolio stream subscription refused")),
            Some("subscriptionResponse")
                if self.requested
                    && data["method"] == "subscribe"
                    && data["subscription"]["type"] == "allDexsClearinghouseState"
                    && data["subscription"]["user"]
                        .as_str()
                        .is_some_and(|u| u.eq_ignore_ascii_case(self.account)) =>
            {
                self.acknowledged = true;
            }
            Some("allDexsClearinghouseState") => {
                let states = self.states(data)?;
                if let Some(before) = &self.before {
                    if self.acknowledged {
                        // The refresh follows every initial order snapshot;
                        // changed orders during the refresh invalidate that pair.
                        let rows: Vec<_> = self.dexes.iter().map(|dex| json!({
                            "dex":dex,"before":before[dex],"state":states[dex],"orders":self.orders[dex]
                        })).collect();
                        return Ok((send, Some(json!(rows))));
                    }
                } else {
                    self.before = Some(states);
                    for dex in self.dexes {
                        send.push(subscription(
                            "subscribe",
                            "openOrders",
                            self.account,
                            Some(dex),
                        ));
                    }
                }
            }
            Some("openOrders") => {
                let dex = data["dex"]
                    .as_str()
                    .ok_or_else(|| fail("missing order DEX"))?;
                if self.before.is_none()
                    || !self.dexes.iter().any(|d| d == dex)
                    || !data["user"]
                        .as_str()
                        .is_some_and(|u| u.eq_ignore_ascii_case(self.account))
                {
                    return Err(fail("unexpected order stream account/DEX"));
                }
                let rows = data["orders"]
                    .as_array()
                    .ok_or_else(|| fail("invalid order snapshot"))?;
                order_ids(rows, if dex.is_empty() { None } else { Some(dex) })?;
                if self.requested && self.orders.get(dex) != Some(rows) {
                    return Err(fail("orders changed during portfolio stream refresh"));
                }
                if !self.requested {
                    self.orders.insert(dex.to_owned(), rows.clone());
                }
            }
            _ => {}
        }
        if self.orders.len() == self.dexes.len() && !self.requested {
            for method in ["unsubscribe", "subscribe"] {
                send.push(subscription(
                    method,
                    "allDexsClearinghouseState",
                    self.account,
                    None,
                ));
            }
            self.requested = true;
        }
        Ok((send, None))
    }
}

pub(crate) async fn bootstrap(network: Network, account: &str, dexes: &[String]) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let config = WebSocketConfig::default()
            .max_message_size(Some(16 * 1024 * 1024))
            .max_frame_size(Some(16 * 1024 * 1024));
        let (mut socket, _) =
            connect_async_with_config(network.websocket_endpoint(), Some(config), false)
                .await
                .map_err(|e| AccountError(e.to_string()))?;
        socket
            .send(Message::Text(
                subscription("subscribe", "allDexsClearinghouseState", account, None)
                    .to_string()
                    .into(),
            ))
            .await
            .map_err(|e| AccountError(e.to_string()))?;
        let mut collector = Collector::new(account, dexes);
        for _ in 0..10_000 {
            let message = socket
                .next()
                .await
                .ok_or_else(|| fail("portfolio stream closed"))?
                .map_err(|e| AccountError(e.to_string()))?;
            let text = match message {
                Message::Text(text) => text,
                Message::Ping(payload) => {
                    socket
                        .send(Message::Pong(payload))
                        .await
                        .map_err(|e| AccountError(e.to_string()))?;
                    continue;
                }
                Message::Pong(_) => continue,
                _ => return Err(fail("unexpected portfolio stream frame")),
            };
            let event: Value =
                serde_json::from_str(&text).map_err(|e| AccountError(e.to_string()))?;
            let (requests, complete) = collector.event(&event)?;
            for request in requests {
                socket
                    .send(Message::Text(request.to_string().into()))
                    .await
                    .map_err(|e| AccountError(e.to_string()))?;
            }
            if let Some(complete) = complete {
                // Drop closes the socket even if the peer never acknowledges close.
                return Ok(complete);
            }
        }
        Err(fail("portfolio stream message budget exhausted"))
    })
    .await
    .map_err(|_| fail("portfolio stream bootstrap deadline exceeded"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state(account: &str) -> Value {
        json!({"channel":"allDexsClearinghouseState","data":{"user":account,"clearinghouseStates":[["",{}],["xyz",{}]]}})
    }
    fn orders(dex: &str) -> Value {
        json!({"channel":"openOrders","data":{"user":"account","dex":dex,"orders":[]}})
    }
    fn ack() -> Value {
        json!({"channel":"subscriptionResponse","data":{"method":"subscribe","subscription":{"type":"allDexsClearinghouseState","user":"account"}}})
    }
    #[test]
    fn scope_requires_all_orders_and_acknowledged_refresh() {
        let dexes = [String::new(), "xyz".into()];
        let mut c = Collector::new("account", &dexes);
        c.event(&ack()).unwrap(); // An old acknowledgement cannot authorize refresh.
        assert_eq!(c.event(&state("account")).unwrap().0.len(), 2);
        assert_eq!(c.event(&orders("")).unwrap().0, Vec::<Value>::new());
        assert!(c.event(&state("account")).unwrap().1.is_none());
        assert_eq!(c.event(&orders("xyz")).unwrap().0.len(), 2);
        assert!(c.event(&state("account")).unwrap().1.is_none());
        let mut wrong = ack();
        wrong["data"]["subscription"]["user"] = json!("other");
        c.event(&wrong).unwrap();
        assert!(c.event(&state("account")).unwrap().1.is_none());
        c.event(&ack()).unwrap();
        assert_eq!(
            c.event(&state("account"))
                .unwrap()
                .1
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
    #[test]
    fn missing_duplicate_misrouted_and_wrong_account_messages_fail() {
        let dexes = [String::new(), "xyz".into()];
        for case in 0..6 {
            let mut c = Collector::new("account", &dexes);
            let mut event = state("account");
            match case {
                0 => event["data"]["user"] = json!("other"),
                1 => {
                    event["data"]["clearinghouseStates"]
                        .as_array_mut()
                        .unwrap()
                        .pop();
                }
                2 => event["data"]["clearinghouseStates"][1][0] = json!(""),
                _ => {
                    c.event(&event).unwrap();
                    event = orders("xyz");
                    match case {
                        3 => event["data"]["user"] = json!("other"),
                        4 => event["data"]["orders"] = json!([{"oid":1,"coin":"BTC"}]),
                        _ => {
                            event["data"]["orders"] =
                                json!([{"oid":1,"coin":"xyz:A"},{"oid":1,"coin":"xyz:A"}]);
                        }
                    }
                }
            }
            assert!(c.event(&event).is_err(), "case {case}");
        }
    }

    #[test]
    fn new_orders_during_refresh_cannot_hide_in_flat_scope() {
        let dexes = [String::new(), "xyz".into()];
        let mut c = Collector::new("account", &dexes);
        c.event(&state("account")).unwrap();
        c.event(&orders("")).unwrap();
        c.event(&orders("xyz")).unwrap();
        c.event(&orders("xyz")).unwrap(); // Identical repeat is harmless.
        let mut changed = orders("xyz");
        changed["data"]["orders"] = json!([{"coin":"xyz:A","oid":1}]);
        assert!(c.event(&changed).is_err());
    }
}
