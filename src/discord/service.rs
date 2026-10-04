//! The resident connection to Discord: kept open while Discord runs, reopened when it
//! restarts, and used by the `discord.voice` buttons.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::json;
use tokio::{
    sync::{Notify, mpsc, oneshot},
    time::{Instant, sleep},
};
use tracing::{info, warn};

use super::{
    Voice, VoiceError, VoiceStatus,
    client::{Client, CommandError, ConnectError, SignInError, sign_in},
    proto, token,
};
use crate::secrets::DiscordApp;

/// How often to look for Discord while it is not running.
const RETRY: Duration = Duration::from_secs(5);
/// How long a press waits for Discord to start.
const START_TIMEOUT: Duration = Duration::from_mins(1);
const SELECT_TIMEOUT: Duration = Duration::from_secs(15);

struct Request {
    channel_id: String,
    reply: oneshot::Sender<Result<(), VoiceError>>,
}

pub struct DiscordVoice {
    status: Mutex<VoiceStatus>,
    changes: Arc<Notify>,
    requests: mpsc::Sender<Request>,
}

impl Voice for DiscordVoice {
    fn status(&self) -> VoiceStatus {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn toggle(&self, channel_id: &str) -> Result<(), VoiceError> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .blocking_send(Request {
                channel_id: channel_id.to_owned(),
                reply,
            })
            .map_err(|_| VoiceError::Failed("the Discord connection has stopped".into()))?;
        answer.blocking_recv().unwrap_or_else(|_| {
            Err(VoiceError::Failed(
                "the Discord connection has stopped".into(),
            ))
        })
    }
}

impl DiscordVoice {
    /// Start the connection in the current tokio runtime. `changes` is notified whenever
    /// the status changes.
    pub fn start(app: DiscordApp, token_path: PathBuf, changes: Arc<Notify>) -> Arc<Self> {
        let (requests, inbox) = mpsc::channel(8);
        let voice = Arc::new(Self {
            status: Mutex::new(VoiceStatus::unavailable("connecting to Discord")),
            changes,
            requests,
        });
        tokio::spawn(run(voice.clone(), app, token_path, inbox));
        voice
    }

    fn set(&self, status: VoiceStatus) {
        let mut current = self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *current != status {
            *current = status;
            self.changes.notify_one();
        }
    }

    fn set_selected(&self, selected: Option<String>) {
        self.set(VoiceStatus {
            available: true,
            reason: None,
            selected,
        });
    }
}

fn unavailable(error: &SignInError) -> VoiceError {
    match error {
        SignInError::Rejected => VoiceError::Rejected,
        other => VoiceError::Unavailable(other.to_string()),
    }
}

async fn open(app: &DiscordApp, path: &Path, may_ask: bool) -> Result<Client, VoiceError> {
    let client = Client::connect(&app.client_id)
        .await
        .map_err(|err| VoiceError::Unavailable(err.to_string()))?;
    sign_in(&client, app, path, may_ask)
        .await
        .map_err(|err| unavailable(&err))?;
    Ok(client)
}

/// Start Discord minimized (in the tray) the way its own shortcut does.
fn launch_discord() -> Result<(), String> {
    let base = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
    let update = PathBuf::from(base).join("Discord").join("Update.exe");
    if !update.exists() {
        return Err(format!(
            "Discord is not installed ({} is missing)",
            update.display()
        ));
    }
    std::process::Command::new(update)
        .args([
            "--processStart",
            "Discord.exe",
            "--process-start-args",
            "--start-minimized",
        ])
        .spawn()
        .map(drop)
        .map_err(|err| format!("cannot start Discord: {err}"))
}

/// A connection for a press: starts Discord when needed and may ask for approval.
async fn open_for_press(app: &DiscordApp, path: &Path) -> Result<Client, VoiceError> {
    let client = match Client::connect(&app.client_id).await {
        Ok(client) => client,
        Err(ConnectError::NotRunning) => {
            info!("starting Discord for a press");
            launch_discord().map_err(VoiceError::Unavailable)?;
            let deadline = Instant::now() + START_TIMEOUT;
            loop {
                sleep(Duration::from_secs(1)).await;
                match Client::connect(&app.client_id).await {
                    Ok(client) => break client,
                    Err(_) if Instant::now() < deadline => {}
                    Err(err) => {
                        return Err(VoiceError::Unavailable(format!(
                            "Discord did not start in time: {err}"
                        )));
                    }
                }
            }
        }
        Err(err) => return Err(VoiceError::Unavailable(err.to_string())),
    };
    sign_in(&client, app, path, true)
        .await
        .map_err(|err| unavailable(&err))?;
    Ok(client)
}

async fn run(
    voice: Arc<DiscordVoice>,
    app: DiscordApp,
    path: PathBuf,
    mut inbox: mpsc::Receiver<Request>,
) {
    loop {
        // Without a token there is nothing to do until a press (which may ask) or
        // `windows-link discord-channels` saves one.
        if token::load(&path).is_none() {
            voice.set(VoiceStatus::unavailable(
                SignInError::NeedsApproval.to_string(),
            ));
        } else {
            match open(&app, &path, false).await {
                Ok(client) => {
                    session(client, &voice, &mut inbox, None).await;
                    voice.set(VoiceStatus::unavailable("Discord closed the connection"));
                    continue;
                }
                Err(err) => voice.set(VoiceStatus::unavailable(err.to_string())),
            }
        }
        tokio::select! {
            () = sleep(RETRY) => {}
            request = inbox.recv() => {
                let Some(request) = request else { return };
                match open_for_press(&app, &path).await {
                    Ok(client) => session(client, &voice, &mut inbox, Some(request)).await,
                    Err(err) => {
                        warn!(%err, "cannot reach Discord for a press");
                        let _ = request.reply.send(Err(err));
                    }
                }
            }
        }
    }
}

async fn session(
    mut client: Client,
    voice: &DiscordVoice,
    inbox: &mut mpsc::Receiver<Request>,
    first: Option<Request>,
) {
    let selected = match client
        .command("GET_SELECTED_VOICE_CHANNEL", json!({}))
        .await
    {
        Ok(data) => proto::selected_channel(&data),
        Err(err) => {
            warn!(%err, "cannot read the current voice channel");
            if let Some(request) = first {
                let _ = request.reply.send(Err(VoiceError::Failed(err.to_string())));
            }
            return;
        }
    };
    if let Err(err) = client.subscribe("VOICE_CHANNEL_SELECT").await {
        warn!(%err, "cannot follow voice channel changes");
    }
    info!("connected to Discord");
    voice.set_selected(selected);
    if let Some(request) = first {
        select(&client, voice, request).await;
    }
    loop {
        tokio::select! {
            event = client.next_event() => match event {
                None => return,
                Some((evt, data)) if evt == "VOICE_CHANNEL_SELECT" => {
                    voice.set_selected(proto::selected_channel(&data));
                }
                Some(_) => {}
            },
            request = inbox.recv() => match request {
                None => return,
                Some(request) => select(&client, voice, request).await,
            },
        }
    }
}

async fn select(client: &Client, voice: &DiscordVoice, request: Request) {
    let current = voice.status().selected;
    let target = proto::toggle_target(current.as_deref(), &request.channel_id);
    let result = client
        .command_with(
            "SELECT_VOICE_CHANNEL",
            proto::select_voice_channel(target),
            None,
            SELECT_TIMEOUT,
        )
        .await;
    let reply = match result {
        Ok(data) => {
            voice.set_selected(proto::selected_channel(&data));
            Ok(())
        }
        Err(CommandError::Disconnected) => Err(VoiceError::Unavailable(
            "Discord closed the connection".into(),
        )),
        Err(err) => Err(VoiceError::Failed(err.to_string())),
    };
    let _ = request.reply.send(reply);
}
