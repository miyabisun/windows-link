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
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, broadcast};
use tower_http::trace::TraceLayer;
use tracing::warn;

use crate::{
    audio::Audio,
    buttons::{self, AudioSnapshot, ButtonState, ButtonView, PressError},
    config::Config,
    desktops::{self, VirtualDesktops},
    discord::{NoVoice, Voice},
    icons,
    launch::{Launcher, windows::WindowsLauncher},
    power,
};

/// How often state is re-read to catch changes Windows does not notify (e.g. mixer volume).
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct AppState {
    config: Arc<Config>,
    audio: Arc<dyn Audio>,
    hub: Arc<Hub>,
    desktops: Option<Arc<dyn VirtualDesktops>>,
    voice: Arc<dyn Voice>,
    launcher: Arc<dyn Launcher>,
    icons: Arc<std::sync::Mutex<std::collections::HashMap<String, Arc<Vec<u8>>>>>,
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
            desktops: None,
            voice: Arc::new(NoVoice("no Discord connection".into())),
            launcher: Arc::new(WindowsLauncher),
            icons: Arc::default(),
        }
    }

    /// Start programs and read running processes through `launcher` (tests use a fake).
    #[must_use]
    pub fn with_launcher(mut self, launcher: Arc<dyn Launcher>) -> Self {
        self.launcher = launcher;
        self
    }

    /// Back the `discord.voice` buttons with a Discord connection.
    #[must_use]
    pub fn with_voice(mut self, voice: Arc<dyn Voice>) -> Self {
        self.voice = voice;
        self
    }

    /// Include virtual desktops in `/events` snapshots and change messages.
    #[must_use]
    pub fn with_desktops(mut self, desktops: Arc<dyn VirtualDesktops>) -> Self {
        self.desktops = Some(desktops);
        self
    }

    async fn desktop_listing(&self) -> Value {
        match &self.desktops {
            Some(desktops) => desktops::api::listing(desktops.clone(), &self.config.desktops).await,
            None => json!({
                "desktops": [],
                "unmatched": [],
                "error": "virtual desktops are not enabled"
            }),
        }
    }

    /// Push the current desktop list on `/events` after a desktop change.
    pub async fn publish_desktops(&self, reason: &str) {
        let listing = self.desktop_listing().await;
        let _ordering = self.hub.last.lock().await;
        let _ = self.hub.events.send(
            json!({
                "type": "desktops",
                "reason": reason,
                "desktops": listing["desktops"],
                "unmatched": listing["unmatched"],
                "error": listing["error"],
            })
            .to_string(),
        );
    }

    /// Re-read every button, broadcast the ones that changed, and return all of them.
    pub async fn refresh(&self) -> Vec<ButtonView> {
        let config = self.config.clone();
        let audio = self.audio.clone();
        let voice = self.voice.clone();
        let launcher = self.launcher.clone();
        let views = tokio::task::spawn_blocking(move || {
            compute(&config, audio.as_ref(), voice.as_ref(), launcher.as_ref())
        })
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

fn compute(
    config: &Config,
    audio: &dyn Audio,
    voice: &dyn Voice,
    launcher: &dyn Launcher,
) -> Vec<ButtonView> {
    let snapshot = AudioSnapshot::read(audio);
    let voice = voice.status();
    let processes = launcher.processes();
    config
        .buttons
        .iter()
        .map(|button| match &snapshot {
            Ok(snapshot) => {
                let readings = buttons::Readings {
                    audio: snapshot,
                    voice: &voice,
                    processes: &processes,
                };
                buttons::view(config, button, &readings, audio)
            }
            Err(error) => ButtonView {
                id: button.id.clone(),
                kind: button.spec.type_name(),
                label: button.label.clone(),
                desktop: button.desktop.clone(),
                except: button.except.clone(),
                icon: false,
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
        .route("/buttons/{id}/icon", get(button_icon))
        .route("/power/sleep", post(sleep))
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
    let voice = state.voice.clone();
    let launcher = state.launcher.clone();
    let pressed_id = id.clone();
    let result = tokio::task::spawn_blocking(move || {
        buttons::press(
            &config,
            &pressed_id,
            audio.as_ref(),
            voice.as_ref(),
            launcher.as_ref(),
        )
    })
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
        Ok(Err(PressError::Launch(failure))) => {
            warn!(button = %id, %failure, "press failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "launch", &failure)
        }
        Ok(Err(PressError::Discord(failure))) => {
            warn!(button = %id, %failure, "press failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "discord", &failure)
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

/// The button's Windows icon as PNG, read once and then kept.
async fn button_icon(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let cached = state
        .icons
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&id)
        .cloned();
    let png = if let Some(png) = cached {
        png
    } else {
        let Some(path) = state
            .config
            .buttons
            .iter()
            .find(|b| b.id == id)
            .and_then(buttons::icon_path)
        else {
            return error(
                StatusCode::NOT_FOUND,
                "not_found",
                "no icon for this button",
            );
        };
        match tokio::task::spawn_blocking(move || icons::icon_png(&path)).await {
            Ok(Ok(png)) => {
                let png = Arc::new(png);
                state
                    .icons
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(id, png.clone());
                png
            }
            Ok(Err(message)) => {
                warn!(button = %id, %message, "cannot read the icon");
                return error(StatusCode::NOT_FOUND, "not_found", &message);
            }
            Err(join) => {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    &join.to_string(),
                );
            }
        }
    };
    (
        [
            (axum::http::header::CONTENT_TYPE, "image/png"),
            (axum::http::header::CACHE_CONTROL, "max-age=3600"),
        ],
        png.as_ref().clone(),
    )
        .into_response()
}

/// Answer first, then put the PC to sleep.
async fn sleep() -> Response {
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        match tokio::task::spawn_blocking(power::sleep).await {
            Ok(Ok(())) => {}
            Ok(Err(message)) => warn!(%message, "cannot sleep"),
            Err(join) => warn!(%join, "cannot sleep"),
        }
    });
    (StatusCode::ACCEPTED, Json(json!({ "sleeping": true }))).into_response()
}

async fn events(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| stream(state, socket))
}

/// Send a full snapshot first, then each changed button; a lagging client gets a
/// fresh snapshot instead of the missed messages.
async fn snapshot(state: &AppState, buttons: Vec<ButtonView>) -> String {
    let listing = state.desktop_listing().await;
    json!({
        "type": "snapshot",
        "buttons": buttons,
        "desktops": listing["desktops"],
        "unmatched": listing["unmatched"],
        "desktops_error": listing["error"],
    })
    .to_string()
}

async fn stream(state: AppState, mut socket: WebSocket) {
    let mut changes = state.hub.events.subscribe();
    state.refresh().await;
    // Publishers send while holding `last`, so under the same lock every queued
    // change is already reflected in it: drop them and start from `last`.
    let first = {
        let last = state.hub.last.lock().await;
        while !matches!(
            changes.try_recv(),
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed)
        ) {}
        last.clone()
    };
    let first = snapshot(&state, first).await;
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
                        snapshot(&state, state.refresh().await).await
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

    #[tokio::test]
    async fn events_include_desktops_and_push_desktop_changes() {
        let (state, _) = state();
        let fake = Arc::new(crate::desktops::api::fake::FakeDesktops::new(&[
            "dev",
            "ゲーム",
        ]));
        let state = state.with_desktops(fake.clone());
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
        assert_eq!(first["desktops"][1]["name"], "ゲーム");
        assert_eq!(first["desktops_error"], Value::Null);

        crate::desktops::VirtualDesktops::switch(fake.as_ref(), "GUID-1").unwrap();
        state.publish_desktops("changed").await;
        let change = next(socket.next().await.unwrap().unwrap());
        assert_eq!(change["type"], "desktops");
        assert_eq!(change["reason"], "changed");
        assert_eq!(change["desktops"][1]["current"], true);
    }

    #[tokio::test]
    async fn buttons_report_their_desktop_and_exceptions() {
        let shared = CONFIG.replace(
            "    devices: [motu, jbl]\n  - id: sf6\n    label: SF6\n    type: audio.app_volume_toggle\n    process: StreetFighter6.exe\n    levels: [0.2, 1.0]\n",
            "    devices: [motu, jbl]\n    except: [dev]\n",
        );
        let files = vec![(
            "SF6".to_owned(),
            "buttons:\n  - id: sf6\n    label: SF6\n    type: audio.app_volume_toggle\n    process: StreetFighter6.exe\n    levels: [0.2, 1.0]\n".to_owned(),
        )];
        let state = AppState::new(
            config::parse_all(&shared, &files).unwrap(),
            Arc::new(FakeAudio::new(Vec::new(), None)),
        );
        let (_, body) = call(state, "GET", "/buttons").await;
        assert_eq!(body[0]["desktop"], Value::Null);
        assert_eq!(body[0]["except"], serde_json::json!(["dev"]));
        assert_eq!(body[1]["desktop"], "SF6");
        assert_eq!(body[1]["icon"], false);
    }
}
