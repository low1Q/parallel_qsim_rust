use crate::external_services::event_sharing::{InternalEventSharingRequest, InternalEventSharingRequestPayload,};
use crate::simulation::events::{EventsPublisher, OnEventFnBuilder, LinkEnterEvent, LinkLeaveEvent, EventTrait};
use std::sync::Arc;
use tokio::sync::mpsc::{error::TrySendError, Sender};
use uuid::Uuid;

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
            // Versuch LinkEnterEvent
            if let Some(e) = ev.as_any().downcast_ref::<LinkEnterEvent>() {
                let payload = InternalEventSharingRequestPayload {
                    link_type: LinkEnterEvent::TYPE.to_string(),
                    link_id: e.link.external().to_string(),
                    vehicle_id: e.vehicle.external().to_string(),
                    now: e.time,
                    uuid: Uuid::now_v7(),
                };

                let req = InternalEventSharingRequest {payload};

                match sender.try_send(req) {
                    Ok(_) => {}
                    Err(TrySendError::Full(_)) => {
                        eprintln!(
                            "EventSharing queue full — dropped LinkEnterEvent (link={}, vehicle={}, time={})",
                            e.link.external(),
                            e.vehicle.external(),
                            e.time
                        );
                    }
                    Err(TrySendError::Closed(_)) => {
                        eprintln!("EventSharing sender closed — dropped LinkEnterEvent");
                    }
                }
                return; // handled, early return
            }

            // Versuch LinkLeaveEvent
            if let Some(e) = ev.as_any().downcast_ref::<LinkLeaveEvent>() {
                let payload = InternalEventSharingRequestPayload {
                    link_type: LinkLeaveEvent::TYPE.to_string(),
                    link_id: e.link.external().to_string(),
                    vehicle_id: e.vehicle.external().to_string(),
                    now: e.time,
                    uuid: Uuid::now_v7(),
                };
                //let (tx, _rx): (oneshot::Sender<InternalEventSharingResponse>, oneshot::Receiver<InternalEventSharingResponse>) = oneshot::channel();
                let req = InternalEventSharingRequest { payload };
                match sender.try_send(req) {
                    Ok(_) => {}
                    Err(TrySendError::Full(_)) => {
                        eprintln!(
                            "EventSharing queue full — dropped LinkLeaveEvent (link={}, vehicle={}, time={})",
                            e.link.external(),
                            e.vehicle.external(),
                            e.time
                        );
                    }
                    Err(TrySendError::Closed(_)) => {
                        eprintln!("EventSharing sender closed — dropped LinkLeaveEvent");
                    }
                }
                return;
            }
        });
    })
}
//use crate::simulation::events::link_events::{LinkEnterEvent, LinkLeaveEvent};
// 
// pub fn register_grpc_link_logger(
//     events_publisher: &mut EventsPublisher,
//     sender: Arc<tokio::sync::mpsc::Sender<InternalEventSharingRequest>>,
// ) {
//     events_publisher.on_any(move |ev: &dyn EventTrait| {
//         let sender = sender.clone();
// 
//         // 🔽 Typ-sicherer Downcast
//         if let Some(e) = ev.as_any().downcast_ref::<LinkEnterEvent>() {
//             send_link_event(
//                 sender,
//                 "entered link",
//                 e.link.external(),
//                 e.vehicle.external(),
//                 e.time,
//             );
//         } else if let Some(e) = ev.as_any().downcast_ref::<LinkLeaveEvent>() {
//             send_link_event(
//                 sender,
//                 "left link",
//                 e.link.external(),
//                 e.vehicle.external(),
//                 e.time,
//             );
//         }
//     });
// }
// 
// fn send_link_event(
//     sender: Arc<tokio::sync::mpsc::Sender<InternalEventSharingRequest>>,
//     link_type: &str,
//     link_id: &str,
//     vehicle_id: &str,
//     now: u32,
// ) {
//     let payload = InternalEventSharingRequestPayload {
//         link_type: link_type.to_string(),
//         link_id: link_id.to_string(),
//         vehicle_id: vehicle_id.to_string(),
//         now,
//         uuid: Uuid::now_v7(),
//     };
// 
//     let (tx, _rx) = tokio::sync::oneshot::channel();
//     let req = InternalEventSharingRequest {
//         payload,
//         response_tx: tx,
//     };
// 
//     // 🔥 nicht blockierend, simulierungssicher
//     let _ = tokio::spawn(async move {
//         let _ = sender.send(req).await;
//     });
// }


