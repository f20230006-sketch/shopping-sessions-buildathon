use crate::{CampaignDays, Name, UserRow};
use arrow_array::{ArrayRef, Date32Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

fn text(value: Name<'_>) -> &str {
    std::str::from_utf8(value).unwrap()
}

fn write(path: &Path, fields: &[(&str, DataType, bool)], arrays: Vec<ArrayRef>) {
    let schema = Arc::new(Schema::new(
        fields
            .iter()
            .map(|(name, kind, null)| Field::new(*name, kind.clone(), *null))
            .collect::<Vec<_>>(),
    ));
    let batch = RecordBatch::try_new(schema.clone(), arrays).unwrap();
    let properties = WriterProperties::builder()
        .set_compression(Compression::UNCOMPRESSED)
        .set_statistics_enabled(parquet::file::properties::EnabledStatistics::None)
        .build();
    let mut writer =
        ArrowWriter::try_new(File::create(path).unwrap(), schema, Some(properties)).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

pub fn users(path: &Path, rows: &[UserRow]) {
    use DataType::{Date32, Int64, Utf8};
    write(
        path,
        &[
            ("user_id", Utf8, false),
            ("day", Date32, false),
            ("sessions", Int64, false),
            ("events", Int64, false),
            ("revenue_cents", Int64, false),
            ("top_category", Utf8, true),
        ],
        vec![
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| text(row.user)),
            )),
            Arc::new(Date32Array::from_iter_values(
                rows.iter().map(|row| row.day),
            )),
            Arc::new(Int64Array::from_iter_values(
                rows.iter().map(|row| row.sessions),
            )),
            Arc::new(Int64Array::from_iter_values(
                rows.iter().map(|row| row.events),
            )),
            Arc::new(Int64Array::from_iter_values(
                rows.iter().map(|row| row.revenue),
            )),
            Arc::new(
                rows.iter()
                    .map(|row| row.category.map(text))
                    .collect::<StringArray>(),
            ),
        ],
    );
}

pub fn campaigns(path: &Path, totals: CampaignDays, names: &[(Name, Name)]) {
    use DataType::{Date32, Int64, Utf8};
    let rows: Vec<_> = totals.into_iter().collect();
    write(
        path,
        &[
            ("campaign_id", Utf8, false),
            ("channel", Utf8, false),
            ("day", Date32, false),
            ("sessions", Int64, false),
            ("purchasing_sessions", Int64, false),
            ("revenue_cents", Int64, false),
            ("assisted_sessions", Int64, false),
        ],
        vec![
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|((id, _), _)| text(names[*id as usize].0)),
            )),
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|((id, _), _)| text(names[*id as usize].1)),
            )),
            Arc::new(Date32Array::from_iter_values(
                rows.iter().map(|((_, day), _)| *day),
            )),
            Arc::new(Int64Array::from_iter_values(
                rows.iter().map(|(_, row)| row.sessions),
            )),
            Arc::new(Int64Array::from_iter_values(
                rows.iter().map(|(_, row)| row.purchasing),
            )),
            Arc::new(Int64Array::from_iter_values(
                rows.iter().map(|(_, row)| row.revenue),
            )),
            Arc::new(Int64Array::from_iter_values(
                rows.iter().map(|(_, row)| row.assisted),
            )),
        ],
    );
}
