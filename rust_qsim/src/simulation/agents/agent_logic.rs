use crate::external_services::routing::{
    InternalRoutingRequest, InternalRoutingRequestPayloadBuilder, InternalRoutingResponse,
};
use crate::external_services::ExternalServiceType;
use crate::simulation::agents::{
    AgentEvent, EnvironmentalEventObserver, SimulationAgentLogic, SimulationAgentState,
};
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::id::Id;
use crate::simulation::network::Link;
use crate::simulation::population::trip_structure_utils::{
    find_trip_starting_at_activity_default, identify_main_mode,
};
use crate::simulation::population::{
    InternalActivity, InternalLeg, InternalPerson, InternalPlan, InternalPlanElement, InternalRoute,
};
use crate::simulation::profiling::car_routing::{RoutingRequestCsvRow, RoutingRequestCsvWriter};
use crate::simulation::profiling::router_blocking_wait::{
    RoutingBlockingWaitCsvRow, RoutingBlockingWaitCsvWriter,
};
use crate::simulation::time_queue::{EndTime, Identifiable};
use once_cell::sync::Lazy;
use std::fmt::{Debug, Formatter};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::Sender;
use tokio::sync::oneshot::Receiver;
use tracing::{trace, warn};

static ROUTING_REQUEST_CSV: Lazy<StdMutex<Option<RoutingRequestCsvWriter>>> = Lazy::new(|| {
    let path = std::env::var("ROUTING_RUST_CSV")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/output/routing/routing_requests_rust.csv"));

    let (writer, _guard) = RoutingRequestCsvWriter::new(&path);

    // Für die erste einfache Version behalten wir den Writer global.
    // Der WriterGuard wird hier absichtlich geleakt, damit flush on drop erst am Prozessende passiert.
    std::mem::forget(_guard);

    StdMutex::new(Some(writer))
});

static ROUTING_BLOCKING_WAIT_CSV: Lazy<StdMutex<Option<RoutingBlockingWaitCsvWriter>>> =
    Lazy::new(|| {
        let path = std::env::var("ROUTING_BLOCKING_WAIT_CSV")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/output/routing/routing_blocking_wait_rust.csv"));

        let (writer, guard) = RoutingBlockingWaitCsvWriter::new(&path);
        std::mem::forget(guard);
        StdMutex::new(Some(writer))
    });

static TOTAL_ROUTING_BLOCKING_WAIT_NS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, PartialEq, Clone)]
pub struct PlanBasedSimulationLogic {
    pub(super) basic_agent_delegate: InternalPerson,
    pub(super) curr_plan_element: usize,
    pub(super) curr_route_element: usize,
}

pub struct AdaptivePlanBasedSimulationLogic {
    delegate: PlanBasedSimulationLogic,
    route_receiver: Option<Receiver<InternalRoutingResponse>>,

    // Systemübergreifende Startzeit des Route-Calls
    route_call_start: Option<i64>,

    agent_sent_request_adapter: Option<i64>,
    // Reale Systemzeiten des Prozesses für Profiling
    route_call_started_instant: Option<Instant>,
    agent_received_response_adapter_instant: Option<Instant>,
    agent_replaced_route_instant: Option<Instant>,
    // Absolute reale Zeiten für CSV/Korrelation
    agent_received_response_adapter: Option<i64>,

    agent_replaced_route: Option<i64>,

    route_replaced_at_sim_time: Option<u32>,

    last_route_end_to_end_sim_time: Option<u32>,
    current_route_request_departure_time: Option<u32>,
    current_route_request_now: Option<u32>,

    current_route_request_mode: Option<String>,
    last_route_blocking_wait_ns: Option<u128>,
}

impl Debug for AdaptivePlanBasedSimulationLogic {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?}, RouteReceiver {:?}",
            self.delegate,
            self.route_receiver.is_some()
        )
    }
}

impl Identifiable<InternalPerson> for PlanBasedSimulationLogic {
    fn id(&self) -> &Id<InternalPerson> {
        self.basic_agent_delegate.id()
    }
}

impl EnvironmentalEventObserver for PlanBasedSimulationLogic {
    fn notify_event(&mut self, event: &mut AgentEvent, _now: u32) {
        match event {
            AgentEvent::TeleportationStarted { .. } => {
                self.set_curr_route_element_to_last();
            }
            AgentEvent::LeftLink { .. } => {
                self.curr_route_element += 1;
            }
            _ => {}
        }
    }
}

impl PlanBasedSimulationLogic {
    /// This method advances the pointer to the last element of the route. We need this in case of
    /// teleported legs. Advancing the route pointer to the last element directly ensures that teleporting
    /// the vehicle is independent of whether the leg has a Generic-Teleportation route or a network
    /// route.
    fn set_curr_route_element_to_last(&mut self) {
        let route = self.curr_leg().route.as_ref().unwrap();
        if route.as_network().is_some() {
            let last = route.as_network().unwrap().route().len() - 1;
            self.curr_route_element = last;
        } else {
            self.curr_route_element = 1;
        }
    }

    pub fn new(basic_agent_delegate: InternalPerson) -> Self {
        Self {
            basic_agent_delegate,
            curr_plan_element: 0,
            curr_route_element: 0,
        }
    }
}

impl SimulationAgentLogic for PlanBasedSimulationLogic {
    fn curr_act(&self) -> &InternalActivity {
        self.basic_agent_delegate
            .plan_element_at(self.curr_plan_element)
            .and_then(|p| p.as_activity())
            .unwrap()
    }

    fn next_act(&self) -> &InternalActivity {
        let add = if self.curr_plan_element % 2 == 0 {
            // If the current plan element is an activity, the next one should be a leg
            2
        } else {
            // If the current plan element is a leg, the next one should be an activity
            1
        };
        self.basic_agent_delegate
            .plan_element_at(self.curr_plan_element + add)
            .and_then(|p| p.as_activity())
            .unwrap()
    }

    fn curr_leg(&self) -> &InternalLeg {
        self.basic_agent_delegate
            .plan_element_at(self.curr_plan_element)
            .and_then(|p| p.as_leg())
            .unwrap()
    }

    fn next_leg(&self) -> Option<&InternalLeg> {
        let add = if self.curr_plan_element % 2 == 0 {
            // If the current plan element is an activity, the next one should be a leg
            1
        } else {
            // If the current plan element is a leg, the next one should be an activity
            2
        };
        self.basic_agent_delegate
            .plan_element_at(self.curr_plan_element + add)
            .and_then(|p| p.as_leg())
    }

    fn advance_plan(&mut self) {
        self.curr_plan_element += 1;
        self.curr_route_element = 0;
        assert!(
            self.curr_plan_element < self.basic_agent_delegate.total_elements(),
            "Cannot advance plan of agents {:?} beyond its last element.",
            self.basic_agent_delegate.id()
        );
    }

    fn state(&self) -> SimulationAgentState {
        match self.curr_plan_element % 2 {
            0 => SimulationAgentState::ACTIVITY,
            1 => SimulationAgentState::LEG,
            _ => unreachable!(),
        }
    }

    fn is_wanting_to_arrive_on_current_link(&self) -> bool {
        self.peek_next_link_id().is_none()
    }

    fn curr_link_id(&self) -> Option<&Id<Link>> {
        if self.state() != SimulationAgentState::LEG {
            return None;
        }

        match self.curr_leg().route.as_ref().unwrap() {
            InternalRoute::Generic(g) => match self.curr_route_element {
                0 => Some(g.start_link()),
                1 => Some(g.end_link()),
                _ => panic!(
                    "A generic route only has two elements. Current plan element {:?}, Current route element {:?}, Current agent {:?}", self.curr_plan_element, self.curr_route_element, self.basic_agent_delegate.id()
                ),
            },
            InternalRoute::Network(n) => n.route_element_at(self.curr_route_element),
            InternalRoute::Pt(p) => match self.curr_route_element {
                0 => Some(p.start_link()),
                1 => Some(p.end_link()),
                _ => panic!(
                    "A generic route only has two elements. Current plan element {:?}, Current route element {:?}, Current agent {:?}", self.curr_plan_element, self.curr_route_element, self.basic_agent_delegate.id()
                ),
            },
        }
    }

    fn peek_next_link_id(&self) -> Option<&Id<Link>> {
        let next_i = self.curr_route_element + 1;
        self.curr_leg()
            .route
            .as_ref()
            .unwrap()
            .as_network()
            .unwrap()
            .route_element_at(next_i)
    }

    fn wakeup_time(&self, now: u32) -> u32 {
        match self
            .basic_agent_delegate
            .plan_element_at(self.curr_plan_element)
            .unwrap()
        {
            InternalPlanElement::Activity(a) => a.cmp_end_time(now),
            InternalPlanElement::Leg(_) => panic!("Cannot wake up on a leg!"),
        }
    }
}

impl EndTime for PlanBasedSimulationLogic {
    fn end_time(&self, now: u32) -> u32 {
        match self
            .basic_agent_delegate
            .plan_element_at(self.curr_plan_element)
            .unwrap()
        {
            InternalPlanElement::Activity(a) => a.cmp_end_time(now),
            InternalPlanElement::Leg(l) => l.travel_time() + now,
        }
    }
}

impl SimulationAgentLogic for AdaptivePlanBasedSimulationLogic {
    fn curr_act(&self) -> &InternalActivity {
        self.delegate.curr_act()
    }

    fn next_act(&self) -> &InternalActivity {
        self.delegate.next_act()
    }

    fn curr_leg(&self) -> &InternalLeg {
        self.delegate.curr_leg()
    }

    fn next_leg(&self) -> Option<&InternalLeg> {
        self.delegate.next_leg()
    }

    fn advance_plan(&mut self) {
        self.delegate.advance_plan();
    }

    fn state(&self) -> SimulationAgentState {
        self.delegate.state()
    }

    fn is_wanting_to_arrive_on_current_link(&self) -> bool {
        self.delegate.is_wanting_to_arrive_on_current_link()
    }

    fn curr_link_id(&self) -> Option<&Id<Link>> {
        self.delegate.curr_link_id()
    }

    fn peek_next_link_id(&self) -> Option<&Id<Link>> {
        self.delegate.peek_next_link_id()
    }

    fn wakeup_time(&self, now: u32) -> u32 {
        let mut end = self.delegate.curr_act().cmp_end_time(now);
        if let Some(l) = self.delegate.next_leg() {
            let horizon: Option<u32> = l
                .attributes
                .get(crate::simulation::population::PREPLANNING_HORIZON);
            if let Some(h) = horizon {
                end -= h;
            }
        }
        end
    }
}

impl EndTime for AdaptivePlanBasedSimulationLogic {
    fn end_time(&self, now: u32) -> u32 {
        self.delegate.end_time(now)
    }
}

impl Identifiable<InternalPerson> for AdaptivePlanBasedSimulationLogic {
    fn id(&self) -> &Id<InternalPerson> {
        self.delegate.id()
    }
}

impl EnvironmentalEventObserver for AdaptivePlanBasedSimulationLogic {
    fn notify_event(&mut self, mut event: &mut AgentEvent, now: u32) {
        match &mut event {
            AgentEvent::WokeUp(w) => {
                self.react_to_woke_up(w.comp_env, w.end_time, now);
            }
            AgentEvent::ActivityFinished() => self.replace_route(now),
            _ => {}
        }
        self.delegate.notify_event(event, now);
    }
}

impl AdaptivePlanBasedSimulationLogic {
    pub fn new(person: InternalPerson) -> Self {
        Self {
            delegate: PlanBasedSimulationLogic::new(person),
            route_receiver: None,
            route_call_start: None,
            agent_sent_request_adapter: None,
            route_call_started_instant: None,
            agent_received_response_adapter_instant: None,
            agent_replaced_route_instant: None,
            agent_received_response_adapter: None,
            agent_replaced_route: None,
            route_replaced_at_sim_time: None,
            last_route_end_to_end_sim_time: None,
            current_route_request_now: None,
            current_route_request_departure_time: None,
            current_route_request_mode: None,
            last_route_blocking_wait_ns: None,
        }
    }

    pub fn route_call_start_realtime(&self) -> Option<i64> {
        self.route_call_start
    }
    pub fn route_call_started_at(&self) -> &Option<Instant> {
        &self.route_call_started_instant
    }
    pub fn route_received_at(&self) -> &Option<Instant> {
        &self.agent_received_response_adapter_instant
    }
    pub fn route_replaced_at(&self) -> &Option<Instant> {
        &self.agent_replaced_route_instant
    }
    pub fn route_replaced_at_sim_time(&self) -> &Option<u32> {
        &self.route_replaced_at_sim_time
    }
    pub fn last_route_end_to_end_sim_time(&self) -> &Option<u32> {
        &self.last_route_end_to_end_sim_time
    }
    pub fn total_routing_blocking_wait_ns() -> u64 {
        TOTAL_ROUTING_BLOCKING_WAIT_NS.load(Ordering::Relaxed)
    }

    fn react_to_woke_up(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        departure_time: u32,
        now: u32,
    ) {
        if self.route_receiver.is_some() {
            // If we already have a route request in progress, we do not call the router again.
            return;
        }

//         if (departure_time - now <= 300){
//             return;
//         }

        let preplan = self
            .next_leg()
            .and_then(|l| {
                l.attributes
                    .get::<u32>(crate::simulation::population::PREPLANNING_HORIZON)
            })
            .is_some();

        if !preplan {
            // No reason to call the router if we are not preplanning.
            return;
        }

        self.call_router(comp_env, departure_time, now);
    }

    #[tracing::instrument(level = "trace", skip(comp_env), fields(uuid = tracing::field::Empty, person_id = self.delegate.id().external(), mode = tracing::field::Empty
    ))]
    fn call_router(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        departure_time: u32,
        now: u32,
    ) {
        let route_call_started_at = Instant::now();
        let route_call_start_realtime = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("SystemTime before UNIX EPOCH!")
            .as_nanos() as i64;

        let (send, recv) = tokio::sync::oneshot::channel();

        let trip = find_trip_starting_at_activity_default(
            &self
                .delegate
                .basic_agent_delegate
                .selected_plan()
                .unwrap()
                .elements,
            self.delegate.curr_plan_element,
        )
        .unwrap_or_else(|| {
            panic!(
                "No trip found for agent {:?} at plan element {:?} at time {:?}",
                self.delegate.id(),
                self.delegate.curr_plan_element,
                now
            )
        });

        let origin = trip.origin;
        let destination = trip.destination;

        let mode = identify_main_mode(trip.legs).unwrap_or_else(|| {
            panic!(
                "Could not identify main mode for trip starting at activity {:?} in agent {:?}",
                origin,
                self.delegate.id()
            )
        });

        self.current_route_request_departure_time = Some(departure_time);
        self.current_route_request_now = Some(now);
        self.current_route_request_mode = Some(mode.clone());

        let payload = InternalRoutingRequestPayloadBuilder::default()
            .person_id(self.delegate.id().external().to_string())
            .from_link(origin.link_id.external().to_string())
            .from_x(origin.x)
            .from_y(origin.y)
            .to_link(destination.link_id.external().to_string())
            .to_x(destination.x)
            .to_y(destination.y)
            .mode(mode.clone())
            .departure_time(departure_time)
            .now(now)
            .route_call_start_realtime(route_call_start_realtime)
            .adapter_sent_request_grpc(None)
            .build()
            .unwrap();

        trace!(uuid = payload.uuid.as_u128(), mode = mode.as_str());

        let request = InternalRoutingRequest {
            payload,
            response_tx: send,
        };
        self.agent_sent_request_adapter = None;
        let agent_sent_request_adapter = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("SystemTime before UNIX EPOCH!")
            .as_nanos() as i64;

        self.agent_sent_request_adapter = Some(agent_sent_request_adapter);

        comp_env
            .get_service::<Sender<InternalRoutingRequest>>(ExternalServiceType::Routing(mode.clone()))
            .unwrap_or_else(|| panic!("There is not service registered for routing of mode {} and agent id {}. Please make sure that you have started a corresponding thread. Next leg {:?}", mode, self.id(), self.next_leg()))
            .blocking_send(request)
            .expect("InternalRoutingRequest channel closed unexpectedly");

        self.route_receiver = Some(recv);

        self.route_call_started_instant = Some(route_call_started_at);
        self.route_call_start = Some(route_call_start_realtime);

        self.agent_received_response_adapter_instant = None;
        self.agent_replaced_route_instant = None;
        self.agent_received_response_adapter = None;
        self.agent_replaced_route = None;

        self.route_replaced_at_sim_time = None;

        self.last_route_end_to_end_sim_time = None;

        self.last_route_blocking_wait_ns = None;
    }

    #[tracing::instrument(level = "trace", fields(person_id = self.delegate.id().external()))]
    fn replace_route(&mut self, _now: u32) {
        if self.route_receiver.is_none() {
            // No route request in progress, nothing to replace.
            return;
        }

        let response = self.blocking_recv(_now);

        let route_received_at = Instant::now();
        let route_received_realtime = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("SystemTime before UNIX EPOCH!")
            .as_nanos() as i64;

        self.agent_received_response_adapter_instant = Some(route_received_at);
        self.agent_received_response_adapter = Some(route_received_realtime);

        trace!(uuid = response.request_id.as_u128());

        let response_for_csv = response.clone();
        self.replace_next_trip(response, _now);
        // Aktualisierte Route wurde eingefügt zum Zeitpunkt now.
        let route_replaced_at = Instant::now();
        let route_replaced_realtime = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("SystemTime before UNIX EPOCH!")
            .as_nanos() as i64;

        self.agent_replaced_route_instant = Some(route_replaced_at);
        self.agent_replaced_route = Some(route_replaced_realtime);
        self.route_replaced_at_sim_time = Some(_now);

        if let Some(start_sim_time) = self.current_route_request_now {
            self.last_route_end_to_end_sim_time = Some(_now.saturating_sub(start_sim_time));
        }
        let now = self.current_route_request_now.unwrap_or_default();
        let departure_time = self
            .current_route_request_departure_time
            .unwrap_or_default();
        let mode = self
            .current_route_request_mode
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        if crate::simulation::profiling::flags::performance_logging_enabled() {
            self.write_routing_csv_row(&response_for_csv, now, departure_time, &mode);
        } else if crate::simulation::profiling::flags::only_route_blocking_wait_enabled() {
            self.write_routing_blocking_wait_csv_row(&response_for_csv, now, departure_time, &mode);
        }

        self.current_route_request_departure_time = None;
        self.current_route_request_mode = None;
    }

    #[tracing::instrument(level = "trace", fields(person_id = self.delegate.id().external()))]
    fn blocking_recv(&mut self, _now: u32) -> InternalRoutingResponse {
        let receiver = self.route_receiver.take().unwrap();

        // Blockierungszeit für das Warten auf die Antwort des Routers messen
        let wait_start = if crate::simulation::profiling::flags::any_measurement_enabled() {
            Some(Instant::now())
        } else {
            None
        };

        let response = receiver
            .blocking_recv()
            .expect("InternalRoutingRequest channel closed unexpectedly");

        // Blockierungszeit für das Warten auf die Antwort des Routers messen
        if let Some(wait_start) = wait_start {
            let wait_ns = wait_start.elapsed().as_nanos();
            self.last_route_blocking_wait_ns = Some(wait_ns);
            // Totale Blockierungszeit für diesen Rank aufsummieren
            TOTAL_ROUTING_BLOCKING_WAIT_NS.fetch_add(wait_ns as u64, Ordering::Relaxed);
        } else {
            self.last_route_blocking_wait_ns = None;
        }

        trace!(uuid = response.request_id.as_u128());

        response
    }

    /// Replaces the next trip in the plan with the legs and activities from the given InternalRoutingResponse.
    #[tracing::instrument(level = "trace", skip(response), fields(person_id = self.delegate.id().external()
    ))]
    fn replace_next_trip(&mut self, response: InternalRoutingResponse, _now: u32) {
        trace!(uuid = response.request_id.as_u128());

        if response.elements.is_empty() {
            // If the response is empty, we do not replace anything.
            return;
        }

        let plan = self.delegate.basic_agent_delegate.selected_plan_mut();
        let start_index = self.delegate.curr_plan_element;

        let trip = find_trip_starting_at_activity_default(&plan.elements, start_index)
            .expect("No trip found starting at the current plan element");

        let origin_ptr = trip.origin as *const _;
        let dest_ptr = trip.destination as *const _;

        let origin_idx = Self::get_index(plan, origin_ptr);
        let dest_idx = Self::get_index(plan, dest_ptr);

        // Replace the trip elements (legs and intermediate activities) with the new response
        plan.elements
            .splice(origin_idx + 1..dest_idx, response.elements);
    }

    fn get_index(plan: &mut InternalPlan, origin_ptr: *const InternalActivity) -> usize {
        plan.elements
            .iter()
            .position(|e| e.as_activity().map(|a| a as *const _) == Some(origin_ptr))
            .expect("Didn't find the activity in the plan")
    }

    fn write_routing_csv_row(
        &self,
        response: &InternalRoutingResponse,
        now: u32,
        departure_time: u32,
        mode: &str,
    ) {
        if !crate::simulation::profiling::flags::performance_logging_enabled() {
            return;
        }

        let row = RoutingRequestCsvRow {
            rank: std::env::var("RUST_QSIM_RANK").unwrap_or_else(|_| "unknown".to_string()),
            request_id: response.request_id.to_string(),
            person_id: self.delegate.id().external().to_string(),
            mode: mode.to_string(),
            now,
            departure_time,
            route_call_start: self.route_call_start.unwrap_or_default(),
            agent_sent_request_adapter: self.agent_sent_request_adapter.unwrap_or_default(),
            adapter_received_request_agent: response
                .adapter_received_request_agent
                .unwrap_or_default(),
            adapter_sent_request_grpc: response.adapter_sent_request_grpc.unwrap_or_default(),
            java_routing_service_sent_response_grpc: response
                .java_routing_service_sent_response_grpc
                .unwrap_or_default(),
            adapter_received_response_grpc: response
                .adapter_received_response_grpc
                .unwrap_or_default(),
            adapter_sent_response_agent: response.adapter_sent_response_agent.unwrap_or_default(),
            agent_received_response_adapter: self
                .agent_received_response_adapter
                .unwrap_or_default(),
            agent_replaced_route: self.agent_replaced_route.unwrap_or_default(),
            route_end_to_end_sim_time: self.last_route_end_to_end_sim_time.unwrap_or_default(),
            route_blocking_wait_ns: self.last_route_blocking_wait_ns.unwrap_or_default(),
        };

        let mut guard = ROUTING_REQUEST_CSV.lock().unwrap();
        if let Some(writer) = guard.as_mut() {
            writer.write_row(&row);
        }
    }

    fn write_routing_blocking_wait_csv_row(
        &self,
        response: &InternalRoutingResponse,
        now: u32,
        departure_time: u32,
        mode: &str,
    ) {
        let row = RoutingBlockingWaitCsvRow {
            rank: std::env::var("RUST_QSIM_RANK").unwrap_or_else(|_| "unknown".to_string()),
            request_id: response.request_id.to_string(),
            person_id: self.delegate.id().external().to_string(),
            mode: mode.to_string(),
            now,
            departure_time,
            route_blocking_wait_ns: self.last_route_blocking_wait_ns.unwrap_or_default(),
        };

        let mut guard = ROUTING_BLOCKING_WAIT_CSV.lock().unwrap();
        if let Some(writer) = guard.as_mut() {
            writer.write_row(&row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_services::routing::InternalRoutingResponse;
    use crate::simulation::id::Id;
    use crate::simulation::population::{
        InternalActivity, InternalLeg, InternalPlan, InternalRoute,
    };
    use uuid::Uuid;

    fn make_activity(act_type: &str, link: &str) -> InternalActivity {
        InternalActivity {
            act_type: Id::create(act_type),
            link_id: Id::create(link),
            x: 0.0,
            y: 0.0,
            start_time: None,
            end_time: None,
            max_dur: None,
        }
    }

    fn make_leg(mode: &str) -> InternalLeg {
        InternalLeg {
            mode: Id::create(mode),
            routing_mode: Some(Id::create(mode)),
            dep_time: None,
            trav_time: Some(10),
            route: Some(InternalRoute::Generic(
                crate::simulation::population::InternalGenericRoute::new(
                    Id::create("l1"),
                    Id::create("l2"),
                    Some(10),
                    Some(100.0),
                    None,
                ),
            )),
            attributes: Default::default(),
        }
    }

    #[test]
    fn test_replace_next_trip_basic() {
        // Plan: home --leg1--> work --leg2--> shop
        let mut plan = InternalPlan::default();
        plan.add_act(make_activity("home", "1"));
        plan.add_leg(make_leg("car"));
        plan.add_act(make_activity("work", "2"));
        plan.add_leg(make_leg("walk"));
        plan.add_act(make_activity("home", "3"));
        let person = InternalPerson::new(Id::create("p1"), plan);
        let mut logic = AdaptivePlanBasedSimulationLogic::new(person);

        // Replace the first trip (home->work)
        let response = InternalRoutingResponse {
            elements: vec![InternalPlanElement::Leg(make_leg("bike"))],
            request_id: Uuid::now_v7(),
            ..Default::default()
        };

        logic.replace_next_trip(response.clone(), 0);
        let elements = &logic
            .delegate
            .basic_agent_delegate
            .selected_plan()
            .unwrap()
            .elements;

        assert_eq!(
            elements[0].as_activity().unwrap().act_type.external(),
            "home"
        );
        assert_eq!(elements[1].as_leg().unwrap().mode.external(), "bike");
        assert_eq!(
            elements[2].as_activity().unwrap().act_type.external(),
            "work"
        );
        assert_eq!(elements[3].as_leg().unwrap().mode.external(), "walk");
        assert_eq!(
            elements[4].as_activity().unwrap().act_type.external(),
            "home"
        );
    }
}
