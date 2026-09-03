//! Proves the frozen storage contract survives a real Parquet write/read cycle.

use arrow_array::{
    ArrayRef, Decimal128Array, Int32Array, RecordBatch, StringArray, TimestampMillisecondArray,
};
use arrow_schema::Schema;
use bytes::Bytes;
use hypercarry_core::{
    info::Network,
    types::{FundingHistoryEntry, TimestampMs},
};
use hypercarry_storage::settled_funding::{
    DECIMAL_SCALE, IngestionProvenance, RequestWindow, SettledFundingRecord, SourceEndpointClass,
    parquet_schema,
};
use parquet::{
    arrow::{ArrowWriter, arrow_reader::ParquetRecordBatchReaderBuilder, parquet_to_arrow_schema},
    schema::types::SchemaDescriptor,
};
use rust_decimal::Decimal;
use std::{str::FromStr, sync::Arc};

fn decimal(value: &str) -> Decimal {
    Decimal::from_str(value).expect("valid test decimal")
}

fn record(
    time: i64,
    funding_rate: &str,
    premium: &str,
    endpoint_class: SourceEndpointClass,
) -> SettledFundingRecord {
    let request_window = RequestWindow::new(
        TimestampMs::new(1_683_849_600_000),
        TimestampMs::new(1_683_856_800_000),
    )
    .expect("valid request window");
    SettledFundingRecord::from_history_entry(
        Network::Testnet,
        "hyperliquid",
        FundingHistoryEntry {
            coin: "BTC".to_owned(),
            funding_rate: decimal(funding_rate),
            premium: decimal(premium),
            time: TimestampMs::new(time),
        },
        IngestionProvenance {
            source_endpoint_class: endpoint_class,
            ingestion_time: TimestampMs::new(1_683_856_900_000),
            request_window,
            software_version: "0.0.0-interop-test".to_owned(),
        },
    )
    .expect("valid storage record")
}

fn scaled_decimal(value: Decimal) -> i128 {
    let mut value = value;
    value.rescale(DECIMAL_SCALE);
    value.mantissa()
}

fn records_to_batch(records: &[SettledFundingRecord]) -> RecordBatch {
    let descriptor = SchemaDescriptor::new(parquet_schema().expect("frozen schema parses"));
    let schema = Arc::new(
        parquet_to_arrow_schema(&descriptor, None).expect("Parquet schema converts to Arrow"),
    );
    assert_eq!(schema.fields().len(), 12);

    let funding_rate = Decimal128Array::from_iter_values(
        records
            .iter()
            .map(|record| scaled_decimal(record.funding_rate)),
    )
    .with_precision_and_scale(38, i8::try_from(DECIMAL_SCALE).expect("scale fits i8"))
    .expect("funding rate uses schema precision and scale");
    let premium = Decimal128Array::from_iter_values(
        records.iter().map(|record| scaled_decimal(record.premium)),
    )
    .with_precision_and_scale(38, i8::try_from(DECIMAL_SCALE).expect("scale fits i8"))
    .expect("premium uses schema precision and scale");

    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int32Array::from_iter_values(records.iter().map(|record| {
            i32::try_from(record.schema_version).expect("schema version fits i32")
        }))),
        Arc::new(StringArray::from_iter_values(
            records
                .iter()
                .map(|record| record.provenance.software_version.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            records
                .iter()
                .map(|record| record.identity.network.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            records.iter().map(|record| record.identity.venue.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            records.iter().map(|record| record.identity.coin.as_str()),
        )),
        Arc::new(
            TimestampMillisecondArray::from_iter_values(
                records
                    .iter()
                    .map(|record| record.identity.settlement_time.as_i64()),
            )
            .with_timezone("UTC"),
        ),
        Arc::new(funding_rate),
        Arc::new(premium),
        Arc::new(StringArray::from_iter_values(
            records
                .iter()
                .map(|record| record.provenance.source_endpoint_class.as_str()),
        )),
        Arc::new(
            TimestampMillisecondArray::from_iter_values(
                records
                    .iter()
                    .map(|record| record.provenance.ingestion_time.as_i64()),
            )
            .with_timezone("UTC"),
        ),
        Arc::new(
            TimestampMillisecondArray::from_iter_values(
                records
                    .iter()
                    .map(|record| record.provenance.request_window.start().as_i64()),
            )
            .with_timezone("UTC"),
        ),
        Arc::new(
            TimestampMillisecondArray::from_iter_values(
                records
                    .iter()
                    .map(|record| record.provenance.request_window.end().as_i64()),
            )
            .with_timezone("UTC"),
        ),
    ];

    RecordBatch::try_new(Arc::<Schema>::clone(&schema), columns)
        .expect("records conform to frozen Arrow representation")
}

#[test]
fn settled_funding_v1_round_trips_through_a_real_parquet_file() {
    let records = [
        record(
            1_683_849_600_048,
            "-0.0006133368",
            "-0.0009133368",
            SourceEndpointClass::Official,
        ),
        record(
            1_683_853_200_048,
            "0.0000125",
            "0.00000625",
            SourceEndpointClass::Development,
        ),
    ];
    let expected = records_to_batch(&records);

    let mut parquet_bytes = Vec::new();
    {
        let mut writer = ArrowWriter::try_new(&mut parquet_bytes, expected.schema(), None)
            .expect("Parquet writer accepts frozen schema");
        writer.write(&expected).expect("record batch writes");
        let metadata = writer.close().expect("Parquet footer writes");
        assert_eq!(metadata.file_metadata().num_rows(), 2);
    }

    assert_eq!(&parquet_bytes[..4], b"PAR1");
    assert_eq!(&parquet_bytes[parquet_bytes.len() - 4..], b"PAR1");

    let mut reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(parquet_bytes))
        .expect("written bytes contain valid Parquet metadata")
        .build()
        .expect("Parquet reader builds");
    let actual = reader
        .next()
        .expect("one record batch is present")
        .expect("record batch decodes");

    assert_eq!(actual, expected);
    assert!(reader.next().is_none());
}
