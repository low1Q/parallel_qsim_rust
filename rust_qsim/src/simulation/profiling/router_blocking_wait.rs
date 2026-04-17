use crate::simulation::profiling::{create_file, WriterGuard};
use csv::Writer;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex};

const HEADER: [&str; 7] = [
    "rank",
    "request_id",
    "person_id",
    "mode",
    "sim_now",
    "departure_time",
    "route_blocking_wait_ns",
];

#[derive(Clone)]
pub struct RoutingBlockingWaitCsvWriter {
    writer: Arc<Mutex<Writer<File>>>,
}

#[derive(Debug, Clone)]
pub struct RoutingBlockingWaitCsvRow {
    pub rank: String,
    pub request_id: String,
    pub person_id: String,
    pub mode: String,
    pub now: u32,
    pub departure_time: u32,
    pub route_blocking_wait_ns: u128,
}

impl RoutingBlockingWaitCsvWriter {
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

    pub fn write_row(&self, row: &RoutingBlockingWaitCsvRow) {
        let writer = &mut *self.writer.lock().unwrap();

        writer
            .write_record([
                row.rank.as_str(),
                row.request_id.as_str(),
                row.person_id.as_str(),
                row.mode.as_str(),
                &row.now.to_string(),
                &row.departure_time.to_string(),
                &row.route_blocking_wait_ns.to_string(),
            ])
            .unwrap();

        writer.flush().unwrap();
    }
}