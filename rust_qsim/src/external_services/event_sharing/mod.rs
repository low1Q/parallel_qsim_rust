pub mod event_sharing_listener;

use crate::external_services::{RequestAdapter, RequestAdapterFactory, RequestToAdapter};
use crate::generated::event_sharing::event_sharing_service_client::EventSharingServiceClient;
use crate::generated::event_sharing::{Ack, BatchRequest, Request};
use crate::simulation::config::Config;
use crate::simulation::data_structures::RingIter;
use crate::simulation::profiling::event_sharing::{
    EventSharingSummaryCsvRow, EventSharingSummaryCsvWriter,
};
use derive_builder::Builder;
use std::collections::BTreeMap;
use std::mem;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::SystemTime;
use tokio::task::JoinHandle;
use tracing::{info, warn};
use uuid::Uuid;

const DEFAULT_BIN_SIZE_SECS: u32 = 900;
const BIN_FINALIZATION_LAG_SECS: u32 = 2;
const DEFAULT_MAX_EVENTS_PER_BIN_CHUNK: usize = 10000;

fn bin_start(t: u32, bin_size: u32) -> u32 {
    (t / bin_size) * bin_size
}

fn bin_end(bin_start: u32, bin_size: u32) -> u32 {
    bin_start + bin_size
}

// Ein Bin [start, end) gilt erst dann als sicher abgeschlossen,
// wenn ein Event mit t >= end + 1 eingetroffen ist.
//
// Begründung:
// - Simulation läuft in ganzzahligen Sekunden
// - Events kommen zeitlich sortiert
// - bei t = end kann theoretisch noch ein Event mit t < end kommen
// - erst bei t = end + 1 ist ausgeschlossen, dass noch ein Event aus dem
// gerade abgeschlossenen Bin nachkommt
fn is_bin_safely_closed(current_bin_end: u32, event_time: u32) -> bool {
    event_time >= current_bin_end + BIN_FINALIZATION_LAG_SECS
}

#[derive(Debug)]
struct BinState {
    // Noch nicht gesendete Events dieses Bins.
    pending_events: Vec<InternalEventSharingRequestPayload>,
    // Ob dieser Bin jemals ein Event gesehen hat.
    saw_any_event: bool,
    // Nur für Logging.
    sent_chunk_count: usize,
}

impl BinState {
    fn new() -> Self {
        Self {
            pending_events: Vec::new(),
            saw_any_event: false,
            sent_chunk_count: 0,
        }
    }
}

#[derive(Debug)]
struct OutgoingBinChunk {
    events: Vec<InternalEventSharingRequestPayload>,
    completed_bin_start: u32,
    completed_bin_end: u32,
    empty_bin: bool,
    publish_snapshot: bool,
    chunk_seq: usize,
    batch_started_at_realtime: Option<i64>,
    batch_id: Uuid,
}

#[derive(Debug, Default)]
struct AdapterMetrics {
    incoming_event_count: AtomicUsize,
    batch_count: AtomicUsize,
    publish_batch_count: AtomicUsize,

    adapter_event_residence_ns_total: AtomicU64,
    adapter_event_residence_ns_max: AtomicU64,

    batch_wait_ns_total: AtomicU64,
    batch_wait_ns_max: AtomicU64,
}
impl AdapterMetrics {
    fn update_max(target: &AtomicU64, value: u64) {
        let mut prev = target.load(Ordering::Relaxed);
        while value > prev {
            match target.compare_exchange(prev, value, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(actual) => prev = actual,
            }
        }
    }
}

pub struct EventSharingServiceAdapter {
    clients: RingIter<EventSharingServiceClient<tonic::transport::Channel>>,
    shutdown_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,

    bin_size_secs: u32,
    max_events_per_bin_chunk: usize,
    // Start des ältesten noch NICHT finalisierten Bins.
    oldest_unfinalized_bin_start: Option<u32>,
    // Zustand pro Bin.
    bins: BTreeMap<u32, BinState>,
    // Serieller Versandpfad nach Java.
    outgoing_tx: Sender<OutgoingBinChunk>,

    metrics: Arc<AdapterMetrics>,

    received_events_count: u64,
}

#[derive(Debug)]
pub struct InternalEventSharingRequest {
    pub payload: InternalEventSharingRequestPayload,
}

impl RequestToAdapter for InternalEventSharingRequest {}

#[derive(Debug, PartialEq, Builder)]
pub struct InternalEventSharingRequestPayload {
    pub event_type: String,
    pub link_id: String,
    pub vehicle_id: String,
    pub now: u32,
    pub driver_id: Option<String>,
    pub network_mode: Option<String>,
    pub relative_position_on_link: Option<f64>,
    pub event_detected_at_realtime: Option<i64>,
    pub adapter_arrived_at_realtime: Option<i64>,
    pub logger_send_started_realtime: Option<i64>,
    pub seq_in_partition: u64,
    pub partition_id: u32,
}

impl InternalEventSharingRequestPayload {
    pub fn equals_ignoring_uuid(&self, other: &Self) -> bool {
        self.event_type == other.event_type
            && self.link_id == other.link_id
            && self.vehicle_id == other.vehicle_id
            && self.now == other.now
            && self.driver_id == other.driver_id
            && self.network_mode == other.network_mode
            && self.relative_position_on_link == other.relative_position_on_link
    }
}

#[derive(Debug, Clone, Default)]
pub struct InternalEventSharingResponse {
    pub(crate) message_received: bool,
}

impl From<InternalEventSharingRequestPayload> for Request {
    fn from(req: InternalEventSharingRequestPayload) -> Self {
        Request {
            event_type: req.event_type,
            link_id: req.link_id,
            vehicle_id: req.vehicle_id,
            now: req.now,
            event_detected_at_realtime: req.event_detected_at_realtime,
            adapter_arrived_at_realtime: req.adapter_arrived_at_realtime,
            sequence_number: Some(req.seq_in_partition),
            participant_id: Some(req.partition_id),
        }
    }
}

impl From<Ack> for InternalEventSharingResponse {
    fn from(value: Ack) -> Self {
        Self {
            message_received: value.message_received,
        }
    }
}

// Factory for creating event sharing service adapters.
// Connects to the event sharing service at the given IP address.
pub struct EventSharingServiceAdapterFactory {
    ip: Vec<String>,
    config: Arc<Config>,
    shutdown_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    bin_size_secs: u32,
    max_events_per_bin_chunk: usize,
}

impl EventSharingServiceAdapterFactory {
    pub fn new(
        ip: Vec<impl Into<String>>,
        config: Arc<Config>,
        shutdown_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    ) -> Self {
        Self {
            ip: ip.into_iter().map(|s| s.into()).collect(),
            config,
            shutdown_handles,
            bin_size_secs: DEFAULT_BIN_SIZE_SECS,
            max_events_per_bin_chunk: DEFAULT_MAX_EVENTS_PER_BIN_CHUNK,
        }
    }

    // Kompatibilitäts-No-Op
    pub fn with_batch_params(self, _max_batch_size: usize, _batch_interval_millisecs: u64) -> Self {
        self
    }

    pub fn with_bin_size_secs(mut self, bin_size_secs: u32) -> Self {
        self.bin_size_secs = bin_size_secs;
        self
    }

    pub fn with_closed_bin_batch_size(mut self, batch_size: usize) -> Self {
        assert!(batch_size > 0, "bin chunk size must be > 0");
        self.max_events_per_bin_chunk = batch_size;
        self
    }
}

impl RequestAdapterFactory<InternalEventSharingRequest> for EventSharingServiceAdapterFactory {
    async fn build(self) -> impl RequestAdapter<InternalEventSharingRequest> {
        let mut res = Vec::new();
        for ip in self.ip {
            info!("Connecting to event sharing service at {}", ip);
            let start = std::time::Instant::now();
            let client;
            loop {
                match EventSharingServiceClient::connect(ip.clone()).await {
                    Ok(c) => {
                        client = c;
                        break;
                    }
                    Err(e) => {
                        if start.elapsed().as_secs()
                            >= self.config.computational_setup().retry_time_seconds
                        {
                            panic!(
                                "Failed to connect to event sharing service at {} after configured retry maximum: {}",
                                ip, e
                            );
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
            }
            res.push(client);
        }

        EventSharingServiceAdapter::new(
            res,
            self.shutdown_handles,
            self.bin_size_secs,
            self.max_events_per_bin_chunk,
        )
    }
}

impl RequestAdapter<InternalEventSharingRequest> for EventSharingServiceAdapter {
    fn on_request(&mut self, internal_req: InternalEventSharingRequest) {
        self.received_events_count += 1;
        let full_measurement = crate::simulation::profiling::flags::performance_logging_enabled();

        let mut event = internal_req.payload;
        let event_time = event.now;
        let event_bin_start = bin_start(event_time, self.bin_size_secs);

        if full_measurement {
            event.adapter_arrived_at_realtime = Some(unix_nanos_now());
            self.metrics
                .incoming_event_count
                .fetch_add(1, Ordering::Relaxed);
        }

        if self.oldest_unfinalized_bin_start.is_none() {
            let start_time = 0;
            self.oldest_unfinalized_bin_start = Some(start_time);
            // info!(
            //     "EventSharingServiceAdapter: opening first bin [{}, {})",
            //     event_bin_start,
            //     bin_end(event_bin_start, self.bin_size_secs)
            // );
        }

        let state = self
            .bins
            .entry(event_bin_start)
            .or_insert_with(BinState::new);
        state.saw_any_event = true;
        state.pending_events.push(event);

        // Neue Idee:
        // Wenn ein Bin-Chunk voll ist, sofort senden (publish_snapshot=false).
        self.flush_full_chunks_for_bin(event_bin_start);

        // Danach ältere Bins finalisieren, falls durch dieses Event sicher abgeschlossen.
        self.finalize_bins_up_to(event_time);
    }

    fn on_shutdown(&mut self) {
        info!("EventSharingServiceAdapter: Starting shutdown sequence...");
        println!(
            "EventSharingServiceAdapter received total events: {}",
            self.received_events_count
        );
        if let Some(oldest) = self.oldest_unfinalized_bin_start {
            let latest_bin_with_state = self.bins.keys().next_back().copied();

            match latest_bin_with_state {
                Some(latest) => {
                    let latest_event_count = self
                        .bins
                        .get(&latest)
                        .map(|state| state.pending_events.len())
                        .unwrap_or(0);
                    let oldest_event_count = self
                        .bins
                        .get(&oldest)
                        .map(|state| state.pending_events.len())
                        .unwrap_or(0);
                    warn!(
                     "EventSharingServiceAdapter: shutdown with unfinalized bins starting at [{}, {}). Latest buffered bin start is {}. Latest: {}, Oldest: {}",
                     oldest,
                     bin_end(oldest, self.bin_size_secs),
                     latest,
                        latest_event_count,
                        oldest_event_count,
                     );
                }
                None => {
                    info!(
                     "EventSharingServiceAdapter: shutdown with oldest_unfinalized_bin_start={}, but no buffered bin state remains.",
                     oldest
                     );
                }
            }
        } else {
            info!("EventSharingServiceAdapter: shutdown with no open bins.");
        }

        for client in &mut self.clients {
            let mut c = client.clone();
            let handle = tokio::spawn(async move {
                c.shutdown(())
                    .await
                    .expect("Error while shutting down routing service");
            });
            self.shutdown_handles.lock().unwrap().push(handle);
        }

        // Channel schließen, damit der Sender-Thread sauber auslaufen kann.
        // Das passiert implizit, wenn self gedroppt wird; hier kein explizites close nötig.
        info!("EventSharingServiceAdapter: All clients dropped and resources cleared.");
    }
}

impl EventSharingServiceAdapter {
    fn new(
        clients: Vec<EventSharingServiceClient<tonic::transport::Channel>>,
        shutdown_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
        bin_size_secs: u32,
        max_events_per_bin_chunk: usize,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<OutgoingBinChunk>();

        let enable_event_measurement =
            crate::simulation::profiling::flags::performance_logging_enabled();
        let summary_writer_and_guard = if enable_event_measurement {
            let summary_path = std::env::var("EVENT_SHARING_SUMMARY_RUST_CSV")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("event_sharing_summary_rust.csv"));
            let (writer, guard) = EventSharingSummaryCsvWriter::new(&summary_path);
            Some((writer, guard))
        } else {
            None
        };
        let metrics = Arc::new(AdapterMetrics::default());
        let sender_metrics = metrics.clone();

        let mut sender_client = clients
            .first()
            .expect("At least one event sharing client must exist")
            .clone();

        thread::Builder::new()
            .name("event-sharing-bin-sender".into())
            .spawn(move || {
                let (_summary_guard, summary_writer) = match summary_writer_and_guard {
                    Some((writer, guard)) => (Some(guard), Some(writer)),
                    None => (None, None),
                };
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to build tokio runtime for event-sharing sender");

                while let Ok(msg) = rx.recv() {
                    let event_count = msg.events.len();
                    let batch_sent_at_realtime = unix_nanos_now();

                    // Werte aus msg früh sichern, bevor msg.events verbraucht wird
                    let chunk_seq = msg.chunk_seq;
                    let completed_bin_start = msg.completed_bin_start;
                    let completed_bin_end = msg.completed_bin_end;
                    let publish_snapshot = msg.publish_snapshot;
                    let empty_bin = msg.empty_bin;
                    let batch_started_at_realtime = msg.batch_started_at_realtime.unwrap_or_default();

                    let batch_id = msg.batch_id;
                    let batch_id_str = batch_id.to_string();

                    // 1) Residence time pro Event: Adapter-Ankunft -> Batch-Send
                    let mut residence_total_ns: u128 = 0;
                    let mut residence_count: usize = 0;
                    let mut max_event_residence_ns: u128 = 0;

                    // 2) Event detected -> batch sent
                    let mut detected_total_ns: u128 = 0;
                    let mut detected_count: usize = 0;
                    let mut max_event_detected_to_batch_send_ns: u128 = 0;

                    // 3) Logger send started -> batch sent
                    let mut logger_send_total_ns: u128 = 0;
                    let mut logger_send_count: usize = 0;
                    let mut max_logger_send_to_batch_send_ns: u128 = 0;

                    let mut logger_to_adapter_total_ns: u128 = 0;
                    let mut logger_to_adapter_count: usize = 0;
                    let mut max_logger_to_adapter_arrival_ns: u128 = 0;

                    for ev in &msg.events {
                        if let Some(arrived) = ev.adapter_arrived_at_realtime {
                            let residence = (batch_sent_at_realtime - arrived).max(0) as u128;

                            residence_total_ns += residence;
                            residence_count += 1;
                            if residence > max_event_residence_ns {
                                max_event_residence_ns = residence;
                            }

                            sender_metrics
                                .adapter_event_residence_ns_total
                                .fetch_add(residence as u64, Ordering::Relaxed);
                            AdapterMetrics::update_max(
                                &sender_metrics.adapter_event_residence_ns_max,
                                residence as u64,
                            );
                        }

                        if let Some(detected) = ev.event_detected_at_realtime {
                            let detected_to_send = (batch_sent_at_realtime - detected).max(0) as u128;
                            detected_total_ns += detected_to_send;
                            detected_count += 1;
                            if detected_to_send > max_event_detected_to_batch_send_ns {
                                max_event_detected_to_batch_send_ns = detected_to_send;
                            }
                        }

                        if let Some(logger_send_started) = ev.logger_send_started_realtime {
                            let logger_send_to_send = (batch_sent_at_realtime - logger_send_started).max(0) as u128;
                            logger_send_total_ns += logger_send_to_send;
                            logger_send_count += 1;
                            if logger_send_to_send > max_logger_send_to_batch_send_ns {
                                max_logger_send_to_batch_send_ns = logger_send_to_send;
                            }
                        }

                        if let (Some(logger_send_started), Some(arrived)) =
                            (ev.logger_send_started_realtime, ev.adapter_arrived_at_realtime)
                        {
                            let logger_to_adapter = (arrived - logger_send_started).max(0) as u128;
                            logger_to_adapter_total_ns += logger_to_adapter;
                            logger_to_adapter_count += 1;
                            if logger_to_adapter > max_logger_to_adapter_arrival_ns {
                                max_logger_to_adapter_arrival_ns = logger_to_adapter;
                            }
                        }
                    }

                    let avg_event_residence_ns = if residence_count > 0 {
                        residence_total_ns / residence_count as u128
                    } else {
                        0
                    };

                    let avg_event_detected_to_batch_send_ns = if detected_count > 0 {
                        detected_total_ns / detected_count as u128
                    } else {
                        0
                    };

                    let avg_logger_send_to_batch_send_ns = if logger_send_count > 0 {
                        logger_send_total_ns / logger_send_count as u128
                    } else {
                        0
                    };

                    let avg_logger_to_adapter_arrival_ns = if logger_to_adapter_count > 0 {
                        logger_to_adapter_total_ns / logger_to_adapter_count as u128
                    } else {
                        0
                    };

                    // Batch-Wartezeit = früheste Event-Ankunft im Chunk -> Batch-Send
                    let batch_wait_ns: u128 = if batch_started_at_realtime > 0 {
                        (batch_sent_at_realtime - batch_started_at_realtime).max(0) as u128
                    } else {
                        0
                    };

                    if batch_wait_ns > 0 {
                        sender_metrics
                            .batch_wait_ns_total
                            .fetch_add(batch_wait_ns as u64, Ordering::Relaxed);
                        AdapterMetrics::update_max(
                            &sender_metrics.batch_wait_ns_max,
                            batch_wait_ns as u64,
                        );
                    }

                    sender_metrics.batch_count.fetch_add(1, Ordering::Relaxed);
                    if publish_snapshot {
                        sender_metrics
                            .publish_batch_count
                            .fetch_add(1, Ordering::Relaxed);
                    }

                    // CSV-Zeile schreiben

                    if let Some(summary_writer) = &summary_writer {
                        summary_writer.write_row(&EventSharingSummaryCsvRow
                        {
                            batch_id: batch_id_str.clone(),
                            batch_seq: chunk_seq,
                            completed_bin_start,
                            completed_bin_end,
                            event_count,
                            publish_snapshot,
                            empty_bin,
                            batch_started_at_realtime,
                            batch_sent_at_realtime,
                            batch_wait_ns,
                            avg_event_residence_ns,
                            max_event_residence_ns,
                            avg_event_detected_to_batch_send_ns,
                            max_event_detected_to_batch_send_ns,
                            avg_logger_send_to_batch_send_ns,
                            max_logger_send_to_batch_send_ns,
                            avg_logger_to_adapter_arrival_ns,
                            max_logger_to_adapter_arrival_ns,
                        });
                    }

                    let batch_req = BatchRequest {
                        batch_id: msg.batch_id.as_bytes().to_vec(),
                        requests: msg.events.into_iter().map(Request::from).collect(),
                        publish_snapshot,
                        completed_bin_start,
                        completed_bin_end,
                        empty_bin,
                        batch_sent_at_realtime,
                    };
                    info!(
                        "EventSharingServiceAdapter: sending {} chunk #{} for bin [{}, {}) to Java (publish_snapshot={}, empty_bin={}, events={})",
                        msg.batch_id,
                        chunk_seq,
                        completed_bin_start,
                        completed_bin_end,
                        publish_snapshot,
                        empty_bin,
                        event_count
                    );
                    let result = runtime.block_on(sender_client.update_router_batch(batch_req));

                    match result {
                        Ok(_) => {
                            info!(
                                "EventSharingServiceAdapter: Java acknowledged chunk #{} for bin [{}, {})",
                                chunk_seq,
                                completed_bin_start,
                                completed_bin_end
                                );
                        }
                        Err(e) => {
                            panic!(
                                "EventSharingServiceAdapter: failed to send chunk #{} for bin [{}, {}) to Java: {}",
                                chunk_seq,
                                completed_bin_start,
                                completed_bin_end,
                                e
                            );
                        }
                    }
                }

                let incoming = sender_metrics.incoming_event_count.load(Ordering::Relaxed);
                let batches = sender_metrics.batch_count.load(Ordering::Relaxed);
                let publish_batches = sender_metrics.publish_batch_count.load(Ordering::Relaxed);
                let total_residence = sender_metrics
                    .adapter_event_residence_ns_total
                    .load(Ordering::Relaxed);
                let max_residence = sender_metrics
                    .adapter_event_residence_ns_max
                    .load(Ordering::Relaxed);
                let total_batch_wait = sender_metrics
                    .batch_wait_ns_total
                    .load(Ordering::Relaxed);
                let max_batch_wait = sender_metrics
                    .batch_wait_ns_max
                    .load(Ordering::Relaxed);

                info!(
                     "FINAL_EVENT_ADAPTER_STATS incoming_events={} batches={} publish_batches={} avg_event_residence_ns={} max_event_residence_ns={} avg_batch_wait_ns={} max_batch_wait_ns={}",
                     incoming,
                     batches,
                     publish_batches,
                     if incoming > 0 { total_residence / incoming as u64 } else { 0 },
                     max_residence,
                     if batches > 0 { total_batch_wait / batches as u64 } else { 0 },
                     max_batch_wait
                     );

                info!("EventSharingServiceAdapter: outgoing sender thread exiting.");
            })
            .expect("Failed to spawn event-sharing sender thread");

        Self {
            clients: RingIter::new(clients),
            shutdown_handles,
            bin_size_secs,
            max_events_per_bin_chunk,
            oldest_unfinalized_bin_start: None,
            bins: BTreeMap::new(),
            outgoing_tx: tx,
            metrics,
            received_events_count: 0,
        }
    }

    fn flush_full_chunks_for_bin(&mut self, bin_start_value: u32) {
        let chunk_size = self.max_events_per_bin_chunk;

        loop {
            let maybe_chunk = {
                let state = self
                    .bins
                    .get_mut(&bin_start_value)
                    .expect("Bin state must exist while flushing full chunks");

                if state.pending_events.len() < chunk_size {
                    None
                } else {
                    let chunk: Vec<_> = state.pending_events.drain(0..chunk_size).collect();
                    state.sent_chunk_count += 1;
                    let chunk_seq = state.sent_chunk_count;
                    Some((chunk, chunk_seq))
                }
            };

            let Some((chunk, chunk_seq)) = maybe_chunk else {
                break;
            };

            self.enqueue_chunk(
                chunk,
                bin_start_value,
                bin_end(bin_start_value, self.bin_size_secs),
                false,
                false,
                chunk_seq,
            );
        }
    }

    fn finalize_bins_up_to(&mut self, triggering_event_time: u32) {
        while let Some(current_start) = self.oldest_unfinalized_bin_start {
            let current_end = bin_end(current_start, self.bin_size_secs);

            if !is_bin_safely_closed(current_end, triggering_event_time) {
                break;
            }

            let mut state = self
                .bins
                .remove(&current_start)
                .unwrap_or_else(BinState::new);

            let chunk_seq = state.sent_chunk_count + 1;

            if state.saw_any_event {
                if state.pending_events.is_empty() {
                    // Alle Events dieses Bins wurden schon vorher gestreamt.
                    // Wir senden jetzt nur noch den finalen leeren Abschluss-Chunk.
                    info!(
 "EventSharingServiceAdapter: finalizing bin [{}, {}) with EMPTY final marker triggered by event at t={}",
 current_start, current_end, triggering_event_time
 );

                    self.enqueue_chunk(
                        Vec::new(),
                        current_start,
                        current_end,
                        false,
                        true,
                        chunk_seq,
                    );
                } else {
                    let remaining = mem::take(&mut state.pending_events);
                    // info!(
                    // "EventSharingServiceAdapter: finalizing bin [{}, {}) with final chunk of {} events triggered by event at t={}",
                    // current_start,
                    // current_end,
                    // remaining.len(),
                    // triggering_event_time
                    // );

                    self.enqueue_chunk(
                        remaining,
                        current_start,
                        current_end,
                        false,
                        true,
                        chunk_seq,
                    );
                }
            } else {
                // Echter leerer Bin
                warn!(
 "EventSharingServiceAdapter: finalizing EMPTY bin [{}, {}) triggered by event at t={}",
 current_start, current_end, triggering_event_time
 );

                self.enqueue_chunk(
                    Vec::new(),
                    current_start,
                    current_end,
                    true,
                    true,
                    chunk_seq,
                );
            }

            // Nächster noch nicht finalisierter Bin ist strikt der Nachfolger.
            self.oldest_unfinalized_bin_start = Some(current_end);
        }
    }

    fn enqueue_chunk(
        &mut self,
        events: Vec<InternalEventSharingRequestPayload>,
        completed_bin_start: u32,
        completed_bin_end: u32,
        empty_bin: bool,
        publish_snapshot: bool,
        chunk_seq: usize,
    ) {
        // Events vor dem Versenden nach now und Eventtyp sortieren
        // events.sort_by(|a, b| {
        //     a.now.cmp(&b.now).then_with(|| {
        //         event_type_priority(&a.event_type).cmp(&event_type_priority(&b.event_type))
        //     })
        // });
        let batch_started_at_realtime = events
            .iter()
            .filter_map(|e| e.adapter_arrived_at_realtime)
            .min();
        let msg = OutgoingBinChunk {
            events,
            completed_bin_start,
            completed_bin_end,
            empty_bin,
            publish_snapshot,
            chunk_seq,
            batch_started_at_realtime,
            batch_id: Uuid::now_v7(),
        };

        self.outgoing_tx
            .send(msg)
            .expect("EventSharingServiceAdapter: outgoing sender channel unexpectedly closed");
    }
}

fn unix_nanos_now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("SystemTime before UNIX EPOCH!")
        .as_nanos() as i64
}
