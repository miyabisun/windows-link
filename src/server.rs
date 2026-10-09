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
use tracing::{info, warn};

use crate::{
    audio::Audio,
    buttons::{self, AudioSnapshot, ButtonState, ButtonView, PressError},
    config::{ButtonSpec, Config},
    desktops::{self, VirtualDesktops},
    discord::{Discord, NoDiscord},
    icons,
    launch::{Launcher, windows::WindowsLauncher},
    library::{
        self, GameLibrary, KeyError, LabelError, NoLibrary, Picture, Pin, Pins, Update, UpdateError,
    },
    power,
};

/// How often state is re-read to catch changes Windows does not notify (e.g. mixer volume).
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Library pictures read from this PC, by `<button>/<item>`, with their media type.
type Pictures = std::collections::HashMap<String, (Arc<Vec<u8>>, &'static str)>;

#[derive(Clone)]
pub struct AppState {
    config: Arc<Config>,
    audio: Arc<dyn Audio>,
    hub: Arc<Hub>,
    desktops: Option<Arc<dyn VirtualDesktops>>,
    discord: Arc<dyn Discord>,
    launcher: Arc<dyn Launcher>,
    icons: Arc<std::sync::Mutex<std::collections::HashMap<String, Arc<Vec<u8>>>>>,
    pictures: Arc<std::sync::Mutex<Pictures>>,
    /// Each library button's library, by button ID.
    libraries: Arc<std::collections::HashMap<String, Arc<dyn GameLibrary>>>,
    pins: Arc<Pins>,
    /// FANZA's sign-in, and how to start a FANZA round at once, when a button needs it.
    fanza: Option<(Arc<crate::fanza::Client>, Arc<Notify>)>,
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
            discord: Arc::new(NoDiscord),
            launcher: Arc::new(WindowsLauncher),
            icons: Arc::default(),
            pictures: Arc::default(),
            libraries: Arc::default(),
            pins: Arc::new(Pins::in_memory().expect("an in-memory database opens")),
            fanza: None,
        }
    }

    /// The library a library button opens.
    #[must_use]
    pub fn with_library(mut self, button: &str, library: Arc<dyn GameLibrary>) -> Self {
        Arc::make_mut(&mut self.libraries).insert(button.to_owned(), library);
        self
    }

    /// Read library pictures again when next asked (a game's picture may have changed
    /// from its program's icon to its art).
    pub fn forget_pictures(&self) {
        self.pictures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    /// Keep library pins in `pins` (by default they last until the server stops).
    #[must_use]
    pub fn with_pins(mut self, pins: Arc<Pins>) -> Self {
        self.pins = pins;
        self
    }

    /// The labels a library button hides and its library, or `None` when `id` is not a
    /// library button.
    fn library_button(&self, id: &str) -> Option<(Vec<String>, Arc<dyn GameLibrary>)> {
        let hide = self
            .config
            .buttons
            .iter()
            .find(|b| b.id == id)
            .and_then(|b| match &b.spec {
                ButtonSpec::SteamLibrary { hide }
                | ButtonSpec::DlsiteLibrary { hide, .. }
                | ButtonSpec::FanzaLibrary { hide, .. } => Some(hide.clone()),
                _ => None,
            })?;
        let library = self.libraries.get(id).cloned().unwrap_or_else(|| {
            Arc::new(NoLibrary(
                "no game library is set up for this button".into(),
            ))
        });
        Some((hide, library))
    }

    /// Take FANZA's sign-in from the panel (`PUT /fanza/session`) into `client`, then
    /// wake the FANZA round through `wake`.
    #[must_use]
    pub fn with_fanza(mut self, client: Arc<crate::fanza::Client>, wake: Arc<Notify>) -> Self {
        self.fanza = Some((client, wake));
        self
    }

    /// Start programs and read running processes through `launcher` (tests use a fake).
    #[must_use]
    pub fn with_launcher(mut self, launcher: Arc<dyn Launcher>) -> Self {
        self.launcher = launcher;
        self
    }

    /// Read the `discord.server` buttons' icons from a Discord connection.
    #[must_use]
    pub fn with_discord(mut self, discord: Arc<dyn Discord>) -> Self {
        self.discord = discord;
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
        let server_icons = self.discord.icons();
        let launcher = self.launcher.clone();
        let pins = self.pins.clone();
        let sign_ins: std::collections::HashMap<String, String> = self
            .libraries
            .iter()
            .filter_map(|(id, library)| Some((id.clone(), library.sign_in()?.to_owned())))
            .collect();
        let views = tokio::task::spawn_blocking(move || {
            compute(
                &config,
                audio.as_ref(),
                &server_icons,
                &sign_ins,
                launcher.as_ref(),
                &pins,
            )
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
    server_icons: &crate::discord::ServerIcons,
    sign_ins: &std::collections::HashMap<String, String>,
    launcher: &dyn Launcher,
    pins: &Pins,
) -> Vec<ButtonView> {
    let snapshot = AudioSnapshot::read(audio);
    let processes = launcher.processes();
    let pins = pins.all().unwrap_or_else(|err| {
        warn!(%err, "cannot read the library pins");
        library::Pinned::new()
    });
    config
        .buttons
        .iter()
        .map(|button| match &snapshot {
            Ok(snapshot) => {
                let readings = buttons::Readings {
                    audio: snapshot,
                    server_icons,
                    sign_ins,
                    processes: &processes,
                    pins: &pins,
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
        .route("/buttons/{id}/library", get(library_listing))
        .route("/buttons/{id}/library/update", post(update_library))
        .route("/buttons/{id}/library/{item}/start", post(start_item))
        .route("/buttons/{id}/library/{item}/image", get(item_picture))
        .route("/buttons/{id}/library/{item}/folder", post(open_folder))
        .route("/buttons/{id}/library/{item}/programs", get(item_programs))
        .route("/buttons/{id}/library/{item}/keys", get(license_keys))
        .route("/audio/mixer", get(mixer))
        .route("/audio/master", axum::routing::put(set_master))
        .route("/audio/apps/{process}", axum::routing::put(set_app_volume))
        .route(
            "/buttons/{id}/library/{item}/program",
            axum::routing::put(choose_program),
        )
        .route("/buttons/{id}/labels", post(create_label))
        .route(
            "/buttons/{id}/labels/{label}",
            axum::routing::patch(rename_label).delete(delete_label),
        )
        .route(
            "/buttons/{id}/labels/{label}/items/{item}",
            axum::routing::put(add_to_label).delete(remove_from_label),
        )
        .route(
            "/buttons/{id}/pins/{item}",
            axum::routing::put(pin_item).delete(unpin_item),
        )
        .route("/fanza/session", axum::routing::put(fanza_session))
        .route("/power/sleep", post(sleep))
        .route("/events", get(events))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok\n"
}

#[derive(serde::Deserialize)]
struct FanzaSession {
    cookies: Vec<crate::fanza::session::Cookie>,
}

/// Keep the cookies of the panel's FANZA login window, then read the purchases and
/// download what is missing at once.
async fn fanza_session(
    State(state): State<AppState>,
    Json(session): Json<FanzaSession>,
) -> Response {
    let Some((client, wake)) = state.fanza.clone() else {
        return error(
            StatusCode::NOT_FOUND,
            "not_found",
            "no FANZA library is set up",
        );
    };
    let kept = tokio::task::spawn_blocking(move || client.replace(session.cookies)).await;
    match kept {
        Ok(Ok(kept)) => {
            info!(kept, "signed in to FANZA through the panel");
            wake.notify_one();
            state.refresh().await;
            Json(json!({ "kept": kept })).into_response()
        }
        Ok(Err(reason)) => error(StatusCode::BAD_REQUEST, "invalid_session", &reason),
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &join.to_string(),
        ),
    }
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
    let launcher = state.launcher.clone();
    let pressed_id = id.clone();
    let result = tokio::task::spawn_blocking(move || {
        buttons::press(&config, &pressed_id, audio.as_ref(), launcher.as_ref())
    })
    .await;
    match result {
        Ok(Ok(())) => button_response(&state, &id).await,
        Ok(Err(failure)) => press_error(&id, failure, "unknown button"),
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &join.to_string(),
        ),
    }
}

/// `{"button": …}` with the button's state after a change.
async fn button_response(state: &AppState, id: &str) -> Response {
    let views = state.refresh().await;
    match views.into_iter().find(|view| view.id == id) {
        Some(view) => Json(json!({ "button": view })).into_response(),
        None => error(StatusCode::NOT_FOUND, "not_found", "unknown button"),
    }
}

fn press_error(id: &str, failure: PressError, not_found: &str) -> Response {
    match failure {
        PressError::NotFound => error(StatusCode::NOT_FOUND, "not_found", not_found),
        PressError::Conflict { code, message } => error(StatusCode::CONFLICT, code, &message),
        PressError::Launch(failure) => {
            warn!(button = %id, %failure, "press failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "launch", &failure)
        }
        PressError::Audio(failure) => {
            warn!(button = %id, %failure, "press failed");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "audio",
                &failure.to_string(),
            )
        }
    }
}

const NOT_A_LIBRARY: &str = "no library button has this id";

/// The games a library button opens, each with whether it is pinned to the button. The
/// library is read again afterwards, for the next listing.
async fn library_listing(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some((hide, library)) = state.library_button(&id) else {
        return error(StatusCode::NOT_FOUND, "not_found", NOT_A_LIBRARY);
    };
    let pins = state.pins.clone();
    let reading = Arc::clone(&library);
    let read = tokio::task::spawn_blocking(move || (reading.listing(), pins.all())).await;
    tokio::task::spawn_blocking(move || library.reread());
    let (listing, pins) = match read {
        Ok((listing, Ok(pins))) => (listing, pins),
        Ok((_, Err(err))) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage",
                &err.to_string(),
            );
        }
        Err(join) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                &join.to_string(),
            );
        }
    };
    let pinned = pins.get(&id).cloned().unwrap_or_default();
    let hidden: Vec<&str> = listing
        .labels
        .iter()
        .filter(|label| hide.iter().any(|name| name == &label.name))
        .map(|label| label.id.as_str())
        .collect();
    let items: Vec<Value> = listing
        .items
        .iter()
        .map(|item| {
            let mut value = json!(item);
            value["pinned"] = json!(pinned.iter().any(|pin| pin.id == item.id));
            value
        })
        .collect();
    Json(json!({
        "items": items,
        "labels": listing.labels,
        "hide": hidden,
        "partial": listing.partial,
        "labels_locked": listing.labels_locked,
        "sign_in": listing.sign_in,
    }))
    .into_response()
}

/// Show an installed game's folder in Explorer (`204`).
async fn open_folder(
    State(state): State<AppState>,
    Path((id, item)): Path<(String, String)>,
) -> Response {
    let Some((_, library)) = state.library_button(&id) else {
        return error(StatusCode::NOT_FOUND, "not_found", NOT_A_LIBRARY);
    };
    let launcher = state.launcher.clone();
    let opened = tokio::task::spawn_blocking(move || {
        let folder = library.folder(&item).ok_or(PressError::NotFound)?;
        launcher
            .open(&folder.to_string_lossy(), None, false)
            .map_err(PressError::Launch)
    })
    .await;
    match opened {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(failure)) => press_error(&id, failure, "the game is not installed"),
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &join.to_string(),
        ),
    }
}

#[derive(serde::Deserialize)]
struct LabelName {
    name: String,
}

/// The programs a game can be started with, and the one in use.
async fn item_programs(
    State(state): State<AppState>,
    Path((id, item)): Path<(String, String)>,
) -> Response {
    let Some((_, library)) = state.library_button(&id) else {
        return error(StatusCode::NOT_FOUND, "not_found", NOT_A_LIBRARY);
    };
    match tokio::task::spawn_blocking(move || library.programs(&item)).await {
        Ok(Some(programs)) => Json(programs).into_response(),
        Ok(None) => error(
            StatusCode::NOT_FOUND,
            "not_found",
            "the game has no programs to choose from",
        ),
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &join.to_string(),
        ),
    }
}

/// The default output's volume and mute, and the apps with sound on it:
/// `{"master": {volume, muted}, "apps": [{process, name, volume, muted}]}`.
async fn mixer(State(state): State<AppState>) -> Response {
    mixer_response(&state).await
}

async fn mixer_response(state: &AppState) -> Response {
    let audio = state.audio.clone();
    let read = tokio::task::spawn_blocking(move || {
        Ok::<_, crate::audio::AudioError>((audio.master()?, audio.apps()?))
    })
    .await;
    match read {
        Ok(Ok((master, apps))) => {
            // Volumes to two places, as the buttons give them.
            let apps: Vec<Value> = apps
                .iter()
                .map(|app| json!({ "process": app.process, "name": app.name, "volume": buttons::round2(app.volume), "muted": app.muted }))
                .collect();
            let master = json!({ "volume": buttons::round2(master.volume), "muted": master.muted });
            Json(json!({ "master": master, "apps": apps })).into_response()
        }
        Ok(Err(failure)) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "audio",
            &failure.to_string(),
        ),
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &join.to_string(),
        ),
    }
}

#[derive(serde::Deserialize)]
struct MasterChange {
    volume: Option<f32>,
    muted: Option<bool>,
}

#[derive(serde::Deserialize)]
struct AppChange {
    volume: Option<f32>,
    muted: Option<bool>,
}

fn valid_volume(volume: f32) -> bool {
    (0.0..=1.0).contains(&volume)
}

/// `{"volume"}` and/or `{"muted"}`; a volume alone also unmutes, as Windows' own
/// slider does. Answers the mixer.
async fn set_master(State(state): State<AppState>, Json(change): Json<MasterChange>) -> Response {
    if change.volume.is_some_and(|v| !valid_volume(v)) {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_volume",
            "volume must be between 0 and 1",
        );
    }
    let muted = change.muted.or(change.volume.map(|_| false));
    let audio = state.audio.clone();
    let set = tokio::task::spawn_blocking(move || audio.set_master(change.volume, muted)).await;
    match set {
        Ok(Ok(())) => {
            state.refresh().await;
            mixer_response(&state).await
        }
        Ok(Err(failure)) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "audio",
            &failure.to_string(),
        ),
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &join.to_string(),
        ),
    }
}

/// `{"volume"}`, `{"muted"}` or both for every session of a program; a volume alone
/// unmutes, as Windows' own slider does. Answers the mixer.
async fn set_app_volume(
    State(state): State<AppState>,
    Path(process): Path<String>,
    Json(change): Json<AppChange>,
) -> Response {
    if change.volume.is_some_and(|v| !valid_volume(v)) {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_volume",
            "volume must be between 0 and 1",
        );
    }
    let muted = change.muted.or(change.volume.map(|_| false));
    let Some(muted) = muted else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_change",
            "give the volume, muted or both",
        );
    };
    let audio = state.audio.clone();
    let set = tokio::task::spawn_blocking(move || {
        if let Some(volume) = change.volume {
            audio.set_app_volume(&process, volume)?;
        }
        audio.set_app_mute(&process, muted)
    })
    .await;
    match set {
        Ok(Ok(0)) => error(
            StatusCode::NOT_FOUND,
            "not_found",
            "the program has no sound now",
        ),
        Ok(Ok(_)) => {
            state.refresh().await;
            mixer_response(&state).await
        }
        Ok(Err(failure)) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "audio",
            &failure.to_string(),
        ),
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &join.to_string(),
        ),
    }
}

/// A game's license keys, read from its store (`{"keys": [{"label", "value"}]}`).
async fn license_keys(
    State(state): State<AppState>,
    Path((id, item)): Path<(String, String)>,
) -> Response {
    let Some((_, library)) = state.library_button(&id) else {
        return error(StatusCode::NOT_FOUND, "not_found", NOT_A_LIBRARY);
    };
    match tokio::task::spawn_blocking(move || library.license_keys(&item)).await {
        Ok(Ok(keys)) => Json(json!({ "keys": keys })).into_response(),
        Ok(Err(KeyError::NotFound)) => error(
            StatusCode::NOT_FOUND,
            "not_found",
            "the library does not know the game's license keys",
        ),
        Ok(Err(KeyError::Unavailable(reason))) => {
            error(StatusCode::CONFLICT, "keys_unavailable", &reason)
        }
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &join.to_string(),
        ),
    }
}

/// Bring a library button's games up to date, leaving out the labels it hides: Steam
/// answers how many games it updates and shows to install
/// (`{"updates", "installs"}`); a shop starts its round of downloads (`202`).
async fn update_library(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some((hide, library)) = state.library_button(&id) else {
        return error(StatusCode::NOT_FOUND, "not_found", NOT_A_LIBRARY);
    };
    match tokio::task::spawn_blocking(move || library.update(&hide)).await {
        Ok(Ok(Update::Steam { updates, installs })) => {
            Json(json!({ "updates": updates, "installs": installs })).into_response()
        }
        Ok(Ok(Update::Round)) => {
            (StatusCode::ACCEPTED, Json(json!({ "round": true }))).into_response()
        }
        Ok(Err(UpdateError::SignIn)) => error(
            StatusCode::CONFLICT,
            "sign_in",
            "sign in to the shop through the panel first",
        ),
        Ok(Err(UpdateError::Unavailable(reason))) => {
            error(StatusCode::CONFLICT, "update_unavailable", &reason)
        }
        Ok(Err(UpdateError::Failed(message))) => {
            warn!(button = %id, %message, "library update failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "update", &message)
        }
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &join.to_string(),
        ),
    }
}

#[derive(serde::Deserialize)]
struct ProgramChoice {
    program: String,
}

/// `{"program": …}`: remember which program starts the game (`204`).
async fn choose_program(
    State(state): State<AppState>,
    Path((id, item)): Path<(String, String)>,
    Json(body): Json<ProgramChoice>,
) -> Response {
    // The picture is the chosen program's icon.
    state
        .pictures
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&format!("{id}/{item}"));
    no_content(
        change_labels(&state, &id, move |library, _| {
            library.choose_program(&item, &body.program)
        })
        .await,
    )
}

fn label_error(failure: LabelError) -> Response {
    match failure {
        LabelError::NotFound => error(StatusCode::NOT_FOUND, "not_found", "no such label or game"),
        LabelError::Invalid(message) => error(StatusCode::BAD_REQUEST, "invalid_label", &message),
        LabelError::Unavailable(message) => {
            error(StatusCode::CONFLICT, "labels_unavailable", &message)
        }
        LabelError::Failed(message) => {
            warn!(%message, "cannot change a label");
            error(StatusCode::INTERNAL_SERVER_ERROR, "labels", &message)
        }
    }
}

/// Run a label change on the blocking pool with the current listing at hand.
async fn change_labels<T: Send + 'static>(
    state: &AppState,
    id: &str,
    change: impl FnOnce(&dyn GameLibrary, &library::Listing) -> Result<T, LabelError> + Send + 'static,
) -> Result<T, Response> {
    let Some((_, library)) = state.library_button(id) else {
        return Err(error(StatusCode::NOT_FOUND, "not_found", NOT_A_LIBRARY));
    };
    let changed = tokio::task::spawn_blocking(move || {
        let listing = library.listing();
        change(library.as_ref(), &listing)
    })
    .await;
    match changed {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(failure)) => Err(label_error(failure)),
        Err(join) => Err(error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &join.to_string(),
        )),
    }
}

/// A label the panel may rename or delete.
fn editable(listing: &library::Listing, label: &str) -> Result<(), LabelError> {
    match listing.labels.iter().find(|l| l.id == label) {
        None => Err(LabelError::NotFound),
        Some(l) if !l.editable => Err(LabelError::Invalid(format!(
            "{:?} is Steam's own label and cannot be renamed or deleted",
            l.name
        ))),
        Some(_) => Ok(()),
    }
}

/// `{"name": …}`: make a label; answers `201 {"label": …}`.
async fn create_label(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<LabelName>,
) -> Response {
    let created = change_labels(&state, &id, move |library, listing| {
        let name = library::check_name(&body.name, &listing.labels, None)?;
        library.create_label(&name)
    })
    .await;
    match created {
        Ok(label) => (StatusCode::CREATED, Json(json!({ "label": label }))).into_response(),
        Err(response) => response,
    }
}

/// `{"name": …}`: rename a label (`204`).
async fn rename_label(
    State(state): State<AppState>,
    Path((id, label)): Path<(String, String)>,
    Json(body): Json<LabelName>,
) -> Response {
    no_content(
        change_labels(&state, &id, move |library, listing| {
            editable(listing, &label)?;
            let name = library::check_name(&body.name, &listing.labels, Some(&label))?;
            library.rename_label(&label, &name)
        })
        .await,
    )
}

/// Delete a label; its games stay in the library (`204`).
async fn delete_label(
    State(state): State<AppState>,
    Path((id, label)): Path<(String, String)>,
) -> Response {
    no_content(
        change_labels(&state, &id, move |library, listing| {
            editable(listing, &label)?;
            library.delete_label(&label)
        })
        .await,
    )
}

async fn add_to_label(
    State(state): State<AppState>,
    Path((id, label, item)): Path<(String, String, String)>,
) -> Response {
    set_label(&state, &id, label, item, true).await
}

async fn remove_from_label(
    State(state): State<AppState>,
    Path((id, label, item)): Path<(String, String, String)>,
) -> Response {
    set_label(&state, &id, label, item, false).await
}

/// Put a game in a label or take it out (`204`).
async fn set_label(state: &AppState, id: &str, label: String, item: String, on: bool) -> Response {
    no_content(
        change_labels(state, id, move |library, listing| {
            if !listing.labels.iter().any(|l| l.id == label)
                || !listing.items.iter().any(|i| i.id == item)
            {
                return Err(LabelError::NotFound);
            }
            library.set_label(&label, &item, on)
        })
        .await,
    )
}

fn no_content(result: Result<(), Response>) -> Response {
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(response) => response,
    }
}

/// Start a library game, or bring it to the front when it runs (`204`).
async fn start_item(
    State(state): State<AppState>,
    Path((id, item)): Path<(String, String)>,
) -> Response {
    let Some((_, library)) = state.library_button(&id) else {
        return error(StatusCode::NOT_FOUND, "not_found", NOT_A_LIBRARY);
    };
    let launcher = state.launcher.clone();
    let started = tokio::task::spawn_blocking(move || {
        library::start(library.as_ref(), launcher.as_ref(), &item)
    })
    .await;
    match started {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(failure)) => press_error(&id, failure, "the library has no such game"),
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &join.to_string(),
        ),
    }
}

/// A library game's picture: its file, or a redirect to it on the web.
async fn item_picture(
    State(state): State<AppState>,
    Path((id, item)): Path<(String, String)>,
) -> Response {
    let Some((_, library)) = state.library_button(&id) else {
        return error(StatusCode::NOT_FOUND, "not_found", NOT_A_LIBRARY);
    };
    let key = format!("{id}/{item}");
    let cached = state
        .pictures
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
        .cloned();
    let read = match cached {
        Some(found) => Ok(Ok(Ok(found))),
        None => {
            tokio::task::spawn_blocking(move || match library.picture(&item) {
                Some(Picture::File(path)) => std::fs::read(&path)
                    .map(|bytes| Ok((Arc::new(bytes), "image/jpeg")))
                    .map_err(|err| format!("cannot read {}: {err}", path.display())),
                Some(Picture::Icon(program)) => {
                    icons::icon_png(&program).map(|png| Ok((Arc::new(png), "image/png")))
                }
                Some(Picture::Url(url)) => Ok(Err(url)),
                None => Err("the library has no such game".to_owned()),
            })
            .await
        }
    };
    match read {
        Ok(Ok(Ok((bytes, kind)))) => {
            state
                .pictures
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(key, (Arc::clone(&bytes), kind));
            (
                [
                    (axum::http::header::CONTENT_TYPE, kind),
                    (axum::http::header::CACHE_CONTROL, "no-cache"),
                ],
                bytes.as_ref().clone(),
            )
                .into_response()
        }
        Ok(Ok(Err(url))) => {
            (StatusCode::FOUND, [(axum::http::header::LOCATION, url)]).into_response()
        }
        Ok(Err(message)) => error(StatusCode::NOT_FOUND, "not_found", &message),
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &join.to_string(),
        ),
    }
}

/// Pin a library game to the button's tab; returns `{"button": …}`.
async fn pin_item(
    State(state): State<AppState>,
    Path((id, item)): Path<(String, String)>,
) -> Response {
    let Some((_, library)) = state.library_button(&id) else {
        return error(StatusCode::NOT_FOUND, "not_found", NOT_A_LIBRARY);
    };
    let pins = state.pins.clone();
    let button = id.clone();
    let pinned = tokio::task::spawn_blocking(move || {
        let found = library.listing().items.into_iter().find(|i| i.id == item)?;
        Some(pins.add(
            &button,
            &Pin {
                id: found.id,
                name: found.name,
            },
        ))
    })
    .await;
    match pinned {
        Ok(Some(Ok(()))) => button_response(&state, &id).await,
        Ok(None) => error(
            StatusCode::NOT_FOUND,
            "not_found",
            "the library has no such game",
        ),
        Ok(Some(Err(err))) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage",
            &err.to_string(),
        ),
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &join.to_string(),
        ),
    }
}

/// Take a game off the button's tab; returns `{"button": …}`.
async fn unpin_item(
    State(state): State<AppState>,
    Path((id, item)): Path<(String, String)>,
) -> Response {
    if state.library_button(&id).is_none() {
        return error(StatusCode::NOT_FOUND, "not_found", NOT_A_LIBRARY);
    }
    if let Err(err) = state.pins.remove(&id, &item) {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage",
            &err.to_string(),
        );
    }
    button_response(&state, &id).await
}

/// The button's Windows icon as PNG, read once and then kept.
async fn button_icon(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    // A picture on the web (a Discord server's icon among them) is the panel's to fetch.
    let server_icons = state.discord.icons();
    let url = state
        .config
        .buttons
        .iter()
        .find(|b| b.id == id)
        .and_then(|button| buttons::icon_url(button, &server_icons));
    if let Some(url) = url {
        return axum::response::Redirect::temporary(&url).into_response();
    }
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

    const LIBRARY: &str = "buttons:\n  - id: games\n    label: Games\n    type: steam.library\n    hide: [非表示]\n  - id: other\n    label: Other\n    type: app.launch\n    target: a.exe\n";

    fn library_state() -> (AppState, Arc<crate::launch::fake::FakeLauncher>) {
        let launcher = Arc::new(crate::launch::fake::FakeLauncher::default());
        let state = AppState::new(
            config::parse(LIBRARY).unwrap(),
            Arc::new(FakeAudio::new(Vec::new(), None)),
        )
        .with_launcher(launcher.clone())
        .with_library("games", Arc::new(crate::library::fake::FakeLibrary::new()));
        (state, launcher)
    }

    #[tokio::test]
    async fn a_library_button_lists_its_games_with_the_labels_it_hides() {
        let (state, _) = library_state();
        let (status, body) = call(state.clone(), "GET", "/buttons/games/library").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["items"][0]["id"], "1");
        assert_eq!(body["items"][0]["pinned"], false);
        assert_eq!(body["items"][1]["labels"], serde_json::json!(["hidden"]));
        assert_eq!(
            body["labels"],
            serde_json::json!([
                {"id": "hidden", "name": "非表示", "editable": false},
                {"id": "uc-1", "name": "outdate", "editable": true}
            ])
        );
        // `hide` names labels in the configuration; the answer gives their IDs.
        assert_eq!(body["hide"], serde_json::json!(["hidden"]));
        assert_eq!(body["partial"], Value::Null);
        assert_eq!(body["labels_locked"], Value::Null);

        let (status, _) = call(state, "GET", "/buttons/other/library").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn listing_a_library_reads_it_again_for_the_next_listing() {
        let library = Arc::new(crate::library::fake::FakeLibrary::new());
        let state = AppState::new(
            config::parse(LIBRARY).unwrap(),
            Arc::new(FakeAudio::new(Vec::new(), None)),
        )
        .with_library("games", library.clone());
        let (status, _) = call(state, "GET", "/buttons/games/library").await;
        assert_eq!(status, StatusCode::OK);
        // In the background, after the answer.
        for _ in 0..200 {
            if library.rereads.load(std::sync::atomic::Ordering::SeqCst) == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the library was not read again");
    }

    #[tokio::test]
    async fn pinning_shows_the_game_on_the_button_until_it_is_unpinned() {
        let (state, _) = library_state();
        let (status, body) = call(state.clone(), "PUT", "/buttons/games/pins/2").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["button"]["state"]["kind"], "library");
        assert_eq!(body["button"]["state"]["pins"][0]["name"], "Two");
        let (_, body) = call(state.clone(), "GET", "/buttons/games/library").await;
        assert_eq!(body["items"][1]["pinned"], true);
        assert_eq!(body["items"][0]["pinned"], false);

        let (status, _) = call(state.clone(), "PUT", "/buttons/games/pins/9").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, body) = call(state, "DELETE", "/buttons/games/pins/2").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["button"]["state"]["pins"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn a_library_picture_is_served_from_its_file_or_redirected_to_the_web() {
        let (state, _) = library_state();
        let get = |uri: &str| {
            app(state.clone()).oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        };
        let response = get("/buttons/games/library/1/image").await.unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(response.headers()["location"], "https://img/1.jpg");

        let response = get("/buttons/games/library/2/image").await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "image/jpeg");
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(body.starts_with(b"[package]"));

        let (status, _) = call(state.clone(), "GET", "/buttons/games/library/9/image").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(state, "GET", "/buttons/other/library/1/image").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    async fn send(state: AppState, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        let response = app(state)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn the_panels_fanza_sign_in_is_kept_and_starts_a_round() {
        use serde_json::json;

        let file = std::env::temp_dir().join(format!(
            "windows-link-fanza-session-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&file);
        let client = Arc::new(crate::fanza::Client::open(file.clone()));
        let wake = Arc::new(tokio::sync::Notify::new());
        let (plain, _) = state();
        let state = plain.clone().with_fanza(client.clone(), wake.clone());
        let cookie = |name: &str, domain: &str| {
            json!({ "name": name, "value": "v", "domain": domain, "path": "/",
                    "expires": 4_102_444_800_i64, "secure": true, "http_only": true })
        };

        // Without a login cookie nothing changes.
        let (status, body) = send(
            state.clone(),
            "PUT",
            "/fanza/session",
            json!({ "cookies": [cookie("guest_id", "dmm.co.jp")] }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_session");
        assert!(!client.signed_in());

        let (status, body) = send(
            state.clone(),
            "PUT",
            "/fanza/session",
            json!({ "cookies": [
                cookie("login_secure_id", ".dmm.co.jp"),
                cookie("laravel_session", "dlsoft.dmm.co.jp"),
                // Other sites' cookies stay out.
                cookie("tracker", ".example.com"),
            ] }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        // DMM's two and the age check FANZA asks for once.
        assert_eq!(body["kept"], 3);
        assert!(client.signed_in());
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .contains("login_secure_id")
        );
        // The FANZA round starts now rather than hours later.
        tokio::time::timeout(std::time::Duration::from_secs(1), wake.notified())
            .await
            .unwrap();
        let _ = std::fs::remove_file(&file);

        // Without a FANZA library there is nothing to sign in to.
        let (status, _) = send(plain, "PUT", "/fanza/session", json!({ "cookies": [] })).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn labels_are_created_renamed_filled_and_deleted() {
        use serde_json::json;

        let (state, _) = library_state();
        let (status, body) = send(
            state.clone(),
            "POST",
            "/buttons/games/labels",
            json!({"name": " RPG "}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(
            body["label"],
            json!({"id": "uc-3", "name": "RPG", "editable": true})
        );

        let (status, body) = send(
            state.clone(),
            "POST",
            "/buttons/games/labels",
            json!({"name": "rpg"}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_label");

        let (status, _) = call(state.clone(), "PUT", "/buttons/games/labels/uc-3/items/1").await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = send(
            state.clone(),
            "PATCH",
            "/buttons/games/labels/uc-3",
            json!({"name": "JRPG"}),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, body) = call(state.clone(), "GET", "/buttons/games/library").await;
        assert_eq!(body["items"][0]["labels"], json!(["uc-3"]));
        assert_eq!(body["labels"][2]["name"], "JRPG");

        let (status, _) = call(
            state.clone(),
            "DELETE",
            "/buttons/games/labels/uc-3/items/1",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(state.clone(), "DELETE", "/buttons/games/labels/uc-3").await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, body) = call(state.clone(), "DELETE", "/buttons/games/labels/uc-3").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "not_found");
        let (_, body) = call(state, "GET", "/buttons/games/library").await;
        assert_eq!(body["labels"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn fixed_labels_hold_games_but_cannot_be_renamed_or_deleted() {
        let (state, _) = library_state();
        let (status, _) = call(state.clone(), "PUT", "/buttons/games/labels/hidden/items/1").await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, body) = send(
            state.clone(),
            "PATCH",
            "/buttons/games/labels/hidden",
            serde_json::json!({"name": "x"}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_label");
        let (status, _) = call(state, "DELETE", "/buttons/games/labels/hidden").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn browsing_a_game_opens_its_folder() {
        let (state, launcher) = library_state();
        let (status, _) = call(state.clone(), "POST", "/buttons/games/library/1/folder").await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(*launcher.opened.lock().unwrap(), [r"C:\Games\One"]);
        let (status, body) = call(state, "POST", "/buttons/games/library/2/folder").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "not_found");
    }

    #[tokio::test]
    async fn a_games_license_keys_are_read_from_its_library() {
        let (state, _) = library_state();
        let (status, body) = call(state.clone(), "GET", "/buttons/games/library/1/keys").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            serde_json::json!({"keys": [{"label": "ライセンスキー", "value": "ABCD-1234-EFGH-5678"}]})
        );
        let (_, body) = call(state.clone(), "GET", "/buttons/games/library/2/keys").await;
        assert_eq!(body, serde_json::json!({"keys": []}));
        let (status, body) = call(state.clone(), "GET", "/buttons/games/library/5/keys").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "keys_unavailable");
        assert_eq!(body["message"], "no account");
        let (status, body) = call(state.clone(), "GET", "/buttons/games/library/9/keys").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "not_found");
        let (status, _) = call(state, "GET", "/buttons/nope/library/1/keys").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_discord_servers_icon_is_a_redirect_to_its_picture() {
        use crate::discord::{ServerIcons, fake::FakeDiscord};

        let picture = "https://cdn.discordapp.com/icons/1533/d08.webp?size=128";
        let state = AppState::new(
            config::parse(
                "buttons:\n  - id: uf4\n    label: UF4\n    type: discord.server\n    guild_id: 1533\n",
            )
            .unwrap(),
            Arc::new(FakeAudio::new(Vec::new(), None)),
        )
        .with_discord(Arc::new(FakeDiscord(ServerIcons::from([(
            "1533".to_owned(),
            picture.to_owned(),
        )]))));
        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri("/buttons/uf4/icon")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_redirection());
        assert_eq!(response.headers()["location"], picture);
    }

    #[tokio::test]
    async fn an_icon_on_the_web_is_a_redirect_to_it() {
        let state = AppState::new(
            config::parse(
                "buttons:\n  - id: dlsite\n    label: DLsite\n    type: dlsite.library\n",
            )
            .unwrap(),
            Arc::new(FakeAudio::new(Vec::new(), None)),
        );
        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri("/buttons/dlsite/icon")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_redirection());
        assert_eq!(response.headers()["location"], crate::buttons::DLSITE_ICON);
    }

    #[tokio::test]
    async fn the_mixer_reads_and_sets_the_output_and_each_apps_volume() {
        use serde_json::json;

        let (state, audio) = state();
        audio.set_volume("discord.exe", 0.8);
        let (status, body) = call(state.clone(), "GET", "/audio/mixer").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({
                "master": {"volume": 0.5, "muted": false},
                "apps": [
                    {"process": "discord.exe", "name": "discord", "volume": 0.8, "muted": false},
                    {"process": "streetfighter6.exe", "name": "streetfighter6", "volume": 1.0, "muted": false}
                ]
            })
        );

        // Muting, then moving the volume, which unmutes as Windows' own slider does.
        let (status, body) = send(
            state.clone(),
            "PUT",
            "/audio/master",
            json!({"muted": true}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["master"], json!({"volume": 0.5, "muted": true}));
        let (_, body) = send(
            state.clone(),
            "PUT",
            "/audio/master",
            json!({"volume": 0.25}),
        )
        .await;
        assert_eq!(body["master"], json!({"volume": 0.25, "muted": false}));
        let (status, body) = send(
            state.clone(),
            "PUT",
            "/audio/master",
            json!({"volume": 1.5}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_volume");

        let (status, body) = send(
            state.clone(),
            "PUT",
            "/audio/apps/Discord.exe",
            json!({"volume": 0.4}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["apps"][0]["volume"], json!(0.4));
        let (status, body) = send(
            state.clone(),
            "PUT",
            "/audio/apps/gone.exe",
            json!({"volume": 0.4}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "not_found");

        // An app is muted by itself, keeping its volume, and its volume unmutes it.
        let (status, body) = send(
            state.clone(),
            "PUT",
            "/audio/apps/Discord.exe",
            json!({"muted": true}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["apps"][0],
            json!({"process": "discord.exe", "name": "discord", "volume": 0.4, "muted": true})
        );
        assert_eq!(body["apps"][1]["muted"], false);
        let (_, body) = send(
            state.clone(),
            "PUT",
            "/audio/apps/Discord.exe",
            json!({"volume": 0.5}),
        )
        .await;
        assert_eq!(body["apps"][0]["muted"], false);
        let (status, body) = send(
            state.clone(),
            "PUT",
            "/audio/apps/gone.exe",
            json!({"muted": true}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "not_found");
        let (status, body) = send(state, "PUT", "/audio/apps/Discord.exe", json!({})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_change");
    }

    #[tokio::test]
    async fn a_game_still_downloading_says_how_it_is_going() {
        let (state, _) = library_state();
        let (status, body) = call(state, "POST", "/buttons/games/library/6/start").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "not_downloaded");
        assert_eq!(body["message"], "ダウンロード中 40%");
    }

    #[tokio::test]
    async fn a_game_with_several_programs_asks_once_which_one() {
        use serde_json::json;

        let (state, _) = library_state();
        let (status, body) = call(state.clone(), "POST", "/buttons/games/library/9/start").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "choose_program");
        let (status, body) = call(state.clone(), "GET", "/buttons/games/library/9/programs").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({"candidates": ["a.exe", "b.exe"], "chosen": null})
        );

        let (status, _) = send(
            state.clone(),
            "PUT",
            "/buttons/games/library/9/program",
            json!({"program": "b.exe"}),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, body) = call(state.clone(), "GET", "/buttons/games/library/9/programs").await;
        assert_eq!(body["chosen"], "b.exe");
        let (status, body) = send(
            state.clone(),
            "PUT",
            "/buttons/games/library/9/program",
            json!({"program": "c.exe"}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_label");
        let (status, _) = call(state, "GET", "/buttons/games/library/1/programs").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn each_library_button_has_its_own_library() {
        let config = config::parse(
            "buttons:\n  - id: steam\n    label: Steam\n    type: steam.library\n  - id: dlsite\n    label: DLsite\n    type: dlsite.library\n",
        )
        .unwrap();
        let state = AppState::new(config, Arc::new(FakeAudio::new(Vec::new(), None)))
            .with_library("dlsite", Arc::new(crate::library::fake::FakeLibrary::new()));
        let (_, body) = call(state.clone(), "GET", "/buttons/dlsite/library").await;
        assert_eq!(body["items"].as_array().unwrap().len(), 2);
        // A library button without a library says so instead of failing.
        let (status, body) = call(state, "GET", "/buttons/steam/library").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body["partial"]
                .as_str()
                .unwrap()
                .contains("no game library")
        );
    }

    #[tokio::test]
    async fn updating_a_library_says_what_it_started_or_why_not() {
        use crate::library::{Update, UpdateError, fake::FakeLibrary};

        let library = Arc::new(FakeLibrary::new());
        *library.updates.lock().unwrap() = vec![
            Ok(Update::Steam {
                updates: 0,
                installs: 2,
            }),
            Ok(Update::Round),
            Err(UpdateError::SignIn),
            Err(UpdateError::Unavailable("Steam is not running".into())),
            Err(UpdateError::Failed("TypeError: boom".into())),
        ];
        let (state, _) = library_state();
        let state = state.with_library("games", library.clone());
        let update = || call(state.clone(), "POST", "/buttons/games/library/update");

        let (status, body) = update().await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"updates": 0, "installs": 2}));
        // The labels the button hides are left out.
        assert_eq!(*library.hidden.lock().unwrap(), [["非表示"]]);
        let (status, body) = update().await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body, serde_json::json!({"round": true}));
        let (status, body) = update().await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "sign_in");
        let (status, body) = update().await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "update_unavailable");
        assert_eq!(body["message"], "Steam is not running");
        let (status, body) = update().await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"], "update");

        let (status, body) = call(state.clone(), "POST", "/buttons/other/library/update").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "not_found");
    }

    #[tokio::test]
    async fn starting_a_library_game_opens_it() {
        let (state, launcher) = library_state();
        let (status, _) = call(state.clone(), "POST", "/buttons/games/library/1/start").await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(*launcher.opened.lock().unwrap(), ["game://run/1"]);
        let (status, body) = call(state.clone(), "POST", "/buttons/games/library/7/start").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "not_found");
        let (status, body) = call(state, "POST", "/buttons/games/press").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "not_pressable");
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
