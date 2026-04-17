use crate::external_services::{RequestAdapter, RequestAdapterFactory, RequestToAdapter};
use crate::generated::routing::routing_service_client::RoutingServiceClient;
use crate::generated::routing::{Request, Response};
use crate::simulation::config::Config;
use crate::simulation::data_structures::RingIter;
use crate::simulation::population::{InternalActivity, InternalLeg, InternalPlanElement};
use derive_builder::Builder;
use itertools::{EitherOrBoth, Itertools};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};
use tokio::sync::oneshot::Sender;
use tokio::task::JoinHandle;
use tracing::info;
use uuid::Uuid;

fn unix_nanos_now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("SystemTime before UNIX EPOCH!")
        .as_nanos() as i64
}

pub struct RoutingServiceAdapter {
    clients: RingIter<RoutingServiceClient<tonic::transport::Channel>>,
    shutdown_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

#[derive(Debug)]
pub struct InternalRoutingRequest {
    pub payload: InternalRoutingRequestPayload,
    pub response_tx: Sender<InternalRoutingResponse>,
}

impl RequestToAdapter for InternalRoutingRequest {}

#[derive(Debug, PartialEq, Builder)]
pub struct InternalRoutingRequestPayload {
    pub person_id: String,
    pub from_link: String,
    pub from_x: f64,
    pub from_y: f64,
    pub to_link: String,
    pub to_x: f64,
    pub to_y: f64,
    pub mode: String,
    pub departure_time: u32,
    pub now: u32,
    pub route_call_start_realtime: i64,
    pub adapter_sent_request_grpc: Option<i64>,
    #[builder(default = "Uuid::now_v7()")]
    pub uuid: Uuid,
}

impl InternalRoutingRequestPayload {
    pub fn equals_ignoring_uuid(&self, other: &Self) -> bool {
        self.person_id == other.person_id
            && self.from_link == other.from_link
            && self.from_x == other.from_x
            && self.from_y == other.from_y
            && self.to_link == other.to_link
            && self.to_x == other.to_x
            && self.to_y == other.to_y
            && self.mode == other.mode
            && self.departure_time == other.departure_time
            && self.now == other.now
            && self.route_call_start_realtime == other.route_call_start_realtime
    }
}

#[derive(Debug, Clone, Default)]
pub struct InternalRoutingResponse {
    pub(crate) elements: Vec<InternalPlanElement>,
    pub(crate) request_id: Uuid,

    pub(crate) adapter_received_request_agent: Option<i64>,
    pub(crate) adapter_sent_request_grpc: Option<i64>,
    pub(crate) java_routing_service_sent_response_grpc: Option<i64>,
    pub(crate) adapter_received_response_grpc: Option<i64>,
    pub(crate) adapter_sent_response_agent: Option<i64>,
}

impl From<InternalRoutingRequestPayload> for Request {
    fn from(req: InternalRoutingRequestPayload) -> Self {
        Request {
            person_id: req.person_id,
            from_link_id: req.from_link,
            from_x: req.from_x,
            from_y: req.from_y,
            to_link_id: req.to_link,
            to_x: req.to_x,
            to_y: req.to_y,
            mode: req.mode,
            departure_time: req.departure_time,
            now: req.now,
            request_id: req.uuid.as_bytes().to_vec(),
            route_call_start_realtime: Option::from(req.route_call_start_realtime),
            rust_adapter_sent_request_grpc: Option::from(req.adapter_sent_request_grpc.unwrap_or_default()),
        }
    }
}

impl From<Response> for InternalRoutingResponse {
    fn from(value: Response) -> Self {
        //zip legs and activities
        let legs = value
            .legs
            .into_iter()
            .map(InternalLeg::from)
            .collect::<Vec<_>>();
        let activities = value
            .activities
            .into_iter()
            .map(InternalActivity::from)
            .collect::<Vec<_>>();

        let mut elements = Vec::new();
        for pair in legs.into_iter().zip_longest(activities.into_iter()) {
            match pair {
                EitherOrBoth::Both(l, a) => {
                    elements.push(InternalPlanElement::Leg(l));
                    elements.push(InternalPlanElement::Activity(a));
                }
                EitherOrBoth::Left(l) => {
                    elements.push(InternalPlanElement::Leg(l));
                }
                EitherOrBoth::Right(_) => {
                    panic!("Received routing response ends with an activity, but expected a leg.");
                }
            }
        }

        Self {
            elements,
            request_id: Uuid::from_bytes(value.request_id.try_into().unwrap()),
            adapter_received_request_agent: None,
            adapter_sent_request_grpc: None,
            java_routing_service_sent_response_grpc: Some(value.java_routing_service_sent_response_grpc.unwrap_or_default()),
            adapter_received_response_grpc: None,
            adapter_sent_response_agent: None,
        }
    }
}

/// Factory for creating routing service adapters. Connects to the routing service at the given IP address.
pub struct RoutingServiceAdapterFactory {
    ip: Vec<String>,
    config: Arc<Config>,
    shutdown_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl RoutingServiceAdapterFactory {
    pub fn new(
        ip: Vec<impl Into<String>>,
        config: Arc<Config>,
        shutdown_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    ) -> Self {
        Self {
            ip: ip.into_iter().map(|s| s.into()).collect(),
            config,
            shutdown_handles,
        }
    }
}

impl RequestAdapterFactory<InternalRoutingRequest> for RoutingServiceAdapterFactory {
    async fn build(self) -> impl RequestAdapter<InternalRoutingRequest> {
        let mut res = Vec::new();
        for ip in self.ip {
            info!("Connecting to routing service at {}", ip);
            let start = Instant::now();
            let client;
            loop {
                match RoutingServiceClient::connect(ip.clone()).await {
                    Ok(c) => {
                        client = c;
                        break;
                    }
                    Err(e) => {
                        if start.elapsed().as_secs()
                            >= self.config.computational_setup().retry_time_seconds
                        {
                            panic!(
                                "Failed to connect to routing service at {} after configured retry maximum: {}",
                                ip, e
                            );
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
            }
            res.push(client);
        }
        RoutingServiceAdapter::new(res, self.shutdown_handles)
    }
}

impl RequestAdapter<InternalRoutingRequest> for RoutingServiceAdapter {
    fn on_request(&mut self, mut internal_req: InternalRoutingRequest) {
        let full_measurement = crate::simulation::profiling::flags::performance_logging_enabled();

        let adapter_received_request_agent = if full_measurement {
            Some(unix_nanos_now())
        } else {
            None
        };

        let mut client = self.clients.next_cloned();

        tokio::spawn(async move {
            let rust_adapter_sent_request_grpc = if full_measurement {
                Some(unix_nanos_now())
            } else {
                None
            };

            if full_measurement {
                internal_req.payload.adapter_sent_request_grpc = rust_adapter_sent_request_grpc;
            }

            let request = Request::from(internal_req.payload);

            let response = client
                .get_route(request)
                .await
                .expect("Error while calling routing service");

            let adapter_received_response_grpc = if full_measurement {
                Some(unix_nanos_now())
            } else {
                None
            };

            let java_routing_service_sent_response_grpc = Some(response.get_ref().java_routing_service_sent_response_grpc.unwrap_or_default());

            let mut internal_res = InternalRoutingResponse::from(response.into_inner());
            internal_res.adapter_received_request_agent = adapter_received_request_agent;
            internal_res.adapter_sent_request_grpc = rust_adapter_sent_request_grpc;
            internal_res.adapter_received_response_grpc = adapter_received_response_grpc;
            internal_res.java_routing_service_sent_response_grpc = java_routing_service_sent_response_grpc;

            let adapter_sent_response_agent = if full_measurement {
                Some(unix_nanos_now())
            } else {
                None
            };
            internal_res.adapter_sent_response_agent = adapter_sent_response_agent;

            let _ = internal_req.response_tx.send(internal_res);
        });
    }

    fn on_shutdown(&mut self) {

        for client in &mut self.clients {
            let mut c = client.clone();
            let handle = tokio::spawn(async move {
                c.shutdown(())
                    .await
                    .expect("Error while shutting down routing service");
            });
            self.shutdown_handles.lock().unwrap().push(handle);
        }
    }
}

impl RoutingServiceAdapter {
    fn new(
        clients: Vec<RoutingServiceClient<tonic::transport::Channel>>,
        shutdown_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    ) -> Self {
        Self {
            clients: RingIter::new(clients),
            shutdown_handles,
        }
    }
}
