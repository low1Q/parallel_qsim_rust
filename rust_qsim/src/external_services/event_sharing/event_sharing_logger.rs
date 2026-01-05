use crate::external_services::event_sharing::{
    InternalEventSharingRequest, InternalEventSharingRequestPayload,
};
use crate::simulation::events::{
    EventTrait, EventsPublisher, LinkEnterEvent, LinkLeaveEvent, OnEventFnBuilder,
    VehicleEntersTrafficEvent, VehicleLeavesTrafficEvent,
};
use std::sync::Arc;
use tokio::sync::mpsc::{error::TrySendError, Sender};

// Erzeugt einen `Box<OnEventFnBuilder>` der beim Ausführen zwei Handler einträgt:
// - für `LinkEnterEvent`
// - für `LinkLeaveEvent`
//
// Implementierung: wir registrieren einen `on_any`-Handler und machen im Closure das Downcast
// auf die konkreten Event-Typen. So erfüllt die Closure die geforderte Signatur
// `Fn(&dyn EventTrait)` und du kannst trotzdem an `ev.link`/`ev.vehicle`/`ev.time`.
pub fn make_event_sharing_subscriber(
    sender: Arc<Sender<InternalEventSharingRequest>>,
) -> Box<OnEventFnBuilder> {
    Box::new(move |events: &mut EventsPublisher| {
        let sender = sender.clone();
        events.on_any(move |ev: &dyn EventTrait| {
            // LinkEnterEvent
            if let Some(e) = ev.as_any().downcast_ref::<LinkEnterEvent>() {
                let payload = InternalEventSharingRequestPayload {
                    event_type: LinkEnterEvent::TYPE.to_string(),
                    link_id: e.link.external().to_string(),
                    vehicle_id: e.vehicle.external().to_string(),
                    now: e.time,
                    driver_id: None,
                    network_mode: None,
                    relative_position_on_link: None,
                };
                send_event(
                    &sender,
                    InternalEventSharingRequest { payload },
                    "LinkEnterEvent",
                );
                return;
            }
            // LinkLeaveEvent
            else if let Some(e) = ev.as_any().downcast_ref::<LinkLeaveEvent>() {
                let payload = InternalEventSharingRequestPayload {
                    event_type: LinkLeaveEvent::TYPE.to_string(),
                    link_id: e.link.external().to_string(),
                    vehicle_id: e.vehicle.external().to_string(),
                    now: e.time,
                    driver_id: None,
                    network_mode: None,
                    relative_position_on_link: None,
                };

                send_event(
                    &sender,
                    InternalEventSharingRequest { payload },
                    "LinkLeaveEvent",
                );
                return;
            }
            // VehicleEntersTrafficEvent
            else if let Some(e) = ev.as_any().downcast_ref::<VehicleEntersTrafficEvent>() {
                let payload = InternalEventSharingRequestPayload {
                    event_type: VehicleEntersTrafficEvent::TYPE.to_string(),
                    now: e.time,
                    link_id: e.link.external().to_string(),
                    vehicle_id: e.vehicle.external().to_string(),
                    driver_id: Some(e.vehicle.external().to_string()),
                    network_mode: Some(e.mode.external().to_string()),
                    relative_position_on_link: Some(e.relative_position_on_link),
                };

                send_event(
                    &sender,
                    InternalEventSharingRequest { payload },
                    "VehicleEntersTrafficEvent",
                );
                return;
            }
            // VehicleLeavesTrafficEvent
            else if let Some(e) = ev.as_any().downcast_ref::<VehicleLeavesTrafficEvent>() {
                let payload = InternalEventSharingRequestPayload {
                    event_type: VehicleLeavesTrafficEvent::TYPE.to_string(),
                    now: e.time,
                    link_id: e.link.external().to_string(),
                    vehicle_id: e.vehicle.external().to_string(),
                    driver_id: Some(e.vehicle.external().to_string()),
                    network_mode: Some(e.mode.external().to_string()),
                    relative_position_on_link: Some(e.relative_position_on_link),
                };

                send_event(
                    &sender,
                    InternalEventSharingRequest { payload },
                    "VehicleLeavesTrafficEvent",
                );
                return;
            }
        });
    })
}

/// Hilfsfunktion zur Vermeidung von Code-Duplizierung beim Senden
fn send_event(
    sender: &Sender<InternalEventSharingRequest>,
    req: InternalEventSharingRequest,
    name: &str,
) {
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
