//! HTTP API: button list, press, and a WebSocket stream of state changes.

use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{
        Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::json;
use tokio::sync::{Mutex, Notify, broadcast};
use tower_http::trace::TraceLayer;
use tracing::warn;

use crate::{
    audio::Audio,
    buttons::{self, AudioSnapshot, ButtonState, ButtonView, PressError},
    config::Config,
};

/// How often state is re-read to catch changes Windows does not notify (e.g. mixer volume).
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct AppState {
    config: Arc<Config>,
    audio: Arc<dyn Audio>,
    hub: Arc<Hub>,
}

struct Hub {
    last: Mutex<Vec<ButtonView>>,
    events: broadcast::Sender<String>,
}

impl AppState {
    pub fn new(config: Config, audio: Arc<dyn Audio>) -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            config: Arc::new(config),
            audio,
            hub: Arc::new(Hub {
                last: Mutex::new(Vec::new()),
                events,
            }),
        }
    }

    /// Re-read every button, broadcast the ones that changed, and return all of them.
    pub async fn refresh(&self) -> Vec<ButtonView> {
        let config = self.config.clone();
        let audio = self.audio.clone();
        let views = tokio::task::spawn_blocking(move || compute(&config, audio.as_ref()))
            .await
            .unwrap_or_default();
        let mut last = self.hub.last.lock().await;
        for view in &views {
            if !last.contains(view) {
                let _ = self
                    .hub
                    .events
                    .send(json!({ "type": "button", "button": view }).to_string());
            }
        }
        last.clone_from(&views);
        views
    }

    /// Refresh on every Windows notification and at least every `POLL_INTERVAL`.
    pub async fn run_refresher(self, changes: Arc<Notify>) {
        loop {
            tokio::select! {
                () = changes.notified() => {}
                () = tokio::time::sleep(POLL_INTERVAL) => {}
            }
            self.refresh().await;
        }
    }
}

fn compute(config: &Config, audio: &dyn Audio) -> Vec<ButtonView> {
    let snapshot = AudioSnapshot::read(audio);
    config
        .buttons
        .iter()
        .map(|button| match &snapshot {
            Ok(snapshot) => buttons::view(config, button, snapshot, audio),
            Err(error) => ButtonView {
                id: button.id.clone(),
                kind: button.spec.type_name(),
                label: button.label.clone(),
                state: ButtonState::Error {
                    message: error.to_string(),
                },
            },
        })
        .collect()
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/buttons", get(list_buttons))
        .route("/buttons/{id}/press", post(press_button))
        .route("/events", get(events))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok\n"
}

async fn list_buttons(State(state): State<AppState>) -> Json<Vec<ButtonView>> {
    Json(state.refresh().await)
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "error": code, "message": message }))).into_response()
}

async fn press_button(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let config = state.config.clone();
    let audio = state.audio.clone();
    let pressed_id = id.clone();
    let result =
        tokio::task::spawn_blocking(move || buttons::press(&config, &pressed_id, audio.as_ref()))
            .await;
    match result {
        Ok(Ok(())) => {
            let views = state.refresh().await;
            match views.into_iter().find(|view| view.id == id) {
                Some(view) => Json(json!({ "button": view })).into_response(),
                None => error(StatusCode::NOT_FOUND, "not_found", "unknown button"),
            }
        }
        Ok(Err(PressError::NotFound)) => {
            error(StatusCode::NOT_FOUND, "not_found", "unknown button")
        }
        Ok(Err(PressError::Conflict { code, message })) => {
            error(StatusCode::CONFLICT, code, &message)
        }
        Ok(Err(PressError::Audio(failure))) => {
            warn!(button = %id, %failure, "press failed");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "audio",
                &failure.to_string(),
            )
        }
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &join.to_string(),
        ),
    }
}

async fn events(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| stream(state, socket))
}

/// Send a full snapshot first, then each changed button; a lagging client gets a
/// fresh snapshot instead of the missed messages.
async fn stream(state: AppState, mut socket: WebSocket) {
    let mut changes = state.hub.events.subscribe();
    let snapshot = |views: Vec<ButtonView>| json!({ "type": "snapshot", "buttons": views });
    state.refresh().await;
    // Publishers send while holding `last`, so under the same lock every queued
    // change is already reflected in it: drop them and start from `last`.
    let first = {
        let last = state.hub.last.lock().await;
        while !matches!(
            changes.try_recv(),
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed)
        ) {}
        snapshot(last.clone()).to_string()
    };
    if socket.send(Message::Text(first.into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_)) | Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
            change = changes.recv() => {
                let text = match change {
                    Ok(text) => text,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        snapshot(state.refresh().await).to_string()
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                if socket.send(Message::Text(text.into())).await.is_err() {
                    return;
                }
            }
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
    use futures_util::StreamExt;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::{AppState, app};
    use crate::{
        audio::{Device, fake::FakeAudio},
        config,
    };

    const CONFIG: &str = r"
devices:
  motu: id-motu
  jbl: id-jbl
buttons:
  - id: output
    label: Output
    type: audio.output_toggle
    devices: [motu, jbl]
  - id: sf6
    label: SF6
    type: audio.app_volume_toggle
    process: StreetFighter6.exe
    levels: [0.2, 1.0]
";

    fn state() -> (AppState, Arc<FakeAudio>) {
        let audio = Arc::new(FakeAudio::new(
            vec![
                Device {
                    id: "id-motu".into(),
                    name: "MOTU".into(),
                    connected: true,
                },
                Device {
                    id: "id-jbl".into(),
                    name: "JBL".into(),
                    connected: true,
                },
            ],
            Some("id-motu"),
        ));
        audio.set_volume("streetfighter6.exe", 1.0);
        (
            AppState::new(config::parse(CONFIG).unwrap(), audio.clone()),
            audio,
        )
    }

    async fn call(state: AppState, method: &str, uri: &str) -> (StatusCode, Value) {
        let response = app(state)
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
    async fn lists_buttons_in_config_order_with_state() {
        let (state, _) = state();
        let (status, body) = call(state, "GET", "/buttons").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body[0]["id"], "output");
        assert_eq!(body[0]["type"], "audio.output_toggle");
        assert_eq!(body[0]["state"]["kind"], "output");
        assert_eq!(body[0]["state"]["current"], "motu");
        assert_eq!(body[1]["state"]["kind"], "volume");
        assert_eq!(body[1]["state"]["volume"], 1.0);
    }

    #[tokio::test]
    async fn unavailable_audio_keeps_serving_and_reports_the_error() {
        let config = config::parse(CONFIG).unwrap();
        let state = AppState::new(
            config,
            Arc::new(crate::audio::UnavailableAudio(
                "audio service stopped".into(),
            )),
        );
        let (status, body) = call(state.clone(), "GET", "/buttons").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body[0]["state"]["kind"], "error");
        assert_eq!(body[0]["state"]["message"], "audio service stopped");
        let (status, body) = call(state, "POST", "/buttons/output/press").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"], "audio");
    }

    #[tokio::test]
    async fn press_returns_the_new_state() {
        let (state, _) = state();
        let (status, body) = call(state.clone(), "POST", "/buttons/output/press").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["button"]["state"]["current"], "jbl");
        let (_, body) = call(state, "POST", "/buttons/sf6/press").await;
        assert_eq!(body["button"]["state"]["volume"], 0.2);
    }

    #[tokio::test]
    async fn press_errors_map_to_status_codes() {
        let (state, audio) = state();
        let (status, body) = call(state.clone(), "POST", "/buttons/nope/press").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "not_found");

        audio.set_connected("id-jbl", false);
        let (status, body) = call(state.clone(), "POST", "/buttons/output/press").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "device_unavailable");

        audio.state.lock().unwrap().volumes.clear();
        let (status, body) = call(state, "POST", "/buttons/sf6/press").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "not_running");
    }

    #[tokio::test]
    async fn events_stream_a_snapshot_then_external_changes() {
        let (state, audio) = state();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_state = state.clone();
        tokio::spawn(async move { axum::serve(listener, app(server_state)).await });

        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/events"))
            .await
            .unwrap();
        let next = |text: tokio_tungstenite::tungstenite::Message| -> Value {
            serde_json::from_str(text.to_text().unwrap()).unwrap()
        };
        let first = next(socket.next().await.unwrap().unwrap());
        assert_eq!(first["type"], "snapshot");
        assert_eq!(first["buttons"].as_array().unwrap().len(), 2);

        // A change made outside the server (e.g. Windows sound settings) is pushed
        // on the next refresh.
        audio.state.lock().unwrap().default_output = Some("id-jbl".into());
        state.refresh().await;
        let change = next(socket.next().await.unwrap().unwrap());
        assert_eq!(change["type"], "button");
        assert_eq!(change["button"]["id"], "output");
        assert_eq!(change["button"]["state"]["current"], "jbl");
    }
}
