// pub mod event_sharing_logger;
//
// use crate::external_services::{RequestAdapter, RequestAdapterFactory, RequestToAdapter};
// use crate::generated::event_sharing::event_sharing_service_client::EventSharingServiceClient;
// use crate::generated::event_sharing::{Ack, BatchRequest, Request};
// use crate::simulation::config::Config;
// use crate::simulation::data_structures::RingIter;
// use derive_builder::Builder;
// use std::collections::BTreeMap;
// use std::sync::{Arc, Mutex};
// use tokio::task::JoinHandle;
// use tracing::{info, warn};
// use uuid::Uuid;
// use std::sync::mpsc::{self, Sender};
// use std::thread;
//
// const DEFAULT_BIN_SIZE_SECS: u32 = 900;
// const BIN_FINALIZATION_LAG_SECS: u32 = 2;
//
// // pub struct EventSharingServiceAdapter {
// //     clients: RingIter<EventSharingServiceClient<tonic::transport::Channel>>,
// //     shutdown_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
// //     // Batching
// //     buffer: Arc<Mutex<Vec<InternalEventSharingRequestPayload>>>,
// //     max_batch_size: usize,
// //     batch_interval_millisecs: u64,
// //     flusher_handle: Option<JoinHandle<()>>,
// // }
//
// pub struct EventSharingServiceAdapter {
//     clients: RingIter<EventSharingServiceClient<tonic::transport::Channel>>,
//     shutdown_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
//     bin_size_secs: u32,
//     /// Start des ältesten noch NICHT finalisierten Bins.
//     ///
//     /// Beispiel:
//     /// Wenn dies Some(900) ist, dann kann es bereits Events für 900, 1800, 2700, ...
//     /// in `bins` geben, aber finalisiert werden darf immer nur beginnend bei 900
//     /// und dann streng aufsteigend.
//     oldest_unfinalized_bin_start: Option<u32>,
//     /// Gepufferte Events pro Bin.
//     ///
//     /// Es können bewusst mehrere Bins gleichzeitig existieren:
//     /// z.B. Events für Bin 900 und schon erste Events für Bin 1800,
//     /// obwohl Bin 900 wegen der +2-Regel noch nicht finalisiert werden darf.
//     bins: BTreeMap<u32, Vec<InternalEventSharingRequestPayload>>,
//     closed_bin_tx: Sender<ClosedBinMessage>,
// }
//
// #[derive(Debug)]
// pub struct InternalEventSharingRequest {
//     pub payload: InternalEventSharingRequestPayload,
// }
//
// #[derive(Debug)]
// struct ClosedBinMessage {
//     events: Vec<InternalEventSharingRequestPayload>,
//     completed_bin_start: u32,
//     completed_bin_end: u32,
//     empty_bin: bool,
// }
//
// impl RequestToAdapter for InternalEventSharingRequest {}
//
// #[derive(Debug, PartialEq, Builder)]
// pub struct InternalEventSharingRequestPayload {
//     pub event_type: String,
//     pub link_id: String,
//     pub vehicle_id: String,
//     pub now: u32,
//     pub driver_id: Option<String>,
//     pub network_mode: Option<String>,
//     pub relative_position_on_link: Option<f64>,
// }
//
// impl InternalEventSharingRequestPayload {
//     pub fn equals_ignoring_uuid(&self, other: &Self) -> bool {
//         self.event_type == other.event_type
//             && self.link_id == other.link_id
//             && self.vehicle_id == other.vehicle_id
//             && self.now == other.now
//             && self.driver_id == other.driver_id
//             && self.network_mode == other.network_mode
//             && self.relative_position_on_link == other.relative_position_on_link
//     }
// }
//
// #[derive(Debug, Clone, Default)]
// pub struct InternalEventSharingResponse {
//     pub(crate) message_received: bool,
//     pub(crate) request_id: Uuid,
// }
//
// impl From<InternalEventSharingRequestPayload> for Request {
//     fn from(req: InternalEventSharingRequestPayload) -> Self {
//         Request {
//             event_type: req.event_type,
//             link_id: req.link_id,
//             vehicle_id: req.vehicle_id,
//             now: req.now,
//             network_mode: req.network_mode,
//             driver_id: req.driver_id,
//             relative_position_on_link: req.relative_position_on_link,
//         }
//     }
// }
//
// impl From<Ack> for InternalEventSharingResponse {
//     fn from(value: Ack) -> Self {
//         Self {
//             message_received: value.message_received,
//             request_id: Uuid::from_bytes(value.request_id.try_into().expect("Invalid UUID bytes")),
//         }
//     }
// }
//
// /// Factory for creating event sharing service adapters. Connects to the event sharing service at the given IP address.
// // pub struct EventSharingServiceAdapterFactory {
// //     ip: Vec<String>,
// //     config: Arc<Config>,
// //     shutdown_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
// //     // Batching
// //     max_batch_size: usize,
// //     batch_interval_millisecs: u64,
// // }
//
// pub struct EventSharingServiceAdapterFactory {
//     ip: Vec<String>,
//     config: Arc<Config>,
//     shutdown_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
//     bin_size_secs: u32,
// }
//
// impl EventSharingServiceAdapterFactory {
//     pub fn new(
//         ip: Vec<impl Into<String>>,
//         config: Arc<Config>,
//         shutdown_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
//     ) -> Self {
//         Self {
//             ip: ip.into_iter().map(|s| s.into()).collect(),
//             config,
//             shutdown_handles,
//             // Batching
//             // max_batch_size: 50,
//             // batch_interval_millisecs: 1,
//             bin_size_secs: DEFAULT_BIN_SIZE_SECS,
//         }
//     }
//
//     /// Beibehalten für Kompatibilität mit bestehendem Aufrufcode.
//     /// Die alte flush-/batch-getriebene Semantik wird hier bewusst nicht mehr verwendet.
//     pub fn with_batch_params(
//         self,
//         _max_batch_size: usize,
//         _batch_interval_millisecs: u64,
//     ) -> Self {
//         self
//     }
//
//     pub fn with_bin_size_secs(mut self, bin_size_secs: u32) -> Self {
//         self.bin_size_secs = bin_size_secs;
//         self
//     }
//
//     // pub fn with_batch_params(
//     //     mut self,
//     //     max_batch_size: usize,
//     //     batch_interval_millisecs: u64,
//     // ) -> Self {
//     //     self.max_batch_size = max_batch_size;
//     //     self.batch_interval_millisecs = batch_interval_millisecs;
//     //     self
//     // }
// }
//
// impl RequestAdapterFactory<InternalEventSharingRequest> for EventSharingServiceAdapterFactory {
//     async fn build(self) -> impl RequestAdapter<InternalEventSharingRequest> {
//         let mut res = Vec::new();
//         for ip in self.ip {
//             info!("Connecting to event sharing service at {}", ip);
//             let start = std::time::Instant::now();
//             let client;
//             loop {
//                 match EventSharingServiceClient::connect(ip.clone()).await {
//                     Ok(c) => {
//                         client = c;
//                         break;
//                     }
//                     Err(e) => {
//                         if start.elapsed().as_secs()
//                             >= self.config.computational_setup().retry_time_seconds
//                         {
//                             panic!(
//                                 "Failed to connect to event sharing service at {} after configured retry maximum: {}",
//                                 ip, e
//                             );
//                         }
//                         tokio::time::sleep(std::time::Duration::from_secs(1)).await;
//                     }
//                 }
//             }
//             res.push(client);
//         }
//         EventSharingServiceAdapter::new(res, self.shutdown_handles, self.bin_size_secs)
//     }
// }
//
// fn bin_start(t: u32, bin_size: u32) -> u32 {
//     (t / bin_size) * bin_size
// }
// fn bin_end(bin_start: u32, bin_size: u32) -> u32 {
//     bin_start + bin_size
// }
// /// Ein Bin [start, end) gilt erst dann als sicher abgeschlossen,
// /// wenn ein Event mit t >= end + 2 eingetroffen ist.
// ///
// /// Begründung:
// /// - Simulation läuft in ganzzahligen Sekunden
// /// - Events kommen zeitlich sortiert
// /// - bei t = end + 1 kann theoretisch noch ein Event mit t <= end kommen
// /// - erst bei t = end + 2 ist ausgeschlossen, dass noch ein Event aus dem
// ///   gerade abgeschlossenen Bin nachkommt
// fn is_bin_safely_closed(current_bin_end: u32, event_time: u32) -> bool {
//     event_time >= current_bin_end + BIN_FINALIZATION_LAG_SECS
// }
//
// impl RequestAdapter<InternalEventSharingRequest> for EventSharingServiceAdapter {
//     // fn on_request(&mut self, internal_req: InternalEventSharingRequest) {
//     //     // Nur in den Puffer schreiben; Flusher kümmert sich ums Senden.
//     //     {
//     //         let mut buf = self.buffer.lock().unwrap();
//     //         buf.push(internal_req.payload);
//     //         if buf.len() >= self.max_batch_size {
//     //             // Ziehe sofort eine Charge ab und sende asynchron
//     //             let to_send = std::mem::take(&mut *buf);
//     //             let mut client = self.clients.next_cloned();
//     //             // spawn send task
//     //             tokio::spawn(async move {
//     //                 let batch_req = BatchRequest {
//     //                     requests: to_send.into_iter().map(Request::from).collect(),
//     //                 };
//     //                 let _ = client.update_router_batch(batch_req).await;
//     //             });
//     //         }
//     //     }
//     // }
//
//     fn on_request(&mut self, internal_req: InternalEventSharingRequest) {
//         let event = internal_req.payload;
//         let event_time = event.now;
//         let event_bin_start = bin_start(event_time, self.bin_size_secs);
//
//         if self.oldest_unfinalized_bin_start.is_none() {
//             self.oldest_unfinalized_bin_start = Some(event_bin_start);
//             info!(
//                 "EventSharingServiceAdapter: opening first bin [{}, {})",
//                 event_bin_start,
//                 bin_end(event_bin_start, self.bin_size_secs)
//             );
//         }
//
//         // Event immer zuerst in seinen echten Bin einsortieren.
//         self.bins.entry(event_bin_start).or_default().push(event);
//
//         // Danach so viele der ältesten Bins wie möglich finalisieren.
//         self.finalize_bins_up_to(event_time);
//     }
//
//
//     fn on_shutdown(&mut self) {
//         info!("EventSharingServiceAdapter: Starting shutdown sequence...");
//
//         if let Some(oldest) = self.oldest_unfinalized_bin_start {
//             let latest_bin_with_events = self.bins.keys().next_back().copied();
//
//             match latest_bin_with_events {
//                 Some(latest) => {
//                     warn!(
//                         "EventSharingServiceAdapter: shutdown with unfinalized bins starting at [{} , {}). \
// Latest buffered bin start is {}. These bins are NOT published because they are not known to be safely closed.",
//                         oldest,
//                         bin_end(oldest, self.bin_size_secs),
//                         latest
//                     );
//                 }
//                 None => {
//                     info!(
//                         "EventSharingServiceAdapter: shutdown with oldest_unfinalized_bin_start={}, but no buffered events remain.",
//                         oldest
//                     );
//                 }
//             }
//         } else {
//             info!("EventSharingServiceAdapter: shutdown with no open bins.");
//         }
//
//         self.clients = RingIter::new(vec![]);
//         info!("EventSharingServiceAdapter: All clients dropped and resources cleared.");
//     }
// }
//
// impl EventSharingServiceAdapter {
//     fn new(
//         clients: Vec<EventSharingServiceClient<tonic::transport::Channel>>,
//         shutdown_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
//         bin_size_secs: u32,
//     ) -> Self {
//         Self {
//             clients: RingIter::new(clients),
//             shutdown_handles,
//             bin_size_secs,
//             oldest_unfinalized_bin_start: None,
//             bins: BTreeMap::new(),
//         }
//     }
//
//     fn finalize_bins_up_to(&mut self, triggering_event_time: u32) {
//         while let Some(current_start) = self.oldest_unfinalized_bin_start {
//             let current_end = bin_end(current_start, self.bin_size_secs);
//
//             if !is_bin_safely_closed(current_end, triggering_event_time) {
//                 break;
//             }
//
//             let events_to_send = self.bins.remove(&current_start).unwrap_or_default();
//             let empty_bin = events_to_send.is_empty();
//
//             if empty_bin {
//                 warn!(
//                     "EventSharingServiceAdapter: closing EMPTY bin [{}, {}) triggered by event at t={}",
//                     current_start, current_end, triggering_event_time
//                 );
//             } else {
//                 info!(
//                     "EventSharingServiceAdapter: closing bin [{}, {}) with {} events triggered by event at t={}",
//                     current_start,
//                     current_end,
//                     events_to_send.len(),
//                     triggering_event_time
//                 );
//             }
//
//             self.spawn_send_closed_bin(events_to_send, current_start, current_end, empty_bin);
//
//             // Nächster noch nicht finalisierter Bin ist streng der Nachfolger.
//             self.oldest_unfinalized_bin_start = Some(current_end);
//
//             // Kleines Aufräumen:
//             // Falls es ab jetzt keinerlei Events mehr in späteren Bins gibt, lassen wir den
//             // nächsten Bin trotzdem als "offen" bestehen. Das ist korrekt, weil er später leer
//             // geschlossen werden kann, sobald ein genügend spätes Event eintrifft.
//         }
//     }
//
//     fn spawn_send_closed_bin(
//         &mut self,
//         events: Vec<InternalEventSharingRequestPayload>,
//         completed_bin_start: u32,
//         completed_bin_end: u32,
//         empty_bin: bool,
//     ) {
//         let mut client = self.clients.next_cloned();
//
//         let handle = tokio::spawn(async move {
//             let batch_req = BatchRequest {
//                 requests: events.into_iter().map(Request::from).collect(),
//                 publish_snapshot: true,
//                 completed_bin_start,
//                 completed_bin_end,
//                 empty_bin,
//             };
//
//             if let Err(e) = client.update_router_batch(batch_req).await {
//                 eprintln!(
//                     "Error sending closed bin [{}, {}) to event sharing service: {}",
//                     completed_bin_start, completed_bin_end, e
//                 );
//             }
//         });
//
//         self.shutdown_handles.lock().unwrap().push(handle);
//     }
// }
//
// // impl EventSharingServiceAdapter {
// //     fn new(
// //         clients: Vec<EventSharingServiceClient<tonic::transport::Channel>>,
// //         shutdown_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
// //         max_batch_size: usize,
// //         batch_interval_millisecs: u64,
// //     ) -> Self {
// //         let buffer = Arc::new(Mutex::new(Vec::with_capacity(max_batch_size)));
// //         let buf_clone = buffer.clone();
// //         let clients_for_flusher = clients.clone();
// //         let clients_ring = RingIter::new(clients);
// //         let mut clients_ring_for_flusher = RingIter::new(clients_for_flusher);
// //         let flusher_handle = {
// //             // Flusher-Task: periodisch flushen
// //             let mut clients_ring_inner = clients_ring_for_flusher;
// //             tokio::spawn(async move {
// //                 let interval = tokio::time::interval(std::time::Duration::from_millis(
// //                     batch_interval_millisecs,
// //                 ));
// //                 tokio::pin!(interval);
// //                 loop {
// //                     interval.as_mut().tick().await;
// //                     let to_send = {
// //                         let mut guard = buf_clone.lock().unwrap();
// //                         if guard.is_empty() {
// //                             continue;
// //                         }
// //                         std::mem::take(&mut *guard)
// //                     };
// //                     if to_send.is_empty() {
// //                         continue;
// //                     }
// //                     let mut client = clients_ring_inner.next_cloned();
// //                     let batch_req = BatchRequest {
// //                         requests: to_send.into_iter().map(Request::from).collect(),
// //                     };
// //                     // Best-Effort send; log on error
// //                     if let Err(e) = client.update_router_batch(batch_req).await {
// //                         eprintln!("Error sending batch to event sharing service: {}", e);
// //                         // optional: requeue or drop
// //                     }
// //                 }
// //             })
// //         };
// //         Self {
// //             clients: clients_ring,
// //             shutdown_handles,
// //             buffer,
// //             max_batch_size,
// //             batch_interval_millisecs: batch_interval_millisecs,
// //             flusher_handle: Some(flusher_handle),
// //         }
// //     }
// // }

pub mod event_sharing_logger;

use crate::external_services::{RequestAdapter, RequestAdapterFactory, RequestToAdapter};
use crate::generated::event_sharing::event_sharing_service_client::EventSharingServiceClient;
use crate::generated::event_sharing::{Ack, BatchRequest, Request};
use crate::simulation::config::Config;
use crate::simulation::data_structures::RingIter;
use derive_builder::Builder;
use std::collections::BTreeMap;
use std::mem;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use tokio::task::JoinHandle;
use tracing::{info, warn};
use uuid::Uuid;

const DEFAULT_BIN_SIZE_SECS: u32 = 900;
const BIN_FINALIZATION_LAG_SECS: u32 = 2;
const DEFAULT_MAX_EVENTS_PER_BIN_CHUNK: usize = 1000;

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
///   gerade abgeschlossenen Bin nachkommt
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
    shutdown_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,

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
    pub(crate) request_id: Uuid,
}

impl From<InternalEventSharingRequestPayload> for Request {
    fn from(req: InternalEventSharingRequestPayload) -> Self {
        Request {
            event_type: req.event_type,
            link_id: req.link_id,
            vehicle_id: req.vehicle_id,
            now: req.now,
            network_mode: req.network_mode,
            driver_id: req.driver_id,
            relative_position_on_link: req.relative_position_on_link,
        }
    }
}

impl From<Ack> for InternalEventSharingResponse {
    fn from(value: Ack) -> Self {
        Self {
            message_received: value.message_received,
            request_id: Uuid::from_bytes(value.request_id.try_into().expect("Invalid UUID bytes")),
        }
    }
}

/// Factory for creating event sharing service adapters.
/// Connects to the event sharing service at the given IP address.
pub struct EventSharingServiceAdapterFactory {
    ip: Vec<String>,
    config: Arc<Config>,
    shutdown_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    bin_size_secs: u32,
    max_events_per_bin_chunk: usize,
}

impl EventSharingServiceAdapterFactory {
    pub fn new(
        ip: Vec<impl Into<String>>,
        config: Arc<Config>,
        shutdown_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
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
    pub fn with_batch_params(
        self,
        _max_batch_size: usize,
        _batch_interval_millisecs: u64,
    ) -> Self {
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
        let event = internal_req.payload;
        let event_time = event.now;
        let event_bin_start = bin_start(event_time, self.bin_size_secs);

        if self.oldest_unfinalized_bin_start.is_none() {
            self.oldest_unfinalized_bin_start = Some(event_bin_start);
            info!(
                "EventSharingServiceAdapter: opening first bin [{}, {})",
                event_bin_start,
                bin_end(event_bin_start, self.bin_size_secs)
            );
        }

        let state = self.bins.entry(event_bin_start).or_insert_with(BinState::new);
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
                    warn!(
                        "EventSharingServiceAdapter: shutdown with unfinalized bins starting at [{}, {}). Latest buffered bin start is {}. These bins are NOT finalized because they are not known to be safely closed.",
                        oldest,
                        bin_end(oldest, self.bin_size_secs),
                        latest
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

        // Clients droppen
        self.clients = RingIter::new(vec![]);

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

        // Für den seriellen Versand nehmen wir genau einen Client.
        // Das macht die Publish-Reihenfolge klar und beobachtbar.
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

                    let batch_req = BatchRequest {
                        requests: msg.events.into_iter().map(Request::from).collect(),
                        publish_snapshot: msg.publish_snapshot,
                        completed_bin_start: msg.completed_bin_start,
                        completed_bin_end: msg.completed_bin_end,
                        empty_bin: msg.empty_bin,
                    };

                    info!(
                        "EventSharingServiceAdapter: sending chunk #{} for bin [{}, {}) to Java (publish_snapshot={}, empty_bin={}, events={})",
                        msg.chunk_seq,
                        msg.completed_bin_start,
                        msg.completed_bin_end,
                        msg.publish_snapshot,
                        msg.empty_bin,
                        event_count
                    );

                    let result = runtime.block_on(sender_client.update_router_batch(batch_req));

                    match result {
                        Ok(_) => {
                            info!(
                                "EventSharingServiceAdapter: Java acknowledged chunk #{} for bin [{}, {})",
                                msg.chunk_seq,
                                msg.completed_bin_start,
                                msg.completed_bin_end
                            );
                        }
                        Err(e) => {
                            panic!(
                                "EventSharingServiceAdapter: failed to send chunk #{} for bin [{}, {}) to Java: {}",
                                msg.chunk_seq,
                                msg.completed_bin_start,
                                msg.completed_bin_end,
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

            let mut state = self.bins.remove(&current_start).unwrap_or_else(BinState::new);

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
                    info!(
                        "EventSharingServiceAdapter: finalizing bin [{}, {}) with final chunk of {} events triggered by event at t={}",
                        current_start,
                        current_end,
                        remaining.len(),
                        triggering_event_time
                    );

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