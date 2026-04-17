use crate::simulation::profiling::{create_file, WriterGuard};
use csv::Writer;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex};

const HEADER: [&str; 17] = [
    "rank",
    "id",
    "person_id",
    "mode",
    "now",
    "departure_time",
    "route_call_start_realtime",
    "agent_sent_request_adapter",
    "adapter_received_request_agent",
    "adapter_sent_request_grpc",
    "java_routing_service_sent_response_grpc",
    "adapter_received_response_grpc",
    "adapter_sent_response_agent",
    "agent_received_response_adapter",
    "agent_replaced_route",
    "route_end_to_end_sim_time",
    "route_blocking_wait_ns",
];

#[derive(Clone)]
pub struct RoutingRequestCsvWriter {
    writer: Arc<Mutex<Writer<File>>>,
}

#[derive(Debug, Clone)]
pub struct RoutingRequestCsvRow {
    pub rank: String,
    pub request_id: String,
    pub person_id: String,
    pub mode: String,
    pub now: u32,
    pub departure_time: u32,
    pub route_call_start: i64,
    pub agent_sent_request_adapter: i64,
    pub adapter_received_request_agent: i64,
    pub adapter_sent_request_grpc: i64,
    pub java_routing_service_sent_response_grpc: i64,
    pub adapter_received_response_grpc: i64,
    pub adapter_sent_response_agent: i64,
    pub agent_received_response_adapter: i64,
    pub agent_replaced_route: i64,
    pub route_end_to_end_sim_time: u32,
    pub route_blocking_wait_ns: u128,
}

impl RoutingRequestCsvWriter {
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

    pub fn write_row(&self, row: &RoutingRequestCsvRow) {
        let writer = &mut *self.writer.lock().unwrap();
        writer
            .write_record([
                row.rank.as_str(),
                row.request_id.as_str(),
                row.person_id.as_str(),
                row.mode.as_str(),
                &row.now.to_string(),
                &row.departure_time.to_string(),
                &row.route_call_start.to_string(),
                &row.agent_sent_request_adapter.to_string(),
                &row.adapter_received_request_agent.to_string(),
                &row.adapter_sent_request_grpc.to_string(),
                &row.java_routing_service_sent_response_grpc.to_string(),
                &row.adapter_received_response_grpc.to_string(),
                &row.adapter_sent_response_agent.to_string(),
                &row.agent_received_response_adapter.to_string(),
                &row.agent_replaced_route.to_string(),
                &row.route_end_to_end_sim_time.to_string(),
                &row.route_blocking_wait_ns.to_string(),
            ])
            .unwrap();
    }
}
