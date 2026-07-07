//! HTTP API + embedded web UI.

use crate::announce::{build_announcement_event, publish};
use crate::state::{now_secs, AppState, ServiceError};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pivss_core::manifest::BackupKind;
use pivss_core::proof::StorageChallenge;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

impl IntoResponse for ServiceError {
    fn into_response(self) -> Response {
        let status = match &self {
            ServiceError::NotFound(_) => StatusCode::NOT_FOUND,
            ServiceError::TooLarge(..) => StatusCode::PAYLOAD_TOO_LARGE,
            ServiceError::Store(crate::store::StoreError::Conflict(_)) => StatusCode::CONFLICT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(json!({ "error": self.to_string() }))).into_response()
    }
}

pub fn build_router(state: Arc<AppState>) -> Router {
    let max_body = state.config.max_backup_bytes as usize + 1024;
    Router::new()
        .route("/", get(|| async { Redirect::temporary("/panel") }))
        .route("/panel", get(panel_page))
        .route("/app", get(app_page))
        .route("/api/v1/info", get(info))
        .route("/api/v1/backups", get(list_backups).post(upload_backup))
        .route("/api/v1/backups/{id}", get(get_backup))
        .route(
            "/api/v1/backups/{id}/versions/{version}/data",
            get(get_backup_data),
        )
        .route("/api/v1/backups/{id}/challenge", post(challenge))
        .route("/api/v1/backups/{id}/payments", post(record_payment))
        .route("/api/v1/payments", get(list_payments))
        .route("/api/v1/announce", post(announce))
        .layer(DefaultBodyLimit::max(max_body))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn panel_page() -> Html<&'static str> {
    Html(include_str!("../web/panel.html"))
}

async fn app_page() -> Html<&'static str> {
    Html(include_str!("../web/app.html"))
}

async fn info(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ServiceError> {
    let backups = state.list_backups().await?;
    let payments = state.list_payments().await?;
    let stored_bytes: u64 = backups
        .iter()
        .flat_map(|b| b.versions.iter().map(|v| v.size))
        .sum();
    Ok(Json(json!({
        "announcement": state.announcement(),
        "nostr_pubkey": state.keys.public_hex(),
        "npub": state.keys.npub(),
        "storage_backend": state.store.backend_name(),
        "seeder": state.seeder.name(),
        "seed_statuses": state.seeder.statuses(),
        "backups_count": backups.len(),
        "stored_bytes": stored_bytes,
        "payments_count": payments.len(),
        "payments_total_msat": payments.iter().map(|p| p.amount_msat).sum::<u64>(),
        "uptime_secs": now_secs().saturating_sub(state.started_at),
        "relays": state.config.nostr.relays,
    })))
}

#[derive(Debug, Deserialize)]
struct UploadParams {
    filename: String,
    #[serde(default)]
    kind: Option<BackupKind>,
    #[serde(default)]
    label: String,
}

async fn upload_backup(
    State(state): State<Arc<AppState>>,
    Query(params): Query<UploadParams>,
    body: Bytes,
) -> Result<impl IntoResponse, ServiceError> {
    let kind = params.kind.unwrap_or(BackupKind::Other);
    let (manifest, version, seed_status, quote_msat) = state
        .store_backup(&params.filename, kind, &params.label, body.to_vec())
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "manifest": manifest,
            "stored_version": version,
            "seed_status": seed_status,
            "quote_msat": quote_msat,
            "bolt12_offer": state.config.bolt12_offer,
        })),
    ))
}

async fn list_backups(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ServiceError> {
    Ok(Json(state.list_backups().await?))
}

async fn get_backup(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ServiceError> {
    Ok(Json(state.get_backup(&id).await?))
}

async fn get_backup_data(
    State(state): State<Arc<AppState>>,
    Path((id, version)): Path<(String, u64)>,
) -> Result<impl IntoResponse, ServiceError> {
    let data = state.get_backup_data(&id, version).await?;
    Ok(([("content-type", "application/octet-stream")], data))
}

#[derive(Debug, Deserialize)]
struct ChallengeBody {
    challenge: StorageChallenge,
    #[serde(default)]
    version: Option<u64>,
}

async fn challenge(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<ChallengeBody>,
) -> Result<impl IntoResponse, ServiceError> {
    let (proof, version) = state
        .answer_challenge(&id, body.version, &body.challenge)
        .await?;
    Ok(Json(json!({ "proof": proof, "version": version })))
}

#[derive(Debug, Deserialize)]
struct PaymentBody {
    amount_msat: u64,
    #[serde(default)]
    note: Option<String>,
    /// "mock" today; a real deployment matches an incoming BOLT12 payment on
    /// the node instead of trusting this endpoint.
    #[serde(default)]
    method: Option<String>,
}

async fn record_payment(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<PaymentBody>,
) -> Result<impl IntoResponse, ServiceError> {
    let method = body.method.unwrap_or_else(|| "mock".into());
    let record = state
        .record_payment(&id, body.amount_msat, &method, body.note)
        .await?;
    Ok((StatusCode::CREATED, Json(record)))
}

async fn list_payments(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ServiceError> {
    Ok(Json(state.list_payments().await?))
}

#[derive(Debug, Deserialize, Serialize)]
struct AnnounceBody {
    #[serde(default)]
    dry_run: bool,
}

async fn announce(
    State(state): State<Arc<AppState>>,
    body: Option<Json<AnnounceBody>>,
) -> Result<impl IntoResponse, ServiceError> {
    let dry_run = body.map(|b| b.dry_run).unwrap_or(false);
    let event = build_announcement_event(&state.keys, &state.announcement(), now_secs());
    let results = if dry_run {
        vec![]
    } else {
        publish(&event, &state.config.nostr.relays).await
    };
    *state.last_announcement.lock().unwrap() = Some((event.clone(), results.clone()));
    Ok(Json(json!({
        "event": event,
        "relays": results,
        "dry_run": dry_run,
    })))
}
