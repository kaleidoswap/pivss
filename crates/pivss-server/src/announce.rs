//! Publish the service announcement to nostr relays.

use futures_util::{SinkExt, StreamExt};
use pivss_core::manifest::ServiceAnnouncement;
use pivss_core::nostr::{client_publish_message, Event, NostrKeys, SERVICE_ANNOUNCEMENT_KIND};
use serde::Serialize;
use std::time::Duration;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[derive(Debug, Clone, Serialize)]
pub struct RelayResult {
    pub relay: String,
    pub ok: bool,
    pub detail: String,
}

/// Build the (addressable, kind 38831) announcement event.
///
/// Tags follow the NIP-89-style handler pattern: a `d` tag makes the event
/// replaceable per service, and flat tags expose price/offer for cheap
/// relay-side filtering without parsing content.
pub fn build_announcement_event(
    keys: &NostrKeys,
    ann: &ServiceAnnouncement,
    created_at: u64,
) -> Event {
    let content = serde_json::to_string(ann).expect("serialize announcement");
    let tags = vec![
        vec!["d".to_string(), "pivss-service".to_string()],
        vec!["name".to_string(), ann.name.clone()],
        vec!["endpoint".to_string(), ann.endpoint.clone()],
        vec![
            "price_sats_per_mib".to_string(),
            ann.price_sats_per_mib.to_string(),
        ],
        vec!["bolt12".to_string(), ann.bolt12_offer.clone()],
        vec!["t".to_string(), "pivss".to_string()],
        vec!["t".to_string(), "backup".to_string()],
    ];
    keys.sign_event(SERVICE_ANNOUNCEMENT_KIND, &content, tags, created_at)
}

/// Send `["EVENT", ...]` to each relay and wait briefly for the `["OK", ...]` ack.
pub async fn publish(event: &Event, relays: &[String]) -> Vec<RelayResult> {
    let msg = client_publish_message(event);
    let mut results = Vec::new();

    for relay in relays {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let (mut ws, _) = connect_async(relay.as_str()).await?;
            ws.send(WsMessage::Text(msg.clone())).await?;
            // Wait for an OK ack (best effort).
            while let Some(frame) = ws.next().await {
                if let WsMessage::Text(txt) = frame? {
                    let v: serde_json::Value = serde_json::from_str(&txt)?;
                    if v.get(0).and_then(|s| s.as_str()) == Some("OK") {
                        let accepted = v.get(2).and_then(|b| b.as_bool()).unwrap_or(false);
                        let detail = v
                            .get(3)
                            .and_then(|s| s.as_str())
                            .unwrap_or_default()
                            .to_string();
                        return Ok::<(bool, String), anyhow::Error>((accepted, detail));
                    }
                }
            }
            anyhow::bail!("relay closed connection without OK")
        })
        .await;

        results.push(match result {
            Ok(Ok((ok, detail))) => RelayResult {
                relay: relay.clone(),
                ok,
                detail: if detail.is_empty() {
                    "accepted".into()
                } else {
                    detail
                },
            },
            Ok(Err(e)) => RelayResult {
                relay: relay.clone(),
                ok: false,
                detail: e.to_string(),
            },
            Err(_) => RelayResult {
                relay: relay.clone(),
                ok: false,
                detail: "timeout".into(),
            },
        });
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use pivss_core::manifest::BackupKind;
    use pivss_core::nostr::verify_event;

    #[test]
    fn announcement_event_is_valid_and_addressable() {
        let keys = NostrKeys::generate();
        let ann = ServiceAnnouncement {
            name: "test".into(),
            description: "d".into(),
            endpoint: "http://localhost:8339".into(),
            bolt12_offer: "lno1test".into(),
            price_sats_per_mib: 21,
            billing_period_secs: 86_400,
            max_backup_bytes: 1024,
            kinds: vec![BackupKind::Lightning, BackupKind::Rgb],
            pivss_version: "0.1.0".into(),
        };
        let ev = build_announcement_event(&keys, &ann, 1_700_000_000);
        assert_eq!(ev.kind, SERVICE_ANNOUNCEMENT_KIND);
        assert!(verify_event(&ev));
        assert!(ev.tags.iter().any(|t| t[0] == "d"));
        let parsed: ServiceAnnouncement = serde_json::from_str(&ev.content).unwrap();
        assert_eq!(parsed.price_sats_per_mib, 21);
    }
}
