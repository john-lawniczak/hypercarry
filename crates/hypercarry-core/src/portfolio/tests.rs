use super::*;
use serde_json::json;
use std::{collections::VecDeque, sync::Mutex};

fn position(short: bool) -> Position {
    serde_json::from_value(json!({"coin":"xyz:EXAMPLE","szi":if short {"-2"} else {"2"},"entryPx":"100","leverage":{"type":"isolated","value":1,"rawUsd":"400"},"positionValue":"200","unrealizedPnl":"0","marginUsed":"200","liquidationPx":"195","cumFunding":{}})).unwrap()
}
fn order(id: u64, stop: bool, short: bool) -> OpenOrder {
    let price = if stop == short { "120" } else { "80" };
    serde_json::from_value(json!({"coin":"xyz:EXAMPLE","side":if short {"B"} else {"A"},"oid":id,"sz":"2","limitPx":price,"isTrigger":true,"triggerPx":price,"reduceOnly":true,"orderType":if stop {"Stop Market"} else {"Take Profit Market"},"isPositionTpsl":false})).unwrap()
}
#[test]
fn full_exits_require_correct_direction_size_type_and_price() {
    for short in [true, false] {
        let p = position(short);
        let orders = vec![order(1, true, short), order(2, false, short)];
        assert!(exit_coverage(&p, &orders).unwrap().full_pair_observed());
        for bad in 0..9 {
            let mut orders = orders.clone();
            match bad {
                0 => orders[0].reduce_only = false,
                1 => orders[0].side = if short { "A" } else { "B" }.into(),
                2 => orders[0].sz = Decimal::ONE,
                3 => orders[0].is_trigger = false,
                4 => orders[0].order_type = "Unrecognized Stop".into(),
                5 => orders[0].trigger_px = Decimal::ZERO,
                6 => {
                    orders[0].trigger_px = if short {
                        Decimal::from(90)
                    } else {
                        Decimal::from(110)
                    }
                }
                7 => {
                    orders[0].limit_px = if short {
                        Decimal::ONE
                    } else {
                        Decimal::from(1000)
                    }
                }
                _ => {
                    orders[0].is_position_tpsl = true;
                    orders[0].sz = Decimal::ZERO;
                }
            }
            assert!(
                !exit_coverage(&p, &orders).unwrap().full_pair_observed(),
                "case {bad}"
            );
        }
    }
}
#[test]
fn split_exits_and_other_markets_never_inflate_coverage() {
    let mut a = order(1, true, true);
    a.sz = Decimal::ONE;
    let mut b = a.clone();
    b.oid = 2;
    let mut other = order(3, true, true);
    other.coin = "EXAMPLE".into();
    assert_eq!(
        exit_coverage(&position(true), &[a, b, other])
            .unwrap()
            .stop_order_ids,
        [] as [u64; 0]
    );
    let mut p = position(true);
    p.szi = Decimal::ZERO;
    assert!(exit_coverage(&p, &[]).is_err());
}
#[test]
fn discovery_requires_native_and_unique_builder_names() {
    for invalid in [
        json!([]),
        json!([{"name":"xyz"}]),
        json!([null, null]),
        json!([null,{"name":""}]),
        json!([null,{"name":"bad\nname"}]),
        json!([null,{"name":"xyz"},{"name":"xyz"}]),
    ] {
        assert!(dex_names(&serde_json::from_value::<Vec<Option<Dex>>>(invalid).unwrap()).is_err());
    }
    assert_eq!(
        dex_names(&[None, Some(Dex { name: "xyz".into() })]).unwrap(),
        vec![None, Some("xyz".into())]
    );
    assert!(
        dex_names(&[
            None,
            Some(Dex {
                name: "i<3fl".into()
            })
        ])
        .is_ok()
    );
}
struct Fixture(Mutex<VecDeque<(Value, Value)>>);
impl InfoTransport for Fixture {
    fn network(&self) -> Network {
        Network::Testnet
    }
    #[allow(clippy::unused_async_trait_impl)]
    async fn post(&self, body: &[u8]) -> std::result::Result<Vec<u8>, crate::info::BoxError> {
        let request: Value = serde_json::from_slice(body)?;
        let (expected, response) = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request");
        assert_eq!(request, expected);
        Ok(serde_json::to_vec(&response)?)
    }
}
const ADDRESS: &str = "0x1111111111111111111111111111111111111111";
fn fixture() -> VecDeque<(Value, Value)> {
    let state = |ps: Vec<Value>| json!({"assetPositions":ps,"time":now_ms().unwrap(),"crossMarginSummary":{},"crossMaintenanceMarginUsed":"0"});
    VecDeque::from([
        (json!({"type":"perpDexs"}), json!([null,{"name":"xyz"}])),
        (
            json!({"type":"spotClearinghouseState","user":ADDRESS}),
            json!({"balances":[{"coin":"USDC","token":0,"total":"700","hold":"200"}],"tokenToAvailableAfterMaintenance":[[0,"500"]]}),
        ),
        (
            json!({"type":"clearinghouseState","user":ADDRESS}),
            state(vec![]),
        ),
        (
            json!({"type":"frontendOpenOrders","user":ADDRESS}),
            json!([]),
        ),
        (
            json!({"type":"clearinghouseState","user":ADDRESS,"dex":"xyz"}),
            state(vec![json!({"position":position(true)})]),
        ),
        (
            json!({"type":"frontendOpenOrders","user":ADDRESS,"dex":"xyz"}),
            json!([order(1, true, true), order(2, false, true)]),
        ),
        (json!({"type":"perpDexs"}), json!([null,{"name":"xyz"}])),
    ])
}
fn bracket(mut rows: VecDeque<(Value, Value)>) -> VecDeque<(Value, Value)> {
    let native = rows[2].clone();
    let builder = rows[4].clone();
    rows.insert(6, builder);
    rows.insert(4, native);
    rows
}
#[tokio::test]
async fn builder_position_and_exits_are_discovered_and_shared_cash_is_not_added() {
    let c = InfoClient::new(Fixture(Mutex::new(bracket(fixture()))));
    let s = snapshot(&c, ADDRESS).await.unwrap();
    assert_eq!(s.dexs.len(), 2);
    assert!(s.dexs[0].exits.is_empty());
    assert_eq!(s.dexs[1].exits[0].coin, "xyz:EXAMPLE");
    assert!(s.exit_coverage_observed());
    assert_eq!(s.balances.available_usdc().unwrap(), Decimal::from(500));
    assert_eq!(s.balances.balances[0].total, Decimal::from(700));
    assert!(
        serde_json::to_value(&s)
            .unwrap()
            .get("account_equity")
            .is_none()
    );
}
#[tokio::test]
async fn incomplete_builder_state_duplicate_orders_and_stale_data_are_errors() {
    for bad in 0..8 {
        let mut f = fixture();
        match bad {
            0 => f[4].1 = json!({}),
            1 => f[5].1 = json!([order(1, true, true), order(1, false, true)]),
            2 => f[4].1["time"] = json!(1),
            3 => f[6].1 = json!([null]),
            4 => f[4].1["assetPositions"][0]["position"]["coin"] = json!("wrong:EXAMPLE"),
            5 => f[5].1 = json!({"error":"rate limited"}),
            6 => f[5].1[0]["coin"] = json!("wrong:EXAMPLE"),
            _ => {
                let p = f[4].1["assetPositions"][0].clone();
                f[4].1["assetPositions"].as_array_mut().unwrap().push(p);
            }
        }
        assert!(
            snapshot(&InfoClient::new(Fixture(Mutex::new(bracket(f)))), ADDRESS)
                .await
                .is_err(),
            "case {bad}"
        );
    }
}
#[tokio::test]
async fn absent_exit_and_orphan_trigger_are_visible_failures() {
    let mut f = fixture();
    f[5].1 = json!([order(1, true, true)]);
    let s = snapshot(&InfoClient::new(Fixture(Mutex::new(bracket(f)))), ADDRESS)
        .await
        .unwrap();
    assert!(!s.exit_coverage_observed());
    let mut f = fixture();
    f[4].1["assetPositions"] = json!([]);
    let s = snapshot(&InfoClient::new(Fixture(Mutex::new(bracket(f)))), ADDRESS)
        .await
        .unwrap();
    assert_eq!(s.dexs[1].orphan_trigger_ids, vec![1, 2]);
    assert!(!s.exit_coverage_observed());
}

#[tokio::test]
async fn stale_flat_future_and_misrouted_native_states_fail_closed() {
    for bad in 0..4 {
        let mut f = fixture();
        match bad {
            0 => f[2].1["time"] = json!(1),
            1 => f[2].1["time"] = json!(now_ms().unwrap() + 60_000),
            2 => f[2].1["assetPositions"] = f[4].1["assetPositions"].clone(),
            _ => f[3].1 = f[5].1.clone(),
        }
        assert!(
            snapshot(&InfoClient::new(Fixture(Mutex::new(bracket(f)))), ADDRESS)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn position_changes_while_reading_orders_never_certify_old_exits() {
    for (field, value) in [
        ("szi", json!("-3")),
        ("szi", json!("2")),
        ("entryPx", json!("101")),
        ("leverage", json!({"type":"cross","value":2})),
    ] {
        let mut f = bracket(fixture());
        f[7].1["assetPositions"][0]["position"][field] = value;
        assert!(
            snapshot(&InfoClient::new(Fixture(Mutex::new(f))), ADDRESS)
                .await
                .is_err()
        );
    }
    let mut f = bracket(fixture());
    f[7].1["assetPositions"] = json!([]);
    assert!(
        snapshot(&InfoClient::new(Fixture(Mutex::new(f))), ADDRESS)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn ordinary_mark_changes_do_not_invalidate_same_position() {
    let mut f = bracket(fixture());
    f[7].1["assetPositions"][0]["position"]["positionValue"] = json!("202");
    f[7].1["assetPositions"][0]["position"]["unrealizedPnl"] = json!("-2");
    assert!(
        snapshot(&InfoClient::new(Fixture(Mutex::new(f))), ADDRESS)
            .await
            .unwrap()
            .exit_coverage_observed()
    );
}

struct Hung;
impl InfoTransport for Hung {
    fn network(&self) -> Network {
        Network::Testnet
    }
    async fn post(&self, _body: &[u8]) -> std::result::Result<Vec<u8>, crate::info::BoxError> {
        std::future::pending().await
    }
}
#[tokio::test]
async fn entire_observation_has_a_monotonic_deadline() {
    let result = with_deadline(&InfoClient::new(Hung), ADDRESS, Duration::from_millis(1)).await;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("deadline exceeded")
    );
    assert!(check_duration(10, 9).is_err());
    assert!(check_duration(0, 120_001).is_err());
}

#[test]
fn order_evidence_survives_serialization_and_pending_children_are_not_coverage() {
    let mut raw = serde_json::to_value(order(10, true, true)).unwrap();
    raw["cloid"] = json!("0x11111111111111111111111111111111");
    raw["timestamp"] = json!(12345);
    raw["triggerCondition"] = json!("Price above 120");
    raw["children"] = json!([order(11, true, true), order(12, false, true)]);
    let parsed: OpenOrder = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(&parsed).unwrap(), raw);
    assert!(
        !exit_coverage(&position(true), &[parsed])
            .unwrap()
            .full_pair_observed()
    );
}

struct LargeFixture {
    rows: Value,
    names: Value,
    rest_orders: Value,
    changed_discovery: bool,
    discoveries: std::sync::atomic::AtomicUsize,
    reads: Mutex<Vec<Value>>,
}
impl LargeFixture {
    fn new(count: usize) -> Self {
        let state = json!({"assetPositions":[],"time":now_ms().unwrap(),"crossMarginSummary":{},"crossMaintenanceMarginUsed":"0"});
        let names: Vec<_> = std::iter::once(Value::Null)
            .chain((1..count).map(|i| json!({"name":format!("dex{i}")})))
            .collect();
        let rows: Vec<_> = (0..count)
            .map(|i| {
                json!({"dex":if i == 0 { String::new() } else { format!("dex{i}") },
            "before":state,"state":state,"orders":[]})
            })
            .collect();
        Self {
            rows: json!(rows),
            names: json!(names),
            rest_orders: json!([]),
            changed_discovery: false,
            discoveries: std::sync::atomic::AtomicUsize::new(0),
            reads: Mutex::new(vec![]),
        }
    }
}
impl InfoTransport for LargeFixture {
    fn network(&self) -> Network {
        Network::Testnet
    }
    #[allow(clippy::unused_async_trait_impl)]
    async fn portfolio_scope(
        &self,
        _: &str,
        dexes: &[String],
    ) -> std::result::Result<Value, crate::info::BoxError> {
        assert_eq!(dexes.len(), self.names.as_array().unwrap().len());
        Ok(self.rows.clone())
    }
    #[allow(clippy::unused_async_trait_impl)]
    async fn post(&self, body: &[u8]) -> std::result::Result<Vec<u8>, crate::info::BoxError> {
        let request: Value = serde_json::from_slice(body)?;
        self.reads.lock().unwrap().push(request.clone());
        let value = match request["type"].as_str().unwrap() {
            "perpDexs" => {
                let n = self
                    .discoveries
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if self.changed_discovery && n > 0 {
                    json!([null])
                } else {
                    self.names.clone()
                }
            }
            "spotClearinghouseState" => {
                json!({"balances":[],"tokenToAvailableAfterMaintenance":[]})
            }
            "clearinghouseState" => {
                json!({"assetPositions":[],"time":now_ms().unwrap(),"crossMarginSummary":{},"crossMaintenanceMarginUsed":"0"})
            }
            "frontendOpenOrders" => self.rest_orders.clone(),
            _ => panic!("unexpected request"),
        };
        Ok(serde_json::to_vec(&value)?)
    }
}
#[tokio::test]
async fn large_scope_checks_every_dex_and_rechecks_native_and_orphan_orders() {
    struct WithOrphan(LargeFixture, OpenOrder);
    impl InfoTransport for WithOrphan {
        fn network(&self) -> Network {
            Network::Testnet
        }
        async fn portfolio_scope(
            &self,
            a: &str,
            d: &[String],
        ) -> std::result::Result<Value, crate::info::BoxError> {
            self.0.portfolio_scope(a, d).await
        }
        async fn post(&self, b: &[u8]) -> std::result::Result<Vec<u8>, crate::info::BoxError> {
            let r: Value = serde_json::from_slice(b)?;
            if r["type"] == "frontendOpenOrders" && r["dex"] == "dex282" {
                return Ok(serde_json::to_vec(&vec![self.1.clone()])?);
            }
            self.0.post(b).await
        }
    }
    let mut f = LargeFixture::new(283);
    f.rows[282]["orders"] = json!([{"coin":"dex282:EXAMPLE","oid":1}]);
    let mut orphan = order(1, true, true);
    orphan.coin = "dex282:EXAMPLE".into();
    // Native remains flat; use a request-sensitive wrapper for the active builder.
    let c = InfoClient::new(WithOrphan(f, orphan));
    let s = snapshot(&c, ADDRESS).await.unwrap();
    assert_eq!(s.dexs.len(), 283);
    assert_eq!(s.dexs[282].orphan_trigger_ids, vec![1]);
    assert!(!s.exit_coverage_observed());
    assert!(s.scope.contains("fresh_stream_bootstrap_and_active_rest"));
    assert_eq!(c.transport().0.reads.lock().unwrap().len(), 8); // discovery, balance, five REST, discovery
}
#[tokio::test]
async fn large_scope_refuses_incomplete_stale_changed_and_mismatched_evidence() {
    for case in 0..10 {
        let mut f = LargeFixture::new(283);
        match case {
            0 => {
                f.rows.as_array_mut().unwrap().pop();
            }
            1 => f.rows[282]["dex"] = json!(""),
            2 => f.rows[282]["before"]["time"] = json!(1),
            3 => f.rows[282]["state"]["time"] = json!(now_ms().unwrap() + 60_000),
            4 => {
                let mut p = position(true);
                p.coin = "dex282:EXAMPLE".into();
                f.rows[282]["state"]["assetPositions"] = json!([{"position":p}]);
            }
            5 => f.rows[282]["orders"] = json!([{"coin":"dex282:A","oid":1}]), // REST lacks order
            6 => f.rows[282]["orders"] = json!([{"coin":"wrong:A","oid":1}]),
            7 => f.changed_discovery = true,
            8 => {
                f.rows[282]["orders"] =
                    json!([{"coin":"dex282:A","oid":1},{"coin":"dex282:A","oid":1}]);
            }
            _ => {
                let mut p = position(true);
                p.coin = "dex282:EXAMPLE".into();
                f.rows[282]["state"]["assetPositions"] = json!([{"position":p}]);
                f.rows[282]["before"] = f.rows[282]["state"].clone(); // stable stream, REST flat
            }
        }
        assert!(
            snapshot(&InfoClient::new(f), ADDRESS).await.is_err(),
            "case {case}"
        );
    }
}
#[tokio::test]
async fn discovery_budget_is_bounded_at_512() {
    assert_eq!(
        snapshot(&InfoClient::new(LargeFixture::new(512)), ADDRESS)
            .await
            .unwrap()
            .dexs
            .len(),
        512
    );
    assert!(
        snapshot(&InfoClient::new(LargeFixture::new(513)), ADDRESS)
            .await
            .is_err()
    );
}
