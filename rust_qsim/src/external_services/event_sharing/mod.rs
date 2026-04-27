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

/// Ein Bin [start, end) gilt erst dann als sicher abgeschlossen,
/// wenn ein Event mit t >= end + 2 eingetroffen ist.
///
/// Begründung:
/// - Simulation läuft in ganzzahligen Sekunden
/// - Events kommen zeitlich sortiert
/// - bei t = end + 1 kann theoretisch noch ein Event mit t < end kommen
/// - erst bei t = end + 2 ist ausgeschlossen, dass noch ein Event aus dem
/// gerade abgeschlossenen Bin nachkommt
fn is_bin_safely_closed(current_bin_end: u32, event_time: u32) -> bool {
    event_time >= current_bin_end + BIN_FINALIZATION_LAG_SECS
}

#[derive(Debug)]
struct BinState {
    /// Noch nicht gesendete Events dieses Bins.
    pending_events: Vec<InternalEventSharingRequestPayload>,
    /// Ob dieser Bin jemals ein Event gesehen hat.
    saw_any_event: bool,
    /// Nur für Logging.
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
}
pub struct EventSharingServiceAdapter {
    clients: RingIter<EventSharingServiceClient<tonic::transport::Channel>>,
    shutdown_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,

    bin_size_secs: u32,
    max_events_per_bin_chunk: usize,
    /// Start des ältesten noch NICHT finalisierten Bins.
    oldest_unfinalized_bin_start: Option<u32>,
    /// Zustand pro Bin.
    bins: BTreeMap<u32, BinState>,
    /// Serieller Versandpfad nach Java.
    outgoing_tx: Sender<OutgoingBinChunk>,
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

/// Factory for creating event sharing service adapters.
/// Connects to the event sharing service at the given IP address.
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

    /// Kompatibilitäts-No-Op
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

        let mut event = internal_req.payload;
        let event_time = event.now;
        let event_bin_start = bin_start(event_time, self.bin_size_secs);

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

        let mut sender_client = clients
            .first()
            .expect("At least one event sharing client must exist")
            .clone();

        thread::Builder::new()
            .name("event-sharing-bin-sender".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to build tokio runtime for event-sharing sender");

                while let Ok(msg) = rx.recv() {
                    let event_count = msg.events.len();

                    // Werte aus msg früh sichern, bevor msg.events verbraucht wird
                    let chunk_seq = msg.chunk_seq;
                    let completed_bin_start = msg.completed_bin_start;
                    let completed_bin_end = msg.completed_bin_end;
                    let publish_snapshot = msg.publish_snapshot;

                    let batch_req = BatchRequest {
                        requests: msg.events.into_iter().map(Request::from).collect(),
                        publish_snapshot,
                        completed_bin_start,
                        completed_bin_end,
                    };
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
        let msg = OutgoingBinChunk {
            events,
            completed_bin_start,
            completed_bin_end,
            empty_bin,
            publish_snapshot,
            chunk_seq,
        };

        self.outgoing_tx
            .send(msg)
            .expect("EventSharingServiceAdapter: outgoing sender channel unexpectedly closed");
    }
}
