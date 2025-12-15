pub mod event_sharing_logger;

use crate::external_services::{RequestAdapter, RequestAdapterFactory, RequestToAdapter};
// use crate::external_services::event_sharing::event_sharing_logger::make_event_sharing_subscriber;
use crate::generated::event_sharing::event_sharing_service_client::EventSharingServiceClient;
use crate::generated::event_sharing::{Request, Ack};
use crate::simulation::config::Config;
use crate::simulation::data_structures::RingIter;
use derive_builder::Builder;
//use itertools::{EitherOrBoth, Itertools};
use std::sync::{Arc, Mutex};
// use tokio::sync::oneshot;
// use tokio::sync::oneshot::Sender;
use tokio::task::JoinHandle;
use tracing::info;
use uuid::Uuid;
use crate::simulation::events::{EventTrait, EventsPublisher};

pub struct EventSharingServiceAdapter {
    clients: RingIter<EventSharingServiceClient<tonic::transport::Channel>>,
    shutdown_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

#[derive(Debug)]
pub struct InternalEventSharingRequest {
    pub payload: InternalEventSharingRequestPayload,
    //pub response_tx: Sender<InternalEventSharingResponse>,
}

impl RequestToAdapter for InternalEventSharingRequest {}

#[derive(Debug, PartialEq, Builder)]
pub struct InternalEventSharingRequestPayload {
    pub link_type: String,
    pub link_id: String,
    pub vehicle_id: String,
    pub now: u32,
    #[builder(default = "Uuid::now_v7()")]
    pub uuid: Uuid,
}

impl InternalEventSharingRequestPayload {
    pub fn equals_ignoring_uuid(&self, other: &Self) -> bool {
        self.link_type == other.link_type
            && self.link_id == other.link_id
            && self.vehicle_id == other.vehicle_id
            && self.now == other.now
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
            link_type: req.link_type,
            link_id: req.link_id,
            vehicle_id: req.vehicle_id,
            now: req.now,
            request_id: req.uuid.as_bytes().to_vec(),
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
        }
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
        EventSharingServiceAdapter::new(res, self.shutdown_handles)
    }
}

impl RequestAdapter<InternalEventSharingRequest> for EventSharingServiceAdapter {
    fn on_request(&mut self, internal_req: InternalEventSharingRequest) {
        let mut client = self.clients.next_cloned();

        tokio::spawn(async move {
            let request = Request::from(internal_req.payload);

            let _response =client
                .update_router(request)
                .await
                .expect("Error while calling event sharing service");

             //let internal_res = InternalEventSharingResponse::from(response.into_inner());

             //let _ = internal_req.response_tx.send(internal_res);
        });
    }

    fn on_shutdown(&mut self) {
        for client in &mut self.clients {
            let mut c = client.clone();
            let handle = tokio::spawn(async move {
                c.shutdown(())
                    .await
                    .expect("Error while shutting down event sharing service");
            });
            self.shutdown_handles.lock().unwrap().push(handle);
        }
    }
}

impl EventSharingServiceAdapter {
    fn new(
        clients: Vec<EventSharingServiceClient<tonic::transport::Channel>>,
        shutdown_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    ) -> Self {
        Self {
            clients: RingIter::new(clients),
            shutdown_handles,
        }
    }
}