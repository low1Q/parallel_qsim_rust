pub mod event_sharing_logger;

use crate::external_services::{RequestAdapter, RequestAdapterFactory, RequestToAdapter};
use crate::generated::event_sharing::event_sharing_service_client::EventSharingServiceClient;
use crate::generated::event_sharing::{Ack, BatchRequest, Request};
use crate::simulation::config::Config;
use crate::simulation::data_structures::RingIter;
use derive_builder::Builder;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;
use tracing::info;
use uuid::Uuid;

pub struct EventSharingServiceAdapter {
    clients: RingIter<EventSharingServiceClient<tonic::transport::Channel>>,
    shutdown_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    // Batching
    buffer: Arc<Mutex<Vec<InternalEventSharingRequestPayload>>>,
    max_batch_size: usize,
    batch_interval_millisecs: u64,
    flusher_handle: Option<JoinHandle<()>>,
}

#[derive(Debug)]
pub struct InternalEventSharingRequest {
    pub payload: InternalEventSharingRequestPayload,
    //pub response_tx: Sender<InternalEventSharingResponse>,
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

/// Factory for creating event sharing service adapters. Connects to the event sharing service at the given IP address.
pub struct EventSharingServiceAdapterFactory {
    ip: Vec<String>,
    config: Arc<Config>,
    shutdown_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    // Batching
    max_batch_size: usize,
    batch_interval_millisecs: u64,
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
            // Batching
            max_batch_size: 50,
            batch_interval_millisecs: 1,
        }
    }
    pub fn with_batch_params(
        mut self,
        max_batch_size: usize,
        batch_interval_millisecs: u64,
    ) -> Self {
        self.max_batch_size = max_batch_size;
        self.batch_interval_millisecs = batch_interval_millisecs;
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
            self.max_batch_size,
            self.batch_interval_millisecs,
        )
    }
}

impl RequestAdapter<InternalEventSharingRequest> for EventSharingServiceAdapter {
    fn on_request(&mut self, internal_req: InternalEventSharingRequest) {
        // Nur in den Puffer schreiben; Flusher kümmert sich ums Senden.
        {
            let mut buf = self.buffer.lock().unwrap();
            buf.push(internal_req.payload);
            if buf.len() >= self.max_batch_size {
                // Ziehe sofort eine Charge ab und sende asynchron
                let to_send = std::mem::take(&mut *buf);
                let mut client = self.clients.next_cloned();
                // spawn send task
                tokio::spawn(async move {
                    let batch_req = BatchRequest {
                        requests: to_send.into_iter().map(Request::from).collect(),
                    };
                    let _ = client.update_router_batch(batch_req).await;
                });
            }
        }
    }

    fn on_shutdown(&mut self) {
        info!("EventSharingServiceAdapter: Starting shutdown sequence...");
        // Flush remaining buffer synchronously (spawn tasks to send them)
        let leftover = {
            let mut buf = self.buffer.lock().unwrap();
            std::mem::take(&mut *buf)
        };
        if !leftover.is_empty() {
            let mut client = self.clients.next_cloned();
            let _h = tokio::spawn(async move {
                let batch_req = BatchRequest {
                    requests: leftover.into_iter().map(Request::from).collect(),
                };
                if let Err(e) = client.update_router_batch(batch_req).await {
                    eprintln!("Final flush failed: {}", e);
                }
            });
            self.shutdown_handles.lock().unwrap().push(_h);
        }

        // Stop flusher task if present
        if let Some(handle) = self.flusher_handle.take() {
            handle.abort();
            info!("EventSharingServiceAdapter: Periodic flusher task aborted.");
        }
        // Shutdown all clients
        self.clients = RingIter::new(vec![]);

        info!("EventSharingServiceAdapter: All clients dropped and resources cleared.");
    }
}

impl EventSharingServiceAdapter {
    fn new(
        clients: Vec<EventSharingServiceClient<tonic::transport::Channel>>,
        shutdown_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
        max_batch_size: usize,
        batch_interval_millisecs: u64,
    ) -> Self {
        let buffer = Arc::new(Mutex::new(Vec::with_capacity(max_batch_size)));
        let buf_clone = buffer.clone();
        let clients_for_flusher = clients.clone();
        let clients_ring = RingIter::new(clients);
        let mut clients_ring_for_flusher = RingIter::new(clients_for_flusher);
        let flusher_handle = {
            // Flusher-Task: periodisch flushen
            let mut clients_ring_inner = clients_ring_for_flusher;
            tokio::spawn(async move {
                let interval = tokio::time::interval(std::time::Duration::from_millis(
                    batch_interval_millisecs,
                ));
                tokio::pin!(interval);
                loop {
                    interval.as_mut().tick().await;
                    let to_send = {
                        let mut guard = buf_clone.lock().unwrap();
                        if guard.is_empty() {
                            continue;
                        }
                        std::mem::take(&mut *guard)
                    };
                    if to_send.is_empty() {
                        continue;
                    }
                    let mut client = clients_ring_inner.next_cloned();
                    let batch_req = BatchRequest {
                        requests: to_send.into_iter().map(Request::from).collect(),
                    };
                    // Best-Effort send; log on error
                    if let Err(e) = client.update_router_batch(batch_req).await {
                        eprintln!("Error sending batch to event sharing service: {}", e);
                        // optional: requeue or drop
                    }
                }
            })
        };
        Self {
            clients: clients_ring,
            shutdown_handles,
            buffer,
            max_batch_size,
            batch_interval_millisecs: batch_interval_millisecs,
            flusher_handle: Some(flusher_handle),
        }
    }
}
