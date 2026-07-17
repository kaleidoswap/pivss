//! Discover PIVSS providers by querying nostr relays for their kind
//! `38831` service announcements — the client-side counterpart to the
//! server's "Publish to relays" button. Without this, a published
//! announcement is real and retrievable (verified: `nak`/raw NIP-01 `REQ`
//! finds it), but nothing in PIVSS ever looked for it.

use futures_util::{SinkExt, StreamExt};
use pivss_core::manifest::ServiceAnnouncement;
use pivss_core::nostr::{client_query_message, pubkey_hex_to_npub, verify_event, Event};
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[derive(Debug, Clone)]
pub struct DiscoveredProvider {
    pub pubkey: String,
    pub npub: String,
    pub announcement: ServiceAnnouncement,
    pub created_at: u64,
    /// Relays this provider's announcement was seen on.
    pub seen_on: Vec<String>,
}

/// Query each relay for kind 38831 events, verify each one's signature, and
/// return the newest valid announcement per pubkey (de-duplicated across
/// relays), sorted by price ascending.
pub async fn discover_providers(
    relays: &[String],
    per_relay_timeout: Duration,
) -> Vec<DiscoveredProvider> {
    let mut by_pubkey: HashMap<String, (Event, Vec<String>)> = HashMap::new();

    for relay in relays {
        let events = match query_relay(relay, per_relay_timeout).await {
            Ok(events) => events,
            Err(e) => {
                eprintln!("  {relay}: {e}");
                continue;
            }
        };
        for ev in events {
            if !verify_event(&ev) {
                continue;
            }
            match by_pubkey.get_mut(&ev.pubkey) {
                Some((existing, seen_on)) => {
                    seen_on.push(relay.clone());
                    if ev.created_at > existing.created_at {
                        *existing = ev;
                    }
                }
                None => {
                    by_pubkey.insert(ev.pubkey.clone(), (ev, vec![relay.clone()]));
                }
            }
        }
    }

    let mut out: Vec<DiscoveredProvider> = by_pubkey
        .into_values()
        .filter_map(|(ev, seen_on)| {
            let announcement: ServiceAnnouncement = serde_json::from_str(&ev.content).ok()?;
            let npub = pubkey_hex_to_npub(&ev.pubkey).ok()?;
            Some(DiscoveredProvider {
                pubkey: ev.pubkey,
                npub,
                announcement,
                created_at: ev.created_at,
                seen_on,
            })
        })
        .collect();

    out.sort_by_key(|p| p.announcement.price_sats_per_mib);
    out
}

async fn query_relay(relay: &str, timeout: Duration) -> anyhow::Result<Vec<Event>> {
    let (mut ws, _) = tokio::time::timeout(timeout, connect_async(relay)).await??;
    let sub_id = "pivss-discover";
    ws.send(WsMessage::Text(client_query_message(
        sub_id,
        &[pivss_core::nostr::SERVICE_ANNOUNCEMENT_KIND],
    )))
    .await?;

    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let Ok(Some(frame)) = tokio::time::timeout(remaining, ws.next()).await else {
            break;
        };
        let WsMessage::Text(txt) = frame? else {
            continue;
        };
        let msg: serde_json::Value = serde_json::from_str(&txt)?;
        match msg.get(0).and_then(|v| v.as_str()) {
            Some("EVENT") => {
                if let Some(ev) = msg.get(2) {
                    if let Ok(event) = serde_json::from_value::<Event>(ev.clone()) {
                        events.push(event);
                    }
                }
            }
            Some("EOSE") => break,
            _ => {}
        }
    }
    let _ = ws
        .send(WsMessage::Text(
            serde_json::json!(["CLOSE", sub_id]).to_string(),
        ))
        .await;
    Ok(events)
}
