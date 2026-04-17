use crate::external_services::event_sharing::{
    InternalEventSharingRequest, InternalEventSharingRequestPayload,
};
use crate::simulation::events::{
    EventTrait, EventsPublisher, LinkEnterEvent, LinkLeaveEvent, OnEventFnBuilder
    ,
};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::Sender;
use std::cell::Cell;



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
        let seq_in_partition = Cell::new(0u64);
        events.on_any(move |ev: &dyn EventTrait| {
            // LinkEnterEvent
            if let Some(e) = ev.as_any().downcast_ref::<LinkEnterEvent>() {
                let next_seq = seq_in_partition.get() + 1;
                seq_in_partition.set(next_seq);
                let event_detected_at_realtime = unix_nanos_now();
                let payload = InternalEventSharingRequestPayload {
                    event_type: LinkEnterEvent::TYPE.to_string(),
                    link_id: e.link.external().to_string(),
                    vehicle_id: e.vehicle.external().to_string(),
                    now: e.time,
                    driver_id: None,
                    network_mode: None,
                    relative_position_on_link: None,
                    event_detected_at_realtime: Some(event_detected_at_realtime),
                    logger_send_started_realtime: None,
                    adapter_arrived_at_realtime: None,
                    seq_in_partition: next_seq,
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
                let next_seq = seq_in_partition.get() + 1;
                seq_in_partition.set(next_seq);
                let event_detected_at_realtime = unix_nanos_now();
                let payload = InternalEventSharingRequestPayload {
                    event_type: LinkLeaveEvent::TYPE.to_string(),
                    link_id: e.link.external().to_string(),
                    vehicle_id: e.vehicle.external().to_string(),
                    now: e.time,
                    driver_id: None,
                    network_mode: None,
                    relative_position_on_link: None,
                    event_detected_at_realtime: Some(event_detected_at_realtime),
                    logger_send_started_realtime: None,
                    adapter_arrived_at_realtime: None,
                    seq_in_partition: next_seq,
                };

                send_event(
                    &sender,
                    InternalEventSharingRequest { payload },
                    "LinkLeaveEvent",
                );
                return;
            }
            // // VehicleEntersTrafficEvent
            // else if let Some(e) = ev.as_any().downcast_ref::<VehicleEntersTrafficEvent>() {
            //     let event_detected_at_realtime = unix_nanos_now();
            //     let payload = InternalEventSharingRequestPayload {
            //         event_type: VehicleEntersTrafficEvent::TYPE.to_string(),
            //         now: e.time,
            //         link_id: e.link.external().to_string(),
            //         vehicle_id: e.vehicle.external().to_string(),
            //         driver_id: Some(e.vehicle.external().to_string()),
            //         network_mode: Some(e.mode.external().to_string()),
            //         relative_position_on_link: Some(e.relative_position_on_link),
            //         event_detected_at_realtime: Some(event_detected_at_realtime),
            //         logger_send_started_realtime: None,
            //         adapter_arrived_at_realtime: None,
            //     };
            //
            //     send_event(
            //         &sender,
            //         InternalEventSharingRequest { payload },
            //         "VehicleEntersTrafficEvent",
            //     );
            //     return;
            // }
            // // VehicleLeavesTrafficEvent
            // else if let Some(e) = ev.as_any().downcast_ref::<VehicleLeavesTrafficEvent>() {
            //     let event_detected_at_realtime = unix_nanos_now();
            //     let payload = InternalEventSharingRequestPayload {
            //         event_type: VehicleLeavesTrafficEvent::TYPE.to_string(),
            //         now: e.time,
            //         link_id: e.link.external().to_string(),
            //         vehicle_id: e.vehicle.external().to_string(),
            //         driver_id: Some(e.vehicle.external().to_string()),
            //         network_mode: Some(e.mode.external().to_string()),
            //         relative_position_on_link: Some(e.relative_position_on_link),
            //         event_detected_at_realtime: Some(event_detected_at_realtime),
            //         logger_send_started_realtime: None,
            //         adapter_arrived_at_realtime: None,
            //     };
            //
            //     send_event(
            //         &sender,
            //         InternalEventSharingRequest { payload },
            //         "VehicleLeavesTrafficEvent",
            //     );
            //     return;
            // }
        });
    })
}

/// Hilfsfunktion zur Vermeidung von Code-Duplizierung beim Senden
fn send_event(
    sender: &Sender<InternalEventSharingRequest>,
    mut req: InternalEventSharingRequest,
    name: &str,
) {
    let logger_send_started_realtime = unix_nanos_now();
    req.payload.logger_send_started_realtime = Some(logger_send_started_realtime);

    if let Err(e) = sender.blocking_send(req) {
        eprintln!("EventSharing sender closed — failed to send {}: {}", name, e);
    }

    // match sender.try_send(req) {
    //     Ok(_) => {
    //     }
    //     Err(TrySendError::Full(_)) => {
    //         eprintln!("EventSharing queue full — dropped {}", name);
    //     }
    //     Err(TrySendError::Closed(_)) => {
    //         eprintln!("EventSharing sender closed — dropped {}", name);
    //     }
    // }
}
fn unix_nanos_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("SystemTime before UNIX EPOCH!")
        .as_nanos() as i64
}
