use crate::external_services::event_sharing::{
    InternalEventSharingRequest, InternalEventSharingRequestPayload,
};
use crate::simulation::events::{
    EventsPublisher, LinkEnterEvent, LinkLeaveEvent, OnEventFnBuilder,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
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
    partition_id: u32,
) -> Box<OnEventFnBuilder> {
    Box::new(move |events: &mut EventsPublisher| {
        let sender_enter = sender.clone();
        let sender_leave = sender.clone();
        let seq_in_partition = std::rc::Rc::new(std::cell::Cell::new(0u64));
        let seq_enter = seq_in_partition.clone();
        let seq_leave = seq_in_partition.clone();
        //events.on_any(move |ev: &dyn EventTrait| {
        events.on::<LinkEnterEvent, _>(move |ev| {
            let e = ev
                .as_any()
                .downcast_ref::<LinkEnterEvent>()
                .expect("Listener got wrong event type.");
            // LinkEnterEvent
            //if let Some(e) = ev.as_any().downcast_ref::<LinkEnterEvent>() {
            let next_seq = seq_enter.get() + 1;
            seq_enter.set(next_seq);
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
                partition_id,
            };
            send_event(
                &sender_enter,
                InternalEventSharingRequest { payload },
                "LinkEnterEvent",
            );
        });
        // LinkLeaveEvent
        //else if let Some(e) = ev.as_any().downcast_ref::<LinkLeaveEvent>() {
        events.on::<LinkLeaveEvent, _>(move |ev| {
            let e = ev
                .as_any()
                .downcast_ref::<LinkLeaveEvent>()
                .expect("Listener got wrong event type.");
            let next_seq = seq_leave.get() + 1;
            seq_leave.set(next_seq);
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
                partition_id,
            };

            send_event(
                &sender_leave,
                InternalEventSharingRequest { payload },
                "LinkLeaveEvent",
            );
            return;
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

    // if let Err(e) = sender.blocking_send(req) {
    //     eprintln!("EventSharing sender closed — failed to send {}: {}", name, e);
    // }

    match sender.try_send(req) {
        Ok(_) => {
            let sent = sent_ok_counter().fetch_add(1, Ordering::Relaxed) + 1;
        }
        Err(TrySendError::Full(_)) => {
            eprintln!("EventSharing queue full — dropped {}", name);
            let dropped = dropped_full_counter().fetch_add(1, Ordering::Relaxed) + 1;
        }
        Err(TrySendError::Closed(_)) => {
            eprintln!("EventSharing sender closed — dropped {}", name);
            let dropped = dropped_closed_counter().fetch_add(1, Ordering::Relaxed) + 1;
        }
    }
}
fn unix_nanos_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("SystemTime before UNIX EPOCH!")
        .as_nanos() as i64
}

pub fn print_event_sharing_stats() {
    let sent_ok = sent_ok_counter().load(Ordering::Relaxed);
    let dropped_full = dropped_full_counter().load(Ordering::Relaxed);
    let dropped_closed = dropped_closed_counter().load(Ordering::Relaxed);

    let dropped_total = dropped_full + dropped_closed;
    let total_attempted = sent_ok + dropped_total;

    let sent_pct = if total_attempted > 0 {
        100.0 * (sent_ok as f64) / (total_attempted as f64)
    } else {
        0.0
    };

    eprintln!(
        "[event_sharing] final stats: sent_ok={} ({:.4}%).",
        sent_ok, sent_pct,
    );
}
