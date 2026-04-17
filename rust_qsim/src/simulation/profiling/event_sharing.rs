use crate::simulation::profiling::{create_file, WriterGuard};
use csv::Writer;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex};

const HEADER: [&str; 18] = [
    "batch_id",
    "batch_seq",
    "completed_bin_start",
    "completed_bin_end",
    "event_count",
    "publish_snapshot",
    "empty_bin",
    "batch_started_at_realtime",
    "batch_sent_at_realtime",
    "batch_wait_ns",
    "avg_event_residence_ns",
    "max_event_residence_ns",
    "avg_event_detected_to_batch_send_ns",
    "max_event_detected_to_batch_send_ns",
    "avg_logger_send_to_batch_send_ns",
    "max_logger_send_to_batch_send_ns",
    "avg_logger_to_adapter_arrival_ns",
    "max_logger_to_adapter_arrival_ns",
];

#[derive(Clone)]
pub struct EventSharingSummaryCsvWriter {
    writer: Arc<Mutex<Writer<File>>>,
}

#[derive(Debug, Clone)]
pub struct EventSharingSummaryCsvRow {
    pub batch_id: String,
    pub batch_seq: usize,
    pub completed_bin_start: u32,
    pub completed_bin_end: u32,
    pub event_count: usize,
    pub publish_snapshot: bool,
    pub empty_bin: bool,
    pub batch_started_at_realtime: i64,
    pub batch_sent_at_realtime: i64,
    pub batch_wait_ns: u128,
    pub avg_event_residence_ns: u128,
    pub max_event_residence_ns: u128,
    pub avg_event_detected_to_batch_send_ns: u128,
    pub max_event_detected_to_batch_send_ns: u128,
    pub avg_logger_send_to_batch_send_ns: u128,
    pub max_logger_send_to_batch_send_ns: u128,
    pub avg_logger_to_adapter_arrival_ns: u128,
    pub max_logger_to_adapter_arrival_ns: u128,
}

impl EventSharingSummaryCsvWriter {
    pub fn new(path: &Path) -> (Self, WriterGuard) {
        let file = create_file(path);
        let mut writer = csv::Writer::from_writer(file);
        writer.write_record(HEADER).unwrap();

        let writer = Arc::new(Mutex::new(writer));
        let guard = WriterGuard {
            writer: writer.clone(),
        };

        (Self { writer }, guard)
    }

    pub fn write_row(&self, row: &EventSharingSummaryCsvRow) {
        let writer = &mut *self.writer.lock().unwrap();

        writer
            .write_record([
                row.batch_id.as_str(),
                &row.batch_seq.to_string(),
                &row.completed_bin_start.to_string(),
                &row.completed_bin_end.to_string(),
                &row.event_count.to_string(),
                &row.publish_snapshot.to_string(),
                &row.empty_bin.to_string(),
                &row.batch_started_at_realtime.to_string(),
                &row.batch_sent_at_realtime.to_string(),
                &row.batch_wait_ns.to_string(),
                &row.avg_event_residence_ns.to_string(),
                &row.max_event_residence_ns.to_string(),
                &row.avg_event_detected_to_batch_send_ns.to_string(),
                &row.max_event_detected_to_batch_send_ns.to_string(),
                &row.avg_logger_send_to_batch_send_ns.to_string(),
                &row.max_logger_send_to_batch_send_ns.to_string(),
                &row.avg_logger_to_adapter_arrival_ns.to_string(),
                &row.max_logger_to_adapter_arrival_ns.to_string(),
            ])
            .unwrap();
    }
}
