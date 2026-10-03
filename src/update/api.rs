//! `GET /version` and `POST /update/check`.

use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::json;
use tracing::info;

use super::{Outcome, Updater};

/// Time for the response to reach the caller before the process exits.
const RESTART_DELAY: Duration = Duration::from_millis(500);

pub fn router(updater: Arc<Updater>) -> Router {
    Router::new()
        .route("/version", get(version))
        .route("/update/check", post(check))
        .with_state(updater)
}

async fn version(State(updater): State<Arc<Updater>>) -> Json<serde_json::Value> {
    Json(json!({ "version": updater.current().to_string() }))
}

/// Check now instead of waiting for the hourly check. When a newer version is
/// installed, the response is sent first and the server then restarts into it.
async fn check(State(updater): State<Arc<Updater>>) -> Response {
    let worker = updater.clone();
    let current = updater.current().to_string();
    match tokio::task::spawn_blocking(move || worker.check()).await {
        Ok(Ok(Outcome::UpToDate { latest })) => Json(json!({
            "result": "up_to_date", "current": current, "latest": latest.to_string()
        }))
        .into_response(),
        Ok(Ok(Outcome::Skipped { reason })) => Json(json!({
            "result": "skipped", "current": current, "reason": reason
        }))
        .into_response(),
        Ok(Ok(Outcome::Restarting { latest })) => {
            info!(%latest, "installed a new version");
            tokio::spawn(async move {
                tokio::time::sleep(RESTART_DELAY).await;
                updater.restart();
            });
            Json(json!({
                "result": "restarting", "current": current, "latest": latest.to_string()
            }))
            .into_response()
        }
        Ok(Err(message)) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": "update", "message": message })),
        )
            .into_response(),
        Err(join) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "internal", "message": join.to_string() })),
        )
            .into_response(),
    }
}
