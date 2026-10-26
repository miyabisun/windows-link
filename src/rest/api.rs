//! `/power` (whether resting), `/power/rest` and `/power/wake`.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;

use super::{Rest, RestError};

pub fn router(rest: Arc<Rest>) -> Router {
    Router::new()
        .route("/power", get(status))
        .route("/power/rest", post(start))
        .route("/power/wake", post(wake))
        .with_state(rest)
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "error": code, "message": message }))).into_response()
}

async fn status(State(rest): State<Arc<Rest>>) -> Response {
    answer(rest, |_| Ok(())).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    /// The monitor to keep lit: an id from `GET /touch-monitors`.
    keep: String,
}

async fn start(State(rest): State<Arc<Rest>>, Json(start): Json<Start>) -> Response {
    answer(rest, move |rest| rest.rest(&start.keep)).await
}

async fn wake(State(rest): State<Arc<Rest>>) -> Response {
    answer(rest, Rest::wake).await
}

/// Run `work` on a blocking thread (DDC/CI is slow) and answer with whether resting
/// afterwards.
async fn answer(
    rest: Arc<Rest>,
    work: impl FnOnce(&Rest) -> Result<(), RestError> + Send + 'static,
) -> Response {
    let done = tokio::task::spawn_blocking(move || work(&rest).and_then(|()| rest.resting())).await;
    match done {
        Ok(Ok(resting)) => Json(json!({ "resting": resting })).into_response(),
        Ok(Err(RestError::UnknownMonitor)) => error(
            StatusCode::NOT_FOUND,
            "not_found",
            "no connected monitor with this id",
        ),
        Ok(Err(RestError::Unrestored(monitors))) => error(
            StatusCode::CONFLICT,
            "unrestored",
            &format!(
                "cannot put the brightness back on {}; wake again to retry",
                monitors.join(", ")
            ),
        ),
        Ok(Err(RestError::Failed(message))) => {
            error(StatusCode::INTERNAL_SERVER_ERROR, "rest", &message)
        }
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &join.to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use serde_json::Value;
    use tower::ServiceExt;

    use super::router;
    use crate::{
        audio::fake::FakeAudio,
        rest::{Rest, Screens, store::RestStore},
        touch::{Displays, Monitor},
    };

    struct FakeDisplays(Vec<Monitor>);

    impl Displays for FakeDisplays {
        fn monitors(&self) -> Result<Vec<Monitor>, String> {
            Ok(self.0.clone())
        }
    }

    /// Brightness by monitor id; `refuse` makes every write fail.
    #[derive(Default)]
    struct FakeScreens {
        levels: Mutex<HashMap<String, u32>>,
        refuse: Mutex<bool>,
    }

    impl Screens for FakeScreens {
        fn brightness(&self, monitor: &Monitor) -> Option<u32> {
            self.levels.lock().unwrap().get(&monitor.id).copied()
        }

        fn set_brightness(&self, monitor: &Monitor, value: u32) -> Result<(), String> {
            if *self.refuse.lock().unwrap() {
                return Err("refused".into());
            }
            self.levels
                .lock()
                .unwrap()
                .insert(monitor.id.clone(), value);
            Ok(())
        }

        fn cover(&self, _: &[Monitor]) {}
    }

    fn monitor(id: &str) -> Monitor {
        Monitor {
            id: id.into(),
            name: id.to_uppercase(),
            device_path: format!("path-{id}"),
            gdi_name: format!(r"\\.\{id}"),
            primary: false,
            touch: false,
        }
    }

    fn rest() -> (Arc<Rest>, Arc<FakeScreens>) {
        let screens = Arc::new(FakeScreens::default());
        screens.levels.lock().unwrap().insert("main".into(), 40);
        let rest = Rest::new(
            Arc::new(FakeDisplays(vec![monitor("panel"), monitor("main")])),
            screens.clone(),
            Arc::new(FakeAudio::new(Vec::new(), None)),
            RestStore::in_memory().unwrap(),
        );
        (Arc::new(rest), screens)
    }

    async fn send(rest: Arc<Rest>, method: &str, uri: &str, body: &str) -> (StatusCode, Value) {
        let response = router(rest)
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
    async fn rests_and_wakes() {
        let (rest, screens) = rest();
        let (status, body) = send(rest.clone(), "GET", "/power", "").await;
        assert_eq!(
            (status, body["resting"].clone()),
            (StatusCode::OK, false.into())
        );
        let (status, body) = send(rest.clone(), "POST", "/power/rest", r#"{"keep":"panel"}"#).await;
        assert_eq!(
            (status, body["resting"].clone()),
            (StatusCode::OK, true.into())
        );
        assert_eq!(screens.levels.lock().unwrap()["main"], 0);
        let (_, body) = send(rest.clone(), "GET", "/power", "").await;
        assert_eq!(body["resting"], true);
        let (status, body) = send(rest, "POST", "/power/wake", "").await;
        assert_eq!(
            (status, body["resting"].clone()),
            (StatusCode::OK, false.into())
        );
        assert_eq!(screens.levels.lock().unwrap()["main"], 40);
    }

    #[tokio::test]
    async fn an_unknown_or_missing_monitor_is_rejected() {
        let (rest, screens) = rest();
        let (status, body) = send(rest.clone(), "POST", "/power/rest", r#"{"keep":"gone"}"#).await;
        assert_eq!(
            (status, body["error"].clone()),
            (StatusCode::NOT_FOUND, "not_found".into())
        );
        let (status, _) = send(
            rest.clone(),
            "POST",
            "/power/rest",
            r#"{"monitor":"panel"}"#,
        )
        .await;
        assert!(status.is_client_error());
        assert_eq!(screens.levels.lock().unwrap()["main"], 40);
        let (_, body) = send(rest, "GET", "/power", "").await;
        assert_eq!(body["resting"], false);
    }

    #[tokio::test]
    async fn a_wake_that_cannot_put_the_brightness_back_says_where() {
        let (rest, screens) = rest();
        send(rest.clone(), "POST", "/power/rest", r#"{"keep":"panel"}"#).await;
        *screens.refuse.lock().unwrap() = true;
        let (status, body) = send(rest.clone(), "POST", "/power/wake", "").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "unrestored");
        assert!(body["message"].as_str().unwrap().contains("MAIN"));
        *screens.refuse.lock().unwrap() = false;
        let (status, _) = send(rest, "POST", "/power/wake", "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(screens.levels.lock().unwrap()["main"], 40);
    }
}
