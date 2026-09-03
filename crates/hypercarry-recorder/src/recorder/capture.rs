use super::{Recorder, RecorderError, session::timestamp_ms};
use crate::{
    raw::{RAW_SCHEMA_VERSION, RawCaptureRecord, RawCaptureWriter, RawEvent},
    transport::{MarketConnection, MarketTransport, TransportError, TransportMessage},
};
use hypercarry_core::info::Network;
use serde_json::json;
use std::time::{Duration, Instant};
use tokio::{sync::mpsc, time};
use tokio_util::sync::CancellationToken;

impl<T: MarketTransport> Recorder<T> {
    #[allow(clippy::too_many_lines)]
    pub(super) async fn capture(
        &self,
        session_id: &str,
        raw_writer: &mut RawCaptureWriter,
        sender: mpsc::Sender<RawCaptureRecord>,
        cancellation: &CancellationToken,
    ) -> Result<CaptureReport, RecorderError> {
        let mut report = CaptureReport::default();
        let mut sequence = 0_u64;
        let mut connection_id = 0_u64;
        let mut consecutive_failures = 0_u32;
        let mut backoff = self.config.initial_backoff;

        loop {
            if cancellation.is_cancelled() {
                return Ok(report);
            }
            connection_id = connection_id.saturating_add(1);
            let connection = tokio::select! {
                () = cancellation.cancelled() => return Ok(report),
                result = time::timeout(self.config.connect_timeout, self.transport.connect()) => {
                    match result {
                        Ok(Ok(connection)) => connection,
                        Ok(Err(error)) => {
                            consecutive_failures = consecutive_failures.saturating_add(1);
                            tracing::warn!(
                                network = %self.config.network,
                                attempt = consecutive_failures,
                                error = %error,
                                "market WebSocket connection failed"
                            );
                            if consecutive_failures >= self.config.max_reconnect_attempts {
                                return Err(RecorderError::ReconnectExhausted {
                                    attempts: consecutive_failures,
                                    last_error: error.to_string(),
                                });
                            }
                            sleep_or_cancel(backoff, cancellation).await?;
                            backoff = doubled_capped(backoff, self.config.max_backoff);
                            report.reconnects = report.reconnects.saturating_add(1);
                            continue;
                        }
                        Err(_) => {
                            consecutive_failures = consecutive_failures.saturating_add(1);
                            if consecutive_failures >= self.config.max_reconnect_attempts {
                                return Err(RecorderError::ReconnectExhausted {
                                    attempts: consecutive_failures,
                                    last_error: format!(
                                        "connect timed out after {}ms",
                                        self.config.connect_timeout.as_millis()
                                    ),
                                });
                            }
                            sleep_or_cancel(backoff, cancellation).await?;
                            backoff = doubled_capped(backoff, self.config.max_backoff);
                            report.reconnects = report.reconnects.saturating_add(1);
                            continue;
                        }
                    }
                }
            };
            let mut connection = connection;
            report.connections = report.connections.saturating_add(1);
            tracing::info!(
                network = %self.config.network,
                connection_id,
                coins = self.config.coins.len(),
                "market WebSocket connected"
            );

            if let Err(error) = subscribe(&mut connection, &self.config.coins).await {
                consecutive_failures = consecutive_failures.saturating_add(1);
                record_boundary(
                    raw_writer,
                    session_id,
                    self.config.network,
                    connection_id,
                    &mut sequence,
                    RawEvent::Disconnect {
                        reason: format!("subscription failed: {error}"),
                    },
                )
                .await?;
            } else {
                let outcome = self
                    .read_connection(
                        &mut connection,
                        session_id,
                        raw_writer,
                        &sender,
                        cancellation,
                        connection_id,
                        &mut sequence,
                        &mut report,
                    )
                    .await?;
                if outcome.received_frame {
                    consecutive_failures = 0;
                    backoff = self.config.initial_backoff;
                } else {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                }
                if outcome.cancelled {
                    return Ok(report);
                }
            }

            if consecutive_failures >= self.config.max_reconnect_attempts {
                return Err(RecorderError::ReconnectExhausted {
                    attempts: consecutive_failures,
                    last_error: "connection repeatedly ended before receiving data".to_owned(),
                });
            }
            report.reconnects = report.reconnects.saturating_add(1);
            sleep_or_cancel(backoff, cancellation).await?;
            backoff = doubled_capped(backoff, self.config.max_backoff);
        }
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn read_connection<C: MarketConnection>(
        &self,
        connection: &mut C,
        session_id: &str,
        raw_writer: &mut RawCaptureWriter,
        sender: &mpsc::Sender<RawCaptureRecord>,
        cancellation: &CancellationToken,
        connection_id: u64,
        sequence: &mut u64,
        report: &mut CaptureReport,
    ) -> Result<ConnectionOutcome, RecorderError> {
        let mut heartbeat = time::interval(self.config.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        let mut last_server_message = Instant::now();
        let mut received_frame = false;

        loop {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    if let Err(error) = connection.close().await {
                        tracing::warn!(network = %self.config.network, error = %error, "WebSocket close handshake failed");
                    }
                    return Ok(ConnectionOutcome { received_frame, cancelled: true });
                }
                _ = heartbeat.tick() => {
                    let silence = last_server_message.elapsed();
                    if silence >= self.config.stale_after {
                        report.stale_connections = report.stale_connections.saturating_add(1);
                        record_boundary(
                            raw_writer,
                            session_id,
                            self.config.network,
                            connection_id,
                            sequence,
                            RawEvent::Stale {
                                silence_ms: u64::try_from(silence.as_millis()).unwrap_or(u64::MAX),
                            },
                        ).await?;
                        tracing::warn!(
                            network = %self.config.network,
                            connection_id,
                            silence_ms = silence.as_millis(),
                            "market WebSocket is stale; reconnecting"
                        );
                        return Ok(ConnectionOutcome { received_frame, cancelled: false });
                    }
                    connection.send_text(r#"{"method":"ping"}"#.to_owned()).await?;
                    report.heartbeats_sent = report.heartbeats_sent.saturating_add(1);
                }
                message = connection.next_message() => {
                    match message {
                        Ok(Some(TransportMessage::Text(payload))) => {
                            last_server_message = Instant::now();
                            *sequence = sequence.saturating_add(1);
                            let record = RawCaptureRecord {
                                schema_version: RAW_SCHEMA_VERSION,
                                session_id: session_id.to_owned(),
                                network: self.config.network,
                                connection_id,
                                sequence: *sequence,
                                received_at_ms: timestamp_ms()?,
                                event: RawEvent::Frame { payload },
                            };
                            raw_writer.write(&record).await?;
                            report.raw_frames = report.raw_frames.saturating_add(1);
                            received_frame = true;
                            match sender.try_send(record) {
                                Ok(()) => {}
                                Err(mpsc::error::TrySendError::Full(_)) => {
                                    report.dropped_normalized_frames = report.dropped_normalized_frames.saturating_add(1);
                                    tracing::warn!(
                                        network = %self.config.network,
                                        connection_id,
                                        queue_capacity = self.config.queue_capacity,
                                        dropped_total = report.dropped_normalized_frames,
                                        policy = self.config.backpressure_policy.as_str(),
                                        "normalized market-data queue is full"
                                    );
                                }
                                Err(mpsc::error::TrySendError::Closed(_)) => {
                                    return Err(RecorderError::Task("normalization task ended early".to_owned()));
                                }
                            }
                        }
                        Ok(Some(TransportMessage::Ping(payload))) => {
                            last_server_message = Instant::now();
                            connection.send_pong(payload).await?;
                        }
                        Ok(Some(TransportMessage::Pong)) => {
                            last_server_message = Instant::now();
                        }
                        Ok(Some(TransportMessage::Close(reason))) => {
                            record_boundary(
                                raw_writer,
                                session_id,
                                self.config.network,
                                connection_id,
                                sequence,
                                RawEvent::Disconnect {
                                    reason: reason.unwrap_or_else(|| "server closed connection".to_owned()),
                                },
                            ).await?;
                            return Ok(ConnectionOutcome { received_frame, cancelled: false });
                        }
                        Ok(None) => {
                            record_boundary(
                                raw_writer,
                                session_id,
                                self.config.network,
                                connection_id,
                                sequence,
                                RawEvent::Disconnect { reason: "stream ended".to_owned() },
                            ).await?;
                            return Ok(ConnectionOutcome { received_frame, cancelled: false });
                        }
                        Err(error) => {
                            record_boundary(
                                raw_writer,
                                session_id,
                                self.config.network,
                                connection_id,
                                sequence,
                                RawEvent::Disconnect { reason: error.to_string() },
                            ).await?;
                            tracing::warn!(network = %self.config.network, connection_id, error = %error, "market WebSocket disconnected");
                            return Ok(ConnectionOutcome { received_frame, cancelled: false });
                        }
                    }
                }
            }
        }
    }
}

async fn subscribe<C: MarketConnection>(
    connection: &mut C,
    coins: &[String],
) -> Result<(), TransportError> {
    for coin in coins {
        for subscription_type in ["activeAssetCtx", "l2Book"] {
            let request = json!({
                "method": "subscribe",
                "subscription": {
                    "type": subscription_type,
                    "coin": coin,
                }
            });
            connection.send_text(request.to_string()).await?;
        }
    }
    Ok(())
}

async fn record_boundary(
    writer: &mut RawCaptureWriter,
    session_id: &str,
    network: Network,
    connection_id: u64,
    sequence: &mut u64,
    event: RawEvent,
) -> Result<(), RecorderError> {
    *sequence = sequence.saturating_add(1);
    writer
        .write(&RawCaptureRecord {
            schema_version: RAW_SCHEMA_VERSION,
            session_id: session_id.to_owned(),
            network,
            connection_id,
            sequence: *sequence,
            received_at_ms: timestamp_ms()?,
            event,
        })
        .await
        .map_err(RecorderError::Raw)
}

async fn sleep_or_cancel(
    delay: Duration,
    cancellation: &CancellationToken,
) -> Result<(), RecorderError> {
    tokio::select! {
        () = cancellation.cancelled() => Ok(()),
        () = time::sleep(delay) => Ok(()),
    }
}

fn doubled_capped(value: Duration, cap: Duration) -> Duration {
    value.saturating_mul(2).min(cap)
}

#[derive(Debug, Default)]
pub(super) struct CaptureReport {
    pub(super) connections: u64,
    pub(super) reconnects: u64,
    pub(super) heartbeats_sent: u64,
    pub(super) raw_frames: u64,
    pub(super) dropped_normalized_frames: u64,
    pub(super) stale_connections: u64,
}

#[derive(Debug)]
struct ConnectionOutcome {
    received_frame: bool,
    cancelled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_backoff_is_capped() {
        assert_eq!(
            doubled_capped(Duration::from_millis(4), Duration::from_millis(5)),
            Duration::from_millis(5)
        );
    }
}
