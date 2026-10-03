//! `/touch-monitors`: list monitors and switch "keep cursor" per monitor.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, put},
};
use serde::Deserialize;
use serde_json::json;

use super::{Displays, KeepCursor, MonitorView, store::Store};

#[derive(Clone)]
pub struct TouchState {
    displays: Arc<dyn Displays>,
    store: Arc<Store>,
    keep: KeepCursor,
}

impl TouchState {
    pub fn new(displays: Arc<dyn Displays>, store: Arc<Store>, keep: KeepCursor) -> Self {
        Self {
            displays,
            store,
            keep,
        }
    }

    async fn views(&self) -> Result<Vec<MonitorView>, String> {
        let displays = self.displays.clone();
        let monitors = tokio::task::spawn_blocking(move || displays.monitors())
            .await
            .map_err(|e| e.to_string())??;
        Ok(monitors
            .into_iter()
            .map(|monitor| MonitorView {
                keep_cursor: self.keep.get(&monitor.id),
                monitor,
            })
            .collect())
    }
}

pub fn router(state: TouchState) -> Router {
    Router::new()
        .route("/touch-monitors", get(list))
        .route("/touch-monitors/{id}", put(update))
        .with_state(state)
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "error": code, "message": message }))).into_response()
}

async fn list(State(state): State<TouchState>) -> Response {
    match state.views().await {
        Ok(views) => Json(views).into_response(),
        Err(message) => error(StatusCode::INTERNAL_SERVER_ERROR, "displays", &message),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    keep_cursor: bool,
}

async fn update(
    State(state): State<TouchState>,
    Path(id): Path<String>,
    Json(update): Json<Update>,
) -> Response {
    let views = match state.views().await {
        Ok(views) => views,
        Err(message) => return error(StatusCode::INTERNAL_SERVER_ERROR, "displays", &message),
    };
    let Some(mut view) = views.into_iter().find(|v| v.monitor.id == id) else {
        return error(
            StatusCode::NOT_FOUND,
            "not_found",
            "no connected monitor with this id",
        );
    };
    let store = state.store.clone();
    let monitor_id = id.clone();
    let saved =
        tokio::task::spawn_blocking(move || store.set_keep_cursor(&monitor_id, update.keep_cursor))
            .await;
    match saved {
        Ok(Ok(())) => {
            state.keep.set(&id, update.keep_cursor);
            view.keep_cursor = update.keep_cursor;
            Json(view).into_response()
        }
        Ok(Err(err)) => error(StatusCode::INTERNAL_SERVER_ERROR, "store", &err.to_string()),
        Err(err) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &err.to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use serde_json::Value;
    use tower::ServiceExt;

    use super::{TouchState, router};
    use crate::touch::{Displays, KeepCursor, Monitor, store::Store};

    struct FakeDisplays(Vec<Monitor>);

    impl Displays for FakeDisplays {
        fn monitors(&self) -> Result<Vec<Monitor>, String> {
            Ok(self.0.clone())
        }
    }

    fn monitor(id: &str, name: &str, touch: bool) -> Monitor {
        Monitor {
            id: id.into(),
            name: name.into(),
            device_path: format!("path-{id}"),
            gdi_name: format!(r"\\.\{name}"),
            primary: !touch,
            touch,
        }
    }

    fn state() -> (TouchState, KeepCursor, Arc<Store>) {
        let store = Arc::new(Store::in_memory().unwrap());
        let keep = KeepCursor::default();
        let displays = Arc::new(FakeDisplays(vec![
            monitor("mon-main", "MAIN", false),
            monitor("mon-panel", "PANEL", true),
        ]));
        (
            TouchState::new(displays, store.clone(), keep.clone()),
            keep,
            store,
        )
    }

    async fn send(state: TouchState, method: &str, uri: &str, body: &str) -> (StatusCode, Value) {
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn lists_monitors_with_keep_cursor_on_by_default() {
        let (state, _, _) = state();
        let (status, body) = send(state, "GET", "/touch-monitors", "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body[1]["id"], "mon-panel");
        assert_eq!(body[1]["touch"], true);
        assert_eq!(body[1]["keep_cursor"], true);
        assert_eq!(body[0]["touch"], false);
    }

    #[tokio::test]
    async fn switching_off_updates_the_hook_setting_and_the_store() {
        let (state, keep, store) = state();
        let (status, body) = send(
            state.clone(),
            "PUT",
            "/touch-monitors/mon-panel",
            r#"{"keep_cursor":false}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["keep_cursor"], false);
        assert!(!keep.get("mon-panel"));
        assert!(!store.keep_cursor_overrides().unwrap()["mon-panel"]);
        let (_, body) = send(state, "GET", "/touch-monitors", "").await;
        assert_eq!(body[1]["keep_cursor"], false);
    }

    #[tokio::test]
    async fn unknown_monitor_and_bad_bodies_are_rejected() {
        let (state, keep, _) = state();
        let (status, _) = send(
            state.clone(),
            "PUT",
            "/touch-monitors/mon-gone",
            r#"{"keep_cursor":false}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = send(
            state,
            "PUT",
            "/touch-monitors/mon-panel",
            r#"{"keep":false}"#,
        )
        .await;
        assert!(status.is_client_error());
        assert!(keep.get("mon-panel"));
    }
}
