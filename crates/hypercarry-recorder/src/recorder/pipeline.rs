use super::RecorderError;
use crate::{
    normalized::{NormalizedEvent, NormalizedParquetWriter},
    raw::RawCaptureRecord,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::Duration,
};
use tokio::sync::mpsc;

pub(super) async fn normalize_stream(
    mut receiver: mpsc::Receiver<RawCaptureRecord>,
    mut writer: NormalizedParquetWriter,
    stale_after: Duration,
    duplicate_window: usize,
) -> Result<NormalizationReport, RecorderError> {
    let mut report = NormalizationReport::default();
    let mut recent_ids = HashSet::with_capacity(duplicate_window);
    let mut id_order = VecDeque::with_capacity(duplicate_window);
    let mut latest_source_time = HashMap::<(String, String), i64>::new();
    let stale_after_ms = i64::try_from(stale_after.as_millis()).unwrap_or(i64::MAX);

    while let Some(record) = receiver.recv().await {
        let event = match NormalizedEvent::from_raw(&record) {
            Ok(Some(event)) => event,
            Ok(None) => continue,
            Err(error) => {
                report.parse_errors = report.parse_errors.saturating_add(1);
                tracing::warn!(
                    network = %record.network,
                    connection_id = record.connection_id,
                    sequence = record.sequence,
                    error = %error,
                    "raw frame could not be normalized"
                );
                continue;
            }
        };
        if recent_ids.contains(&event.event_id) {
            report.duplicate_frames = report.duplicate_frames.saturating_add(1);
            continue;
        }
        recent_ids.insert(event.event_id.clone());
        id_order.push_back(event.event_id.clone());
        if id_order.len() > duplicate_window
            && let Some(expired) = id_order.pop_front()
        {
            recent_ids.remove(&expired);
        }

        if let Some(source_time) = event.source_time_ms {
            let key = (event.channel.clone(), event.coin.clone());
            if latest_source_time
                .get(&key)
                .is_some_and(|latest| source_time < *latest)
            {
                report.out_of_order_frames = report.out_of_order_frames.saturating_add(1);
            }
            latest_source_time
                .entry(key)
                .and_modify(|latest| *latest = (*latest).max(source_time))
                .or_insert(source_time);
            if event.received_at_ms.saturating_sub(source_time) > stale_after_ms {
                report.stale_frames = report.stale_frames.saturating_add(1);
            }
        }
        writer.push(event)?;
        report.normalized_frames = report.normalized_frames.saturating_add(1);
    }
    writer.finish()?;
    Ok(report)
}

#[derive(Debug, Default)]
pub(super) struct NormalizationReport {
    pub(super) normalized_frames: u64,
    pub(super) duplicate_frames: u64,
    pub(super) out_of_order_frames: u64,
    pub(super) stale_frames: u64,
    pub(super) parse_errors: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::BackpressurePolicy;

    #[test]
    fn bounded_queue_reports_drop_newest_when_full() {
        let (sender, _receiver) = mpsc::channel(1);
        sender.try_send(1_u8).expect("first item fits");
        assert!(matches!(
            sender.try_send(2_u8),
            Err(mpsc::error::TrySendError::Full(2))
        ));
        assert_eq!(
            BackpressurePolicy::DropNewest.as_str(),
            "drop_newest_normalized"
        );
    }
}
