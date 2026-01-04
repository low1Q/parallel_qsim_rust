use crate::external_services::event_sharing::{InternalEventSharingRequest, InternalEventSharingRequestPayload};
use crate::generated::event_sharing::EventType; // WICHTIG: Das generierte Enum importieren
use crate::simulation::events::{EventsPublisher, OnEventFnBuilder, LinkEnterEvent, LinkLeaveEvent, EventTrait, VehicleEntersTrafficEvent, VehicleLeavesTrafficEvent};
use std::sync::Arc;
use tokio::sync::mpsc::{error::TrySendError, Sender};

pub fn make_event_sharing_subscriber(
    sender: Arc<Sender<InternalEventSharingRequest>>,
) -> Box<OnEventFnBuilder> {
    Box::new(move |events: &mut EventsPublisher| {
        let sender = sender.clone();
        events.on_any(move |ev: &dyn EventTrait| {

            // 1. LinkEnterEvent
            if let Some(e) = ev.as_any().downcast_ref::<LinkEnterEvent>() {
                let payload = InternalEventSharingRequestPayload {
                    event_type: EventType::EnteredLink,
                    // Wir nehmen den u64-Index der Simulation und casten ihn zu u32 für Protobuf
                    link_id: e.link.internal() as u32,
                    vehicle_id: e.vehicle.internal() as u32,
                    now: e.time,
                    driver_id: None,
                    network_mode: None,
                    relative_position_on_link: None,
                };
                send_event(&sender, InternalEventSharingRequest { payload }, "LinkEnterEvent");
                return;
            }

            // 2. LinkLeaveEvent
            else if let Some(e) = ev.as_any().downcast_ref::<LinkLeaveEvent>() {
                let payload = InternalEventSharingRequestPayload {
                    event_type: EventType::LeftLink, // Geändert zu Enum
                    link_id: e.link.internal() as u32,
                    vehicle_id: e.vehicle.internal() as u32,
                    now: e.time,
                    driver_id: None,
                    network_mode: None,
                    relative_position_on_link: None,
                };

                send_event(&sender, InternalEventSharingRequest { payload }, "LinkLeaveEvent");
                return;
            }

            // 3. VehicleEntersTrafficEvent
            else if let Some(e) = ev.as_any().downcast_ref::<VehicleEntersTrafficEvent>() {
                let payload = InternalEventSharingRequestPayload {
                    event_type: EventType::VehicleEntersTraffic,
                    now: e.time,
                    // Nutze .internal(), nicht .external()
                    link_id: e.link.internal() as u32,
                    vehicle_id: e.vehicle.internal() as u32,
                    // driver_id ist ebenfalls eine Id<Person>, also auch .internal()
                    driver_id: Some(e.driver.internal() as u32),
                    network_mode: Some(e.mode.external().to_string()), // Mode bleibt String
                    relative_position_on_link: Some(e.relative_position_on_link),
                };

                send_event(&sender, InternalEventSharingRequest { payload }, "VehicleEntersTrafficEvent");
                return;
            }

            // 4. VehicleLeavesTrafficEvent
            else if let Some(e) = ev.as_any().downcast_ref::<VehicleLeavesTrafficEvent>() {
                let payload = InternalEventSharingRequestPayload {
                    event_type: EventType::VehicleLeavesTraffic, // Geändert zu Enum
                    now: e.time,
                    link_id: e.link.internal() as u32,
                    vehicle_id: e.vehicle.internal() as u32,
                    driver_id: Some(e.driver.internal() as u32),
                    network_mode: Some(e.mode.external().to_string()),
                    relative_position_on_link: Some(e.relative_position_on_link),
                };

                send_event(&sender, InternalEventSharingRequest { payload }, "VehicleLeavesTrafficEvent");
                return;
            }
        });
    })
}

/// Hilfsfunktion zur Vermeidung von Code-Duplizierung beim Senden
fn send_event(sender: &Sender<InternalEventSharingRequest>, req: InternalEventSharingRequest, name: &str) {
    match sender.try_send(req) {
        Ok(_) => {}
        Err(TrySendError::Full(_)) => {
            eprintln!("EventSharing queue full — dropped {}", name);
        }
        Err(TrySendError::Closed(_)) => {
            eprintln!("EventSharing sender closed — dropped {}", name);
        }
    }
}