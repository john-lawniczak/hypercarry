//! Testnet-only adapter for the bounded recovery driver. No owner-key loader,
//! account-wide cancel, entry action, mainnet constructor or implicit retries.
#![allow(missing_docs)]
use super::{
    Clock, HyperliquidL1Signer, HyperliquidSigningRequest, HyperliquidTestnetConfig,
    HyperliquidTransport, validate_l1_signer, validate_signed_request,
};
use hypercarry_execution::{
    ExecutionError, ExecutionNetwork, Side,
    bracket::{EntryOutcome, ExitObservation},
    recovery::{
        ObservedOrderState, OrderEvidence, RecoveryAction, RecoveryPolicy, RecoverySnapshot,
        RecoveryVenue, ReservedAction, SubmitOutcome,
    },
};
use hypercarry_notifications::{lock_private, private_dir, read_private, write_private};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::{
    fs::File,
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, ExecutionError>;
fn invalid(message: &str) -> ExecutionError {
    ExecutionError::Validation(message.into())
}
fn check(ok: bool, message: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(invalid(message)) }
}
fn number(value: &Value) -> Result<Decimal> {
    value
        .as_str()
        .ok_or_else(|| invalid("missing decimal"))?
        .parse()
        .map_err(|_| invalid("invalid decimal"))
}
/// Scoped host adapter. The host must pass the SAME private operations directory
/// to all local executors for this account; existing entry risk checks must use
/// `entry_gate_path()`. The lock is held throughout the adapter's lifetime.
pub struct TestnetRecoveryAdapter<T, S, C> {
    policy: RecoveryPolicy,
    config: HyperliquidTestnetConfig,
    asset_id: u32,
    transport: T,
    signer: S,
    clock: C,
    gate: PathBuf,
    _account_lock: File,
    last_snapshot: Option<RecoverySnapshot>,
    agent_valid_until_ms: Option<u64>,
    last_book_time_ms: Option<u64>,
    last_nonce: u64,
    nonce_path: PathBuf,
}
impl<T: HyperliquidTransport, S: HyperliquidL1Signer, C: Clock> TestnetRecoveryAdapter<T, S, C> {
    /// # Errors
    /// Rejects mismatched scope, owner signer, unsafe paths or a competing writer.
    pub fn new(
        policy: RecoveryPolicy,
        config: HyperliquidTestnetConfig,
        asset_id: u32,
        transport: T,
        signer: S,
        clock: C,
        operations_dir: &Path,
    ) -> Result<Self> {
        policy.validate()?;
        check(
            policy.spec.network == ExecutionNetwork::Testnet
                && config.network == ExecutionNetwork::Testnet
                && policy.spec.account == config.account_address(),
            "recovery adapter is testnet-only and account-bound",
        )?;
        check(
            config.authorized_signer_address() != config.account_address(),
            "recovery requires a dedicated approved agent",
        )?;
        validate_l1_signer(&signer, &config)?;
        private_dir(operations_dir)
            .map_err(|_| invalid("private operations directory required"))?;
        let scope = format!("testnet-{}", config.account_address());
        let lock = lock_private(&operations_dir.join(format!("{scope}.lock")))
            .map_err(|_| invalid("account recovery lock unavailable"))?;
        check(
            !operations_dir
                .join(format!("{scope}-pending.json"))
                .try_exists()
                .map_err(|_| invalid("cannot inspect local account reservation"))?,
            "reconcile the local execution reservation before recovery handoff",
        )?;
        let gate = operations_dir.join(format!("{scope}.entries-disabled"));
        let nonce_path = operations_dir.join(format!("{scope}.nonces.json"));
        let last_nonce = if nonce_path.symlink_metadata().is_ok() {
            let state: Value = serde_json::from_slice(
                &read_private(&nonce_path).map_err(|_| invalid("invalid nonce state"))?,
            )?;
            check(
                state["schema_version"] == 1
                    && state["network"] == "testnet"
                    && state["account"] == config.account_address()
                    && state["signer"] == config.authorized_signer_address(),
                "nonce state scope mismatch",
            )?;
            state["last_nonce"]
                .as_u64()
                .ok_or_else(|| invalid("missing durable nonce"))?
        } else {
            0
        };
        Ok(Self {
            policy,
            config,
            asset_id,
            transport,
            signer,
            clock,
            gate,
            _account_lock: lock,
            last_snapshot: None,
            agent_valid_until_ms: None,
            last_book_time_ms: None,
            last_nonce,
            nonce_path,
        })
    }
    pub fn entry_gate_path(&self) -> &Path {
        &self.gate
    }
    fn read(&mut self, body: &Value) -> Result<Value> {
        self.transport
            .info(body)
            .map_err(|_| ExecutionError::Transport("recovery REST read failed".into()))
    }
    fn order(&mut self, cloid: &str) -> Result<Value> {
        let account = self.policy.spec.account.clone();
        self.read(&json!({"type":"orderStatus","user":account,"oid":cloid}))
    }
    fn send_checked(
        &mut self,
        policy: &RecoveryPolicy,
        reserved: &ReservedAction,
    ) -> Result<SubmitOutcome> {
        check(policy == &self.policy, "recovery policy changed")?;
        self.freeze_entries()?;
        let now = self.now_ms()?;
        let observation = self
            .last_snapshot
            .take()
            .ok_or_else(|| invalid("fresh snapshot required before each write"))?;
        check(
            observation.agent_authorized
                && self
                    .agent_valid_until_ms
                    .is_some_and(|until| until > reserved.expires_at_ms)
                && self
                    .last_book_time_ms
                    .is_some_and(|at| at <= now && now - at <= policy.limits.max_snapshot_age_ms)
                && observation.complete
                && now >= observation.completed_at_ms
                && now.saturating_sub(observation.started_at_ms)
                    <= policy.limits.max_snapshot_age_ms
                && reserved.nonce > self.last_nonce
                && reserved.nonce >= observation.completed_at_ms
                && reserved.nonce <= now.saturating_add(1000)
                && reserved.expires_at_ms > now
                && reserved.expires_at_ms <= now.saturating_add(policy.limits.action_ttl_ms),
            "stale recovery write or nonce",
        )?;
        let action = recovery_wire(policy, reserved, &observation, self.asset_id)?;
        let request = HyperliquidSigningRequest {
            network: ExecutionNetwork::Testnet,
            nonce: reserved.nonce,
            expires_after: reserved.expires_at_ms,
            action,
        };
        // The engine has persisted the reservation. The adapter never retries it.
        write_private(
            &self.nonce_path,
            &json!({"schema_version":1,"network":"testnet","account":self.config.account_address(),
            "signer":self.config.authorized_signer_address(),"last_nonce":reserved.nonce}),
        )
        .map_err(|_| invalid("cannot reserve nonce durably"))?;
        self.last_nonce = reserved.nonce;
        let signed = self
            .signer
            .sign_l1_action(&request)
            .map_err(|_| invalid("recovery signer refused request"))?;
        validate_signed_request(&signed, &request)?;
        verify_signature(&signed, &request, self.config.authorized_signer_address())?;
        let signed_at = self.now_ms()?;
        check(
            signed_at >= now
                && signed_at < reserved.expires_at_ms
                && signed_at.saturating_sub(observation.started_at_ms)
                    <= policy.limits.max_snapshot_age_ms
                && self.last_book_time_ms.is_some_and(|at| {
                    at <= signed_at && signed_at - at <= policy.limits.max_snapshot_age_ms
                }),
            "recovery signature expired",
        )?;
        let fresh = self.snapshot(policy, Some(reserved))?;
        check(
            fresh.agent_authorized
                && fresh.complete
                && fresh.unmanaged_orders == 0
                && self.now_ms()? < reserved.expires_at_ms
                && fresh.completed_at_ms.saturating_sub(fresh.started_at_ms)
                    <= policy.limits.max_snapshot_age_ms
                && recovery_wire(policy, reserved, &fresh, self.asset_id)? == *request.action(),
            "recovery scope changed while signing",
        )?;
        self.last_snapshot = None;
        Ok(match self.transport.exchange(&signed) {
            Ok(value) => submission_outcome(
                &value,
                matches!(reserved.action, RecoveryAction::CloseRemaining { .. }),
            ),
            Err(_) => SubmitOutcome::Unknown,
        })
    }
}
impl<T: HyperliquidTransport, S: HyperliquidL1Signer, C: Clock> RecoveryVenue
    for TestnetRecoveryAdapter<T, S, C>
{
    fn freeze_entries(&mut self) -> Result<()> {
        write_private(
            &self.gate,
            &json!({"entries_disabled":true,"network":"testnet"}),
        )
        .map_err(|_| invalid("could not durably freeze entries"))
    }
    fn now_ms(&self) -> Result<u64> {
        u64::try_from(self.clock.now_ms()?).map_err(|_| invalid("invalid clock"))
    }
    #[allow(clippy::too_many_lines)]
    fn snapshot(
        &mut self,
        policy: &RecoveryPolicy,
        pending: Option<&ReservedAction>,
    ) -> Result<RecoverySnapshot> {
        check(policy == &self.policy, "recovery policy changed")?;
        self.last_snapshot = None;
        self.agent_valid_until_ms = None;
        self.last_book_time_ms = None;
        let started = self.now_ms()?;
        let spec = policy.spec.clone();
        let agents = self.read(&json!({"type":"extraAgents","user":spec.account}))?;
        let valid_until = agents
            .as_array()
            .ok_or_else(|| invalid("invalid agent registry"))?
            .iter()
            .filter(|a| {
                a["address"].as_str().is_some_and(|v| {
                    v.eq_ignore_ascii_case(self.config.authorized_signer_address())
                })
            })
            .filter_map(|a| a["validUntil"].as_u64())
            .max();
        let discovery = self.read(&json!({"type":"perpDexs"}))?;
        let dexes = dex_names(&discovery)?;
        let scope = if dexes.len() > 20 {
            Some(
                self.transport
                    .recovery_scope(&spec.account, &dexes)?
                    .ok_or_else(|| {
                        invalid("large recovery scope requires complete fresh subscriptions")
                    })?,
            )
        } else {
            None
        };
        let dex = spec.coin.split_once(':').map_or("", |(dex, _)| dex);
        let dex_index = dexes
            .iter()
            .position(|name| name == dex)
            .ok_or_else(|| invalid("unknown recovery DEX"))?;
        let meta = self.read(&json!({"type":"metaAndAssetCtxs","dex":dex}))?;
        let universe = meta[0]["universe"]
            .as_array()
            .ok_or_else(|| invalid("invalid universe"))?;
        let matches: Vec<_> = universe
            .iter()
            .enumerate()
            .filter(|(_, a)| a["name"] == spec.coin)
            .collect();
        check(matches.len() == 1, "missing or duplicate recovery market")?;
        let (index, asset) = matches[0];
        let expected = if dex_index == 0 {
            index
        } else {
            100_000 + dex_index * 10_000 + index
        };
        check(
            usize::try_from(self.asset_id).ok() == Some(expected)
                && index < 10_000
                && asset["isDelisted"] != true
                && (meta[0]["collateralToken"] == 0
                    || (dex_index == 0 && meta[0].get("collateralToken").is_none())),
            "asset index changed or delisted",
        )?;
        let decimals = asset["szDecimals"]
            .as_u64()
            .filter(|d| *d <= 6)
            .ok_or_else(|| invalid("invalid size precision"))?;
        check(
            policy.limits.size_step
                == Decimal::new(
                    1,
                    u32::try_from(decimals).map_err(|_| invalid("size precision"))?,
                ),
            "size precision changed",
        )?;
        let mark = number(&meta[1][index]["markPx"])?;
        let book = self.read(&json!({"type":"l2Book","coin":spec.coin}))?;
        check(book["coin"] == spec.coin, "wrong book")?;
        let book_time = book["time"]
            .as_u64()
            .ok_or_else(|| invalid("missing book time"))?;
        check(
            book_time <= self.now_ms()?
                && started.saturating_sub(book_time) <= policy.limits.max_snapshot_age_ms,
            "stale book",
        )?;
        let bid = number(&book["levels"][0][0]["px"])?;
        let ask = number(&book["levels"][1][0]["px"])?;
        let entry = self.order(&spec.entry_cloid)?;
        let tp = self.order(policy.tp_cloid())?;
        let sl = self.order(policy.sl_cloid())?;
        let entry_state = order_evidence(&entry, &spec.entry_cloid, &spec.coin)?;
        let tp_state = order_evidence(&tp, policy.tp_cloid(), &spec.coin)?;
        let sl_state = order_evidence(&sl, policy.sl_cloid(), &spec.coin)?;
        let pending_state = if let Some(ReservedAction {
            action: RecoveryAction::CloseRemaining { cloid, .. },
            ..
        }) = pending
        {
            let response = self.order(cloid)?;
            Some(order_evidence(&response, cloid, &spec.coin)?)
        } else {
            None
        };
        let orders =
            self.read(&json!({"type":"frontendOpenOrders","user":spec.account,"dex":dex}))?;
        let rows = orders
            .as_array()
            .ok_or_else(|| invalid("invalid open orders"))?;
        let known_oids: Vec<_> = [&entry, &tp, &sl]
            .iter()
            .filter_map(|v| v["order"]["order"]["oid"].as_u64())
            .collect();
        let mut unmanaged = 0_u32;
        for row in rows {
            let oid = row["oid"]
                .as_u64()
                .ok_or_else(|| invalid("missing open order id"))?;
            // IOC must not remain open; any resting fallback is unresolved and
            // therefore blocks another action rather than being silently adopted.
            if !known_oids.contains(&oid) {
                unmanaged = unmanaged
                    .checked_add(1)
                    .ok_or_else(|| invalid("order count overflow"))?;
            }
        }
        let exit = |response: &Value,
                    evidence: &OrderEvidence,
                    take_profit: bool|
         -> Result<Option<ExitObservation>> {
            let oid = response["order"]["order"]["oid"].as_u64();
            let row = rows
                .iter()
                .find(|r| oid.is_some() && r["oid"].as_u64() == oid);
            if let Some(row) = row {
                check(
                    evidence.state == ObservedOrderState::Open && row["coin"] == spec.coin,
                    "inconsistent exit status",
                )?;
                Ok(Some(ExitObservation {
                    cloid: evidence.cloid.clone(),
                    coin: spec.coin.clone(),
                    side: match row["side"].as_str() {
                        Some("B") => Side::Buy,
                        Some("A") => Side::Sell,
                        _ => return Err(invalid("invalid exit side")),
                    },
                    quantity: number(&row["sz"])?,
                    trigger: number(&row["triggerPx"])?,
                    reduce_only: row["reduceOnly"]
                        .as_bool()
                        .ok_or_else(|| invalid("missing reduce-only flag"))?,
                    is_trigger: row["isTrigger"]
                        .as_bool()
                        .ok_or_else(|| invalid("missing trigger flag"))?,
                    is_take_profit: take_profit
                        && row["orderType"]
                            .as_str()
                            .is_some_and(|t| t.starts_with("Take Profit"))
                        || !take_profit
                            && !row["orderType"]
                                .as_str()
                                .is_some_and(|t| t.starts_with("Stop")),
                }))
            } else {
                check(
                    evidence.state != ObservedOrderState::Open
                        || response["order"]["status"] == "triggered",
                    "open exit missing from complete order list",
                )?;
                // Triggered is not proof of a fill. The trigger may have left
                // this list with exposure remaining; reconcile the position
                // below and retain the stop_fired signal. Any unknown child
                // order still fails the unmanaged-order guard.
                Ok(None)
            }
        };
        let entry_oid = entry["order"]["order"]["oid"].as_u64();
        let entry_open = rows
            .iter()
            .any(|row| entry_oid.is_some() && row["oid"].as_u64() == entry_oid);
        check(
            entry_open == (entry_state.state == ObservedOrderState::Open),
            "inconsistent entry status/open-order list",
        )?;
        let tp_observation = exit(&tp, &tp_state, true)?;
        let sl_observation = exit(&sl, &sl_state, false)?;
        let positions =
            self.read(&json!({"type":"clearinghouseState","user":spec.account,"dex":dex}))?;
        let position_rows = positions["assetPositions"]
            .as_array()
            .ok_or_else(|| invalid("invalid positions"))?;
        let mut quantity = Decimal::ZERO;
        let mut seen = false;
        for row in position_rows {
            let position = &row["position"];
            let q = number(&position["szi"])?;
            if position["coin"] == spec.coin {
                check(!seen, "duplicate position")?;
                seen = true;
                quantity = q;
                check(
                    q.is_zero()
                        || (position["leverage"]["type"] == "isolated"
                            && position["leverage"]["value"] == 1),
                    "unexpected position margin mode/leverage",
                )?;
            } else if !q.is_zero() {
                unmanaged = unmanaged
                    .checked_add(1)
                    .ok_or_else(|| invalid("position count overflow"))?;
            }
        }
        // Account scope includes every discovered DEX. Unrelated exposure/orders
        // block this single-bracket recovery profile, including obsolete exits.
        if let Some(scope) = scope {
            let scope_rows = scope["dexs"]
                .as_array()
                .ok_or_else(|| invalid("missing recovery scope"))?;
            check(scope_rows.len() == dexes.len(), "incomplete recovery scope")?;
            let mut seen = std::collections::BTreeSet::new();
            for row in scope_rows {
                let name = row["dex"]
                    .as_str()
                    .ok_or_else(|| invalid("missing scope DEX"))?;
                check(
                    dexes.iter().any(|d| d == name) && seen.insert(name),
                    "duplicate or unknown scope DEX",
                )?;
                let scoped_orders = row["orders"]
                    .as_array()
                    .ok_or_else(|| invalid("missing scope orders"))?;
                if name == dex {
                    check(
                        super::scope::identities(&row["state"])?
                            == super::scope::identities(&positions)?
                            && scoped_orders
                                .iter()
                                .map(|o| o["oid"].as_u64())
                                .collect::<std::collections::BTreeSet<_>>()
                                == rows
                                    .iter()
                                    .map(|o| o["oid"].as_u64())
                                    .collect::<std::collections::BTreeSet<_>>(),
                        "scope/REST target changed",
                    )?;
                } else {
                    let mut count = scoped_orders.len();
                    for p in row["state"]["assetPositions"]
                        .as_array()
                        .ok_or_else(|| invalid("missing scope positions"))?
                    {
                        count += usize::from(!number(&p["position"]["szi"])?.is_zero());
                    }
                    unmanaged = unmanaged
                        .checked_add(
                            u32::try_from(count).map_err(|_| invalid("scope count overflow"))?,
                        )
                        .ok_or_else(|| invalid("scope count overflow"))?;
                }
            }
        } else {
            for other in dexes.iter().filter(|name| *name != dex) {
                let orders = self
                    .read(&json!({"type":"frontendOpenOrders","user":spec.account,"dex":other}))?;
                let positions = self
                    .read(&json!({"type":"clearinghouseState","user":spec.account,"dex":other}))?;
                let rows = orders
                    .as_array()
                    .ok_or_else(|| invalid("invalid other DEX orders"))?;
                let mut count = rows.len();
                for row in positions["assetPositions"]
                    .as_array()
                    .ok_or_else(|| invalid("invalid other DEX positions"))?
                {
                    count += usize::from(!number(&row["position"]["szi"])?.is_zero());
                }
                unmanaged = unmanaged
                    .checked_add(u32::try_from(count).map_err(|_| invalid("order count overflow"))?)
                    .ok_or_else(|| invalid("order count overflow"))?;
            }
        }
        check(
            dex_names(&self.read(&json!({"type":"perpDexs"}))?)? == dexes,
            "DEX discovery changed",
        )?;
        let completed = self.now_ms()?;
        check(
            book_time <= completed && completed - book_time <= policy.limits.max_snapshot_age_ms,
            "book expired during reconciliation",
        )?;
        let result = RecoverySnapshot {
            network: ExecutionNetwork::Testnet,
            account: spec.account,
            coin: spec.coin,
            started_at_ms: started,
            completed_at_ms: completed,
            complete: true,
            agent_authorized: valid_until
                .is_some_and(|until| until > completed.saturating_add(policy.limits.action_ttl_ms)),
            entry: match entry_state.state {
                ObservedOrderState::Open => EntryOutcome::Pending,
                ObservedOrderState::Terminal => EntryOutcome::Terminal,
                ObservedOrderState::Unknown => EntryOutcome::Unknown,
            },
            signed_position: quantity,
            mark,
            bid,
            ask,
            tp: tp_observation,
            sl: sl_observation,
            stop_fired: matches!(sl["order"]["status"].as_str(), Some("triggered" | "filled")),
            unmanaged_orders: unmanaged,
            pending: pending_state,
        };
        self.last_snapshot = Some(result.clone());
        self.agent_valid_until_ms = valid_until;
        self.last_book_time_ms = Some(book_time);
        Ok(result)
    }
    fn submit(&mut self, policy: &RecoveryPolicy, action: &ReservedAction) -> SubmitOutcome {
        // Any local validation/signing failure occurs before exchange and is a
        // definite rejection. Transport ambiguity is returned by send_checked.
        self.send_checked(policy, action)
            .unwrap_or(SubmitOutcome::Rejected)
    }
}
fn dex_names(value: &Value) -> Result<Vec<String>> {
    let rows = value
        .as_array()
        .ok_or_else(|| invalid("invalid DEX discovery"))?;
    check(
        !rows.is_empty() && rows.len() <= 512 && rows[0].is_null(),
        "invalid native DEX",
    )?;
    let mut names = vec![String::new()];
    for row in &rows[1..] {
        let name = row["name"]
            .as_str()
            .ok_or_else(|| invalid("missing DEX name"))?;
        check(
            !name.is_empty()
                && name.len() <= 64
                && !name.chars().any(char::is_control)
                && !names.iter().any(|n| n == name),
            "invalid or duplicate DEX name",
        )?;
        names.push(name.into());
    }
    Ok(names)
}
fn order_evidence(value: &Value, cloid: &str, coin: &str) -> Result<OrderEvidence> {
    if value["status"] == "unknownOid" {
        return Ok(OrderEvidence {
            cloid: cloid.into(),
            state: ObservedOrderState::Unknown,
        });
    }
    check(
        value["status"] == "order"
            && value["order"]["order"]["cloid"] == cloid
            && value["order"]["order"]["coin"] == coin,
        "order status scope mismatch",
    )?;
    let status = value["order"]["status"]
        .as_str()
        .ok_or_else(|| invalid("missing order status"))?;
    let state = match status {
        "open" | "triggered" => ObservedOrderState::Open,
        "filled"
        | "canceled"
        | "marginCanceled"
        | "vaultWithdrawalCanceled"
        | "openInterestCapCanceled"
        | "selfTradeCanceled"
        | "reduceOnlyCanceled"
        | "siblingFilledCanceled"
        | "delistedCanceled"
        | "liquidatedCanceled"
        | "scheduledCancel"
        | "rejected"
        | "tickRejected"
        | "minTradeNtlRejected"
        | "perpMarginRejected"
        | "reduceOnlyRejected"
        | "iocCancelRejected"
        | "badAloPxRejected"
        | "badTriggerPxRejected"
        | "marketOrderNoLiquidityRejected"
        | "positionIncreaseAtOpenInterestCapRejected"
        | "positionFlipAtOpenInterestCapRejected"
        | "tooAggressiveAtOpenInterestCapRejected"
        | "openInterestIncreaseRejected"
        | "insufficientSpotBalanceRejected"
        | "oracleRejected"
        | "perpMaxPositionRejected" => ObservedOrderState::Terminal,
        _ => ObservedOrderState::Unknown,
    };
    Ok(OrderEvidence {
        cloid: cloid.into(),
        state,
    })
}
fn recovery_wire(
    policy: &RecoveryPolicy,
    r: &ReservedAction,
    o: &RecoverySnapshot,
    asset: u32,
) -> Result<Value> {
    let spec = &policy.spec;
    match &r.action {
        RecoveryAction::CancelOwned { cloid } => {
            check(
                *cloid == spec.entry_cloid
                    || (o.signed_position.is_zero()
                        && o.entry == EntryOutcome::Terminal
                        && (cloid == policy.tp_cloid() || cloid == policy.sl_cloid())),
                "cannot cancel protective exits while exposed or cancel unowned order",
            )?;
            Ok(json!({"type":"cancelByCloid","cancels":[{"asset":asset,"cloid":cloid}]}))
        }
        RecoveryAction::CloseRemaining {
            cloid,
            side,
            quantity,
            limit_price,
        } => {
            let l = &policy.limits;
            check(
                o.entry == EntryOutcome::Terminal
                    && o.unmanaged_orders == 0
                    && *side != spec.entry_side
                    && *quantity > Decimal::ZERO
                    && *quantity <= o.signed_position.abs()
                    && *quantity <= spec.quantity
                    && *limit_price >= l.min_price
                    && *limit_price <= l.max_price
                    && quantity.checked_rem(l.size_step) == Some(Decimal::ZERO)
                    && limit_price.checked_rem(l.price_tick) == Some(Decimal::ZERO),
                "close exceeds scope/price/quantity policy",
            )?;
            check(
                (o.signed_position < Decimal::ZERO && *side == Side::Buy)
                    || (o.signed_position > Decimal::ZERO && *side == Side::Sell),
                "close direction would increase risk",
            )?;
            let slip = Decimal::from(l.slippage_bps) / Decimal::from(10_000);
            let collar = if *side == Side::Buy {
                o.ask.checked_mul(Decimal::ONE + slip)
            } else {
                o.bid.checked_mul(Decimal::ONE - slip)
            }
            .ok_or_else(|| invalid("close slippage collar overflow"))?;
            check(
                o.bid > Decimal::ZERO
                    && o.ask >= o.bid
                    && if *side == Side::Buy {
                        *limit_price >= o.ask && *limit_price <= collar
                    } else {
                        *limit_price <= o.bid && *limit_price >= collar
                    },
                "close is nonmarketable or exceeds slippage policy",
            )?;
            let size_decimals = l.size_step.normalize().scale();
            let rounded = hypercarry_core::trade_plan::price_round(
                *limit_price,
                size_decimals,
                *side == Side::Sell,
            )
            .map_err(|_| invalid("unrepresentable close price"))?;
            check(
                rounded == *limit_price,
                "price violates Hyperliquid precision",
            )?;
            Ok(
                json!({"type":"order","orders":[{"a":asset,"b":*side==Side::Buy,"p":limit_price.normalize().to_string(),"s":quantity.normalize().to_string(),"r":true,"t":{"limit":{"tif":"Ioc"}},"c":cloid}],"grouping":"na"}),
            )
        }
    }
}
fn submission_outcome(value: &Value, order: bool) -> SubmitOutcome {
    if value["status"] == "err" {
        return SubmitOutcome::Rejected;
    }
    if value["status"] != "ok"
        || value["response"]["type"] != if order { "order" } else { "cancel" }
    {
        return SubmitOutcome::Unknown;
    }
    let Some(rows) = value["response"]["data"]["statuses"]
        .as_array()
        .filter(|s| s.len() == 1)
    else {
        return SubmitOutcome::Unknown;
    };
    if rows[0].get("error").and_then(Value::as_str).is_some() {
        SubmitOutcome::Rejected
    } else if rows[0] == "success"
        || rows[0].get("filled").is_some()
        || rows[0].get("resting").is_some()
    {
        SubmitOutcome::Accepted
    } else {
        SubmitOutcome::Unknown
    }
}
fn verify_signature(
    signed: &Value,
    request: &HyperliquidSigningRequest,
    address: &str,
) -> Result<()> {
    use hypercarry_signing::{Action, Chain, Signature, SignatureRecovery};
    let action: Action = serde_json::from_value(request.action().clone())?;
    let expires = hypercarry_signing::ExpiryMs::from_timestamp_millis(
        i64::try_from(request.expires_after()).map_err(|_| invalid("expiry overflow"))?,
    )
    .ok_or_else(|| invalid("invalid expiry"))?;
    let hash = action
        .prehash(request.nonce(), None, Some(expires), Chain::Testnet)
        .map_err(|_| invalid("cannot hash recovery action"))?;
    let sig = &signed["signature"];
    let r = sig["r"]
        .as_str()
        .ok_or_else(|| invalid("missing signature"))?;
    let s = sig["s"]
        .as_str()
        .ok_or_else(|| invalid("missing signature"))?;
    let v = sig["v"]
        .as_u64()
        .ok_or_else(|| invalid("missing recovery id"))?;
    check(v == 27 || v == 28, "invalid recovery id")?;
    let signature: Signature = format!("{r}{}{:02x}", &s[2..], v)
        .parse()
        .map_err(|_| invalid("invalid signature"))?;
    let recovered = signature
        .recover_address_from_prehash(&hash)
        .map_err(|_| invalid("signature recovery failed"))?
        .to_string();
    check(
        recovered.eq_ignore_ascii_case(address),
        "wrong recovery signer",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::venue::{
        LocalTestnetSigner, TESTNET_ACKNOWLEDGEMENT, TestnetAcknowledgement, TransportFailure,
    };
    use hypercarry_execution::{SubmissionFailure, bracket::BracketSpec, recovery::RecoveryLimits};
    use std::{cell::RefCell, os::unix::fs::PermissionsExt, rc::Rc};
    fn d(value: &str) -> Decimal {
        value.parse().unwrap()
    }
    fn policy() -> RecoveryPolicy {
        RecoveryPolicy {
            repaired_exits: None,
            spec: BracketSpec {
                trade_id: "adapter-fixture".into(),
                plan_digest: "aa".repeat(32),
                network: ExecutionNetwork::Testnet,
                account: format!("0x{}", "12".repeat(20)),
                coin: "EXAMPLE".into(),
                entry_side: Side::Sell,
                quantity: d("1.1"),
                tp: d("70"),
                sl: d("95"),
                entry_cloid: format!("0x{}", "11".repeat(16)),
                tp_cloid: format!("0x{}", "22".repeat(16)),
                sl_cloid: format!("0x{}", "33".repeat(16)),
            },
            limits: RecoveryLimits {
                size_step: d("0.01"),
                price_tick: d("0.01"),
                min_price: d("50"),
                max_price: d("100"),
                slippage_bps: 50,
                stop_grace_ms: 0,
                max_snapshot_age_ms: 1000,
                max_close_attempts: 3,
                max_cancel_attempts: 4,
                retry_ms: 1000,
                recovery_timeout_ms: 30_000,
                action_ttl_ms: 1000,
            },
        }
    }
    fn order(cloid: &str, oid: u64, status: &str) -> Value {
        json!({"status":"order","order":{"status":status,"order":{"coin":"EXAMPLE","cloid":cloid,"oid":oid}}})
    }
    fn row(oid: u64, tp: bool) -> Value {
        json!({"coin":"EXAMPLE","oid":oid,"side":"B","sz":"1.1","triggerPx":if tp{"70"}else{"95"},"reduceOnly":true,"isTrigger":true,"orderType":if tp{"Take Profit Market"}else{"Stop Market"}})
    }
    #[allow(clippy::struct_excessive_bools)] // Independent fault-injection switches.
    struct Fixture {
        builder: bool,
        repaired: bool,
        other_exposure: bool,
        agent: String,
        requests: Vec<Value>,
        signed: Vec<Value>,
        fail_exchange: bool,
        revoked: bool,
        valid_until: u64,
        now: i64,
        book_time: u64,
        stop_status: &'static str,
        stop_listed: bool,
    }
    #[derive(Clone)]
    struct Transport(Rc<RefCell<Fixture>>);
    impl HyperliquidTransport for Transport {
        fn exchange(&mut self, request: &Value) -> std::result::Result<Value, TransportFailure> {
            let mut f = self.0.borrow_mut();
            f.signed.push(request.clone());
            if f.fail_exchange {
                Err(TransportFailure {
                    classification: SubmissionFailure::UncertainAfterWrite,
                    operation: "fixture",
                    status: None,
                })
            } else {
                Ok(
                    json!({"status":"ok","response":{"type":"order","data":{"statuses":[{"filled":{"oid":99,"totalSz":"0.7","avgPx":"96"}}]}}}),
                )
            }
        }
        fn info(&mut self, request: &Value) -> std::result::Result<Value, TransportFailure> {
            let mut f = self.0.borrow_mut();
            f.requests.push(request.clone());
            let mut p = policy();
            if f.builder {
                p.spec.coin = "test:EXAMPLE".into();
            }
            if f.repaired {
                p.repaired_exits = Some(hypercarry_execution::recovery::RecoveryExitIds {
                    tp: format!("0x{}", "55".repeat(16)),
                    sl: format!("0x{}", "66".repeat(16)),
                });
            }
            let dex = if f.builder { "test" } else { "" };
            let scoped = request["dex"].as_str().unwrap_or("") == dex;
            Ok(match request["type"].as_str().unwrap() {
                "perpDexs" => {
                    if f.builder {
                        json!([null,{"name":"test"}])
                    } else {
                        json!([null])
                    }
                }
                "extraAgents" => {
                    json!([{"address":f.agent,"validUntil":if f.revoked{1}else{f.valid_until}}])
                }
                "metaAndAssetCtxs" => {
                    json!([{"universe":[{"name":p.spec.coin,"szDecimals":2}],"collateralToken":0},[{"markPx":"96"}]])
                }
                "l2Book" => {
                    json!({"coin":p.spec.coin,"time":f.book_time,"levels":[[{"px":"95.99"}],[{"px":"96"}]]})
                }
                "orderStatus" => {
                    let id = request["oid"].as_str().unwrap();
                    let mut result = if id == p.spec.entry_cloid {
                        order(id, 1, "filled")
                    } else if id == p.tp_cloid() {
                        order(id, 2, "open")
                    } else if id == p.sl_cloid() {
                        order(id, 3, f.stop_status)
                    } else {
                        json!({"status":"unknownOid"})
                    };
                    if result["status"] == "order" {
                        result["order"]["order"]["coin"] = json!(p.spec.coin);
                    }
                    result
                }
                "frontendOpenOrders" => {
                    let mut rows = if !scoped {
                        json!([])
                    } else if f.stop_listed {
                        json!([row(2, true), row(3, false)])
                    } else {
                        json!([row(2, true)])
                    };
                    for r in rows.as_array_mut().unwrap() {
                        r["coin"] = json!(p.spec.coin);
                    }
                    rows
                }
                "clearinghouseState" => {
                    if scoped {
                        json!({"assetPositions":[{"position":{"coin":p.spec.coin,"szi":"-1.1","leverage":{"type":"isolated","value":1}}}]})
                    } else if f.other_exposure {
                        json!({"assetPositions":[{"position":{"coin":"BTC","szi":"1"}}]})
                    } else {
                        json!({"assetPositions":[]})
                    }
                }
                _ => panic!("unexpected fixture request"),
            })
        }
    }
    #[derive(Clone, Copy)]
    struct FixedClock;
    impl Clock for FixedClock {
        fn now_ms(&self) -> Result<i64> {
            Ok(2000)
        }
    }
    #[derive(Clone)]
    struct FixtureClock(Rc<RefCell<Fixture>>);
    impl Clock for FixtureClock {
        fn now_ms(&self) -> Result<i64> {
            Ok(self.0.borrow().now)
        }
    }
    type Adapter = TestnetRecoveryAdapter<
        Transport,
        LocalTestnetSigner<hypercarry_signing::SigningKey>,
        FixtureClock,
    >;
    fn fixture() -> (tempfile::TempDir, Adapter, Rc<RefCell<Fixture>>) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        // Deliberately insecure deterministic TEST FIXTURE. Never a real wallet.
        let signer = LocalTestnetSigner::new(
            hypercarry_signing::SigningKey::from_slice(&[7; 32]).unwrap(),
            "fixture/recovery",
        )
        .unwrap();
        let config = HyperliquidTestnetConfig::new(
            policy().spec.account.clone(),
            1000,
            TestnetAcknowledgement::new(TESTNET_ACKNOWLEDGEMENT).unwrap(),
        )
        .unwrap()
        .with_authorized_signer_address(signer.signer_address())
        .unwrap();
        let f = Rc::new(RefCell::new(Fixture {
            builder: false,
            repaired: false,
            other_exposure: false,
            agent: signer.signer_address().into(),
            requests: vec![],
            signed: vec![],
            fail_exchange: false,
            revoked: false,
            valid_until: 100_000,
            now: 2000,
            book_time: 2000,
            stop_status: "open",
            stop_listed: true,
        }));
        let adapter = TestnetRecoveryAdapter::new(
            policy(),
            config,
            0,
            Transport(f.clone()),
            signer,
            FixtureClock(f.clone()),
            dir.path(),
        )
        .unwrap();
        (dir, adapter, f)
    }
    fn close() -> ReservedAction {
        ReservedAction {
            action: RecoveryAction::CloseRemaining {
                cloid: format!("0x{}", "44".repeat(16)),
                side: Side::Buy,
                quantity: d("1.1"),
                limit_price: d("96.48"),
            },
            nonce: 2000,
            expires_at_ms: 3000,
        }
    }
    #[test]
    fn repaired_builder_exits_use_real_ids_and_builder_asset_on_wire() {
        let (_dir, mut a, f) = fixture();
        f.borrow_mut().builder = true;
        f.borrow_mut().repaired = true;
        a.policy.spec.coin = "test:EXAMPLE".into();
        a.policy.repaired_exits = Some(hypercarry_execution::recovery::RecoveryExitIds {
            tp: format!("0x{}", "55".repeat(16)),
            sl: format!("0x{}", "66".repeat(16)),
        });
        a.asset_id = 110_000;
        let p = a.policy.clone();
        let snapshot = a.snapshot(&p, None).unwrap();
        assert_eq!(snapshot.tp.unwrap().cloid, p.tp_cloid());
        assert_eq!(snapshot.sl.unwrap().cloid, p.sl_cloid());
        assert_eq!(a.submit(&p, &close()), SubmitOutcome::Accepted);
        assert_eq!(f.borrow().signed[0]["action"]["orders"][0]["a"], 110_000);
        assert_eq!(p.spec.sl_cloid, format!("0x{}", "33".repeat(16)));
    }
    #[test]
    fn wrong_builder_asset_and_other_dex_exposure_refuse_submission() {
        let (_dir, mut a, f) = fixture();
        f.borrow_mut().builder = true;
        a.policy.spec.coin = "test:EXAMPLE".into();
        let p = a.policy.clone();
        assert!(a.snapshot(&p, None).is_err());
        a.asset_id = 110_000;
        f.borrow_mut().other_exposure = true;
        let snapshot = a.snapshot(&p, None).unwrap();
        assert_eq!(snapshot.unmanaged_orders, 1);
        assert_eq!(a.submit(&p, &close()), SubmitOutcome::Rejected);
        assert_eq!(f.borrow().signed.len(), 0);
    }
    #[test]
    fn discovery_rejects_null_or_duplicate_builders() {
        assert!(dex_names(&json!([null, null])).is_err());
        assert!(dex_names(&json!([null,{"name":"test"},{"name":"test"}])).is_err());
    }
    #[test]
    fn snapshot_queries_owner_and_signed_close_is_reduce_only_ioc_with_verified_agent() {
        let (_dir, mut a, f) = fixture();
        a.freeze_entries().unwrap();
        assert!(a.entry_gate_path().exists());
        let p = policy();
        let o = a.snapshot(&p, None).unwrap();
        assert_eq!(o.signed_position, d("-1.1"));
        assert!(o.agent_authorized);
        assert_eq!(o.unmanaged_orders, 0);
        assert!(o.tp.unwrap().is_take_profit);
        assert!(!o.sl.unwrap().is_take_profit);
        assert_eq!(a.submit(&p, &close()), SubmitOutcome::Accepted);
        let f = f.borrow();
        assert_eq!(f.signed.len(), 1);
        let wire = &f.signed[0]["action"];
        assert_eq!(wire["orders"][0]["r"], true);
        assert_eq!(wire["orders"][0]["b"], true);
        assert_eq!(wire["orders"][0]["t"]["limit"]["tif"], "Ioc");
        assert_eq!(wire["orders"][0]["s"], "1.1");
        assert!(
            f.requests
                .iter()
                .filter(|r| r.get("user").is_some())
                .all(|r| r["user"] == p.spec.account)
        );
        let nonce: Value = serde_json::from_slice(&read_private(&a.nonce_path).unwrap()).unwrap();
        assert_eq!(nonce["last_nonce"], 2000);
    }
    #[test]
    fn unknown_exchange_outcome_is_not_retried_and_requires_another_snapshot() {
        let (_dir, mut a, f) = fixture();
        f.borrow_mut().fail_exchange = true;
        a.snapshot(&policy(), None).unwrap();
        assert_eq!(a.submit(&policy(), &close()), SubmitOutcome::Unknown);
        assert_eq!(a.submit(&policy(), &close()), SubmitOutcome::Rejected);
        assert_eq!(f.borrow().signed.len(), 1);
    }
    #[test]
    fn no_snapshot_revoked_agent_wrong_direction_and_price_are_rejected_before_exchange() {
        let (_dir, mut a, f) = fixture();
        assert_eq!(a.submit(&policy(), &close()), SubmitOutcome::Rejected);
        f.borrow_mut().revoked = true;
        a.snapshot(&policy(), None).unwrap();
        assert_eq!(a.submit(&policy(), &close()), SubmitOutcome::Rejected);
        f.borrow_mut().revoked = false;
        for mode in 0..3 {
            a.snapshot(&policy(), None).unwrap();
            let mut c = close();
            if let RecoveryAction::CloseRemaining {
                side,
                quantity,
                limit_price,
                ..
            } = &mut c.action
            {
                match mode {
                    0 => *side = Side::Sell,
                    1 => *quantity = d("2"),
                    _ => *limit_price = d("101"),
                }
            }
            assert_eq!(a.submit(&policy(), &c), SubmitOutcome::Rejected);
        }
        assert_eq!(f.borrow().signed.len(), 0);
    }
    #[test]
    fn protective_or_unowned_cancel_is_refused_while_exposed() {
        let (_dir, mut a, f) = fixture();
        for id in [
            policy().spec.tp_cloid,
            policy().spec.sl_cloid,
            format!("0x{}", "55".repeat(16)),
        ] {
            a.snapshot(&policy(), None).unwrap();
            let mut c = close();
            c.action = RecoveryAction::CancelOwned { cloid: id };
            assert_eq!(a.submit(&policy(), &c), SubmitOutcome::Rejected);
        }
        assert_eq!(f.borrow().signed.len(), 0);
    }
    #[test]
    fn unknown_or_mismatched_status_is_not_terminal_evidence() {
        assert_eq!(
            order_evidence(&json!({"status":"unknownOid"}), "fixture", "EXAMPLE")
                .unwrap()
                .state,
            ObservedOrderState::Unknown
        );
        assert!(order_evidence(&order("different", 1, "filled"), "fixture", "EXAMPLE").is_err());
        assert_eq!(
            order_evidence(
                &order("fixture", 1, "newFutureStatus"),
                "fixture",
                "EXAMPLE"
            )
            .unwrap()
            .state,
            ObservedOrderState::Unknown
        );
        assert_eq!(
            submission_outcome(
                &json!({"status":"ok","response":{"type":"order","data":{"statuses":[]}}}),
                true
            ),
            SubmitOutcome::Unknown
        );
    }
    #[test]
    fn triggered_stop_absence_preserves_residual_and_does_not_claim_a_fill() {
        let (_dir, mut a, f) = fixture();
        f.borrow_mut().stop_listed = false;
        // An ordinary open order missing from a complete list is still a conflict.
        assert!(a.snapshot(&policy(), None).is_err());
        f.borrow_mut().stop_status = "triggered";
        let snapshot = a.snapshot(&policy(), None).unwrap();
        assert!(snapshot.stop_fired);
        assert!(snapshot.sl.is_none());
        assert_eq!(snapshot.signed_position, d("-1.1"));
        assert_eq!(a.submit(&policy(), &close()), SubmitOutcome::Accepted);
        assert_eq!(f.borrow().signed.len(), 1);
    }
    #[test]
    fn documented_terminal_rejections_allow_reconciliation_but_unknown_stays_unknown() {
        for status in [
            "marketOrderNoLiquidityRejected",
            "oracleRejected",
            "perpMaxPositionRejected",
            "liquidatedCanceled",
            "badTriggerPxRejected",
            "badAloPxRejected",
            "positionIncreaseAtOpenInterestCapRejected",
            "positionFlipAtOpenInterestCapRejected",
            "tooAggressiveAtOpenInterestCapRejected",
            "openInterestIncreaseRejected",
            "insufficientSpotBalanceRejected",
        ] {
            assert_eq!(
                order_evidence(&order("fixture", 1, status), "fixture", "EXAMPLE")
                    .unwrap()
                    .state,
                ObservedOrderState::Terminal,
                "{status}"
            );
        }
    }
    #[test]
    fn agent_approval_must_cover_actual_reservation_expiry() {
        let (_dir, mut a, f) = fixture();
        f.borrow_mut().valid_until = 3100;
        assert!(a.snapshot(&policy(), None).unwrap().agent_authorized);
        // Approval covered the original snapshot TTL but not this later action.
        f.borrow_mut().now = 2200;
        let mut action = close();
        action.nonce = 2200;
        action.expires_at_ms = 3200;
        assert_eq!(a.submit(&policy(), &action), SubmitOutcome::Rejected);
        assert_eq!(f.borrow().signed.len(), 0);
        assert!(!a.nonce_path.exists());
    }
    #[test]
    fn adapter_enforces_slippage_independently_of_absolute_price_bounds() {
        let (_dir, mut a, f) = fixture();
        for price in ["99", "95.99"] {
            a.snapshot(&policy(), None).unwrap();
            let mut action = close();
            if let RecoveryAction::CloseRemaining { limit_price, .. } = &mut action.action {
                *limit_price = d(price);
            }
            assert_eq!(a.submit(&policy(), &action), SubmitOutcome::Rejected);
        }
        assert_eq!(f.borrow().signed.len(), 0);
    }
    #[test]
    fn venue_book_timestamp_is_checked_again_at_submission() {
        let (_dir, mut a, f) = fixture();
        f.borrow_mut().book_time = 1001;
        a.snapshot(&policy(), None).unwrap();
        f.borrow_mut().now = 2002;
        assert_eq!(a.submit(&policy(), &close()), SubmitOutcome::Rejected);
        assert_eq!(f.borrow().signed.len(), 0);
    }
    #[test]
    fn mainnet_policy_and_competing_account_writer_are_refused() {
        let (dir, a, f) = fixture();
        let signer = LocalTestnetSigner::new(
            hypercarry_signing::SigningKey::from_slice(&[7; 32]).unwrap(),
            "fixture/recovery",
        )
        .unwrap();
        let mut p = policy();
        p.spec.network = ExecutionNetwork::Mainnet;
        assert!(
            TestnetRecoveryAdapter::new(
                p,
                a.config.clone(),
                0,
                Transport(f.clone()),
                signer,
                FixedClock,
                dir.path()
            )
            .is_err()
        );
        assert!(
            lock_private(
                &dir.path()
                    .join(format!("testnet-{}.lock", policy().spec.account))
            )
            .is_err()
        );
    }
}
