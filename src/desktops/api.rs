//! `/desktops` and `/windows/{hwnd}/pin`.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};

use super::{DesktopError, VirtualDesktops, parse_hwnd};

#[derive(Clone)]
pub struct DesktopState {
    desktops: Arc<dyn VirtualDesktops>,
}

impl DesktopState {
    pub fn new(desktops: Arc<dyn VirtualDesktops>) -> Self {
        Self { desktops }
    }
}

pub fn router(state: DesktopState) -> Router {
    Router::new()
        .route("/desktops", get(list))
        .route("/desktops/{id}/switch", post(switch))
        .route("/windows/{hwnd}/pin", post(pin))
        .with_state(state)
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "error": code, "message": message }))).into_response()
}

/// `{"desktops": [...], "error": null}`, or an empty list with the reason when the
/// virtual desktop service is unavailable (other features keep working).
pub async fn listing(desktops: Arc<dyn VirtualDesktops>) -> Value {
    match tokio::task::spawn_blocking(move || desktops.list()).await {
        Ok(Ok(list)) => json!({ "desktops": list, "error": null }),
        Ok(Err(message)) => json!({ "desktops": [], "error": message }),
        Err(join) => json!({ "desktops": [], "error": join.to_string() }),
    }
}

async fn list(State(state): State<DesktopState>) -> Json<Value> {
    Json(listing(state.desktops.clone()).await)
}

async fn switch(State(state): State<DesktopState>, Path(id): Path<String>) -> Response {
    let desktops = state.desktops.clone();
    let target = id.clone();
    match tokio::task::spawn_blocking(move || desktops.switch(&target)).await {
        Ok(Ok(())) => Json(listing(state.desktops.clone()).await).into_response(),
        Ok(Err(DesktopError::NotFound)) => error(
            StatusCode::NOT_FOUND,
            "not_found",
            "no desktop with this id",
        ),
        Ok(Err(DesktopError::Failed(message))) => {
            error(StatusCode::SERVICE_UNAVAILABLE, "desktops", &message)
        }
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &join.to_string(),
        ),
    }
}

async fn pin(State(state): State<DesktopState>, Path(hwnd): Path<String>) -> Response {
    let Some(handle) = parse_hwnd(&hwnd) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_hwnd",
            "expected a decimal or 0x-hex window handle",
        );
    };
    let desktops = state.desktops.clone();
    match tokio::task::spawn_blocking(move || desktops.pin_window(handle)).await {
        Ok(Ok(())) => Json(json!({ "pinned": true })).into_response(),
        Ok(Err(DesktopError::NotFound)) => error(
            StatusCode::NOT_FOUND,
            "not_found",
            "no window with this handle",
        ),
        Ok(Err(DesktopError::Failed(message))) => {
            error(StatusCode::SERVICE_UNAVAILABLE, "desktops", &message)
        }
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &join.to_string(),
        ),
    }
}

#[cfg(test)]
pub mod fake {
    use std::sync::Mutex;

    use crate::desktops::{DesktopError, DesktopInfo, VirtualDesktops};

    /// In-memory desktops; `None` simulates an unavailable service.
    pub struct FakeDesktops {
        pub desktops: Mutex<Option<Vec<DesktopInfo>>>,
        pub pinned: Mutex<Vec<isize>>,
    }

    impl FakeDesktops {
        pub fn new(names: &[&str]) -> Self {
            let list = names
                .iter()
                .enumerate()
                .map(|(i, name)| DesktopInfo {
                    id: format!("GUID-{i}"),
                    name: (*name).to_owned(),
                    index: u32::try_from(i).unwrap(),
                    current: i == 0,
                })
                .collect();
            Self {
                desktops: Mutex::new(Some(list)),
                pinned: Mutex::new(Vec::new()),
            }
        }
    }

    impl VirtualDesktops for FakeDesktops {
        fn list(&self) -> Result<Vec<DesktopInfo>, String> {
            self.desktops
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| "service unavailable".to_owned())
        }

        fn switch(&self, id: &str) -> Result<(), DesktopError> {
            let mut guard = self.desktops.lock().unwrap();
            let list = guard
                .as_mut()
                .ok_or_else(|| DesktopError::Failed("service unavailable".into()))?;
            if !list.iter().any(|d| d.id == id) {
                return Err(DesktopError::NotFound);
            }
            for desktop in list.iter_mut() {
                desktop.current = desktop.id == id;
            }
            Ok(())
        }

        fn pin_window(&self, hwnd: isize) -> Result<(), DesktopError> {
            self.pinned.lock().unwrap().push(hwnd);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use serde_json::Value;
    use tower::ServiceExt;

    use super::{DesktopState, fake::FakeDesktops, router};

    async fn post(state: DesktopState, method: &str, uri: &str) -> (StatusCode, Value) {
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn lists_and_switches_desktops() {
        let fake = Arc::new(FakeDesktops::new(&["dev", "ゲーム"]));
        let state = DesktopState::new(fake.clone());
        let (status, body) = post(state.clone(), "GET", "/desktops").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"], Value::Null);
        assert_eq!(body["desktops"][1]["name"], "ゲーム");
        assert_eq!(body["desktops"][0]["current"], true);

        let (status, body) = post(state, "POST", "/desktops/GUID-1/switch").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["desktops"][1]["current"], true);
        assert_eq!(body["desktops"][0]["current"], false);
    }

    #[tokio::test]
    async fn unknown_desktop_is_404_and_unavailable_service_lists_nothing_with_a_reason() {
        let fake = Arc::new(FakeDesktops::new(&["dev"]));
        let state = DesktopState::new(fake.clone());
        let (status, _) = post(state.clone(), "POST", "/desktops/nope/switch").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        *fake.desktops.lock().unwrap() = None;
        let (status, body) = post(state.clone(), "GET", "/desktops").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["desktops"].as_array().unwrap().len(), 0);
        assert_eq!(body["error"], "service unavailable");
        let (status, _) = post(state, "POST", "/desktops/GUID-0/switch").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn pins_by_decimal_or_hex_handle_and_rejects_bad_ones() {
        let fake = Arc::new(FakeDesktops::new(&["dev"]));
        let state = DesktopState::new(fake.clone());
        assert_eq!(
            post(state.clone(), "POST", "/windows/4723016/pin").await.0,
            StatusCode::OK
        );
        assert_eq!(
            post(state.clone(), "POST", "/windows/0x48114/pin").await.0,
            StatusCode::OK
        );
        assert_eq!(
            post(state, "POST", "/windows/abc/pin").await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(*fake.pinned.lock().unwrap(), vec![4_723_016, 0x48114]);
    }
}
