use crate::external_services::event_sharing::{
    InternalEventSharingRequest, InternalEventSharingRequestPayload,
};
use crate::simulation::events::{
    EventsPublisher, LinkEnterEvent, LinkLeaveEvent, OnEventFnBuilder,
};
use std::sync::atomic::{AtomicU64};
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::Sender;

static DROPPED_FULL: OnceLock<AtomicU64> = OnceLock::new();
static DROPPED_CLOSED: OnceLock<AtomicU64> = OnceLock::new();
static SENT_OK: OnceLock<AtomicU64> = OnceLock::new();

fn dropped_full_counter() -> &'static AtomicU64 {
    DROPPED_FULL.get_or_init(|| AtomicU64::new(0))
}

fn dropped_closed_counter() -> &'static AtomicU64 {
    DROPPED_CLOSED.get_or_init(|| AtomicU64::new(0))
}

fn sent_ok_counter() -> &'static AtomicU64 {
    SENT_OK.get_or_init(|| AtomicU64::new(0))
}

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
        let sender_enter = sender.clone();
        let sender_leave = sender.clone();
        //events.on_any(move |ev: &dyn EventTrait| {
        events.on::<LinkEnterEvent, _>(move |ev| {
            let e = ev
                .as_any()
                .downcast_ref::<LinkEnterEvent>()
                .expect("Listener got wrong event type.");
            // LinkEnterEvent
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
                &sender_enter,
                InternalEventSharingRequest { payload },
            );
        });
        // LinkLeaveEvent{
        events.on::<LinkLeaveEvent, _>(move |ev| {
            let e = ev
                .as_any()
                .downcast_ref::<LinkLeaveEvent>()
                .expect("Listener got wrong event type.");
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
                &sender_leave,
                InternalEventSharingRequest { payload },
            );
            return;
        });
    })
}

/// Hilfsfunktion zur Vermeidung von Code-Duplizierung beim Senden
fn send_event(
    sender: &Sender<InternalEventSharingRequest>,
    req: InternalEventSharingRequest,
) {
    match sender.try_send(req) {
        Ok(_) => {}
        Err(TrySendError::Full(_)) => {}
        Err(TrySendError::Closed(_)) => {}
    }
}
