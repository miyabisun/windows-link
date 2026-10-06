//! The resident connection to Discord: kept open while Discord runs, reopened when it
//! restarts, and used to read the servers' icons for the `discord.server` buttons.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::json;
use tokio::{sync::Notify, time::sleep};
use tracing::{info, warn};

use super::{
    Discord, ServerIcons,
    client::{Client, sign_in},
    guild_icons,
};
use crate::secrets::DiscordApp;

/// How often to look for Discord while it is not running.
const RETRY: Duration = Duration::from_secs(5);
/// How often the icons are read again while Discord runs.
const REFRESH: Duration = Duration::from_hours(1);

pub struct DiscordIcons {
    icons: Mutex<ServerIcons>,
    changes: Arc<Notify>,
}

impl Discord for DiscordIcons {
    fn icons(&self) -> ServerIcons {
        self.icons
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl DiscordIcons {
    /// Start the connection in the current tokio runtime. `changes` is notified whenever
    /// the icons change.
    pub fn start(app: DiscordApp, token_path: PathBuf, changes: Arc<Notify>) -> Arc<Self> {
        let discord = Arc::new(Self {
            icons: Mutex::new(ServerIcons::new()),
            changes,
        });
        tokio::spawn(run(discord.clone(), app, token_path));
        discord
    }

    fn set(&self, icons: ServerIcons) {
        let mut current = self
            .icons
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *current != icons {
            info!(servers = icons.len(), "read the Discord server icons");
            *current = icons;
            self.changes.notify_one();
        }
    }
}

/// Connect and sign in without asking: `windows-link discord-servers` asks once.
async fn open(app: &DiscordApp, path: &Path) -> Result<Client, String> {
    let client = Client::connect(&app.client_id)
        .await
        .map_err(|err| err.to_string())?;
    sign_in(&client, app, path, false)
        .await
        .map_err(|err| err.to_string())?;
    Ok(client)
}

async fn run(discord: Arc<DiscordIcons>, app: DiscordApp, path: PathBuf) {
    let mut last_failure = None;
    loop {
        match open(&app, &path).await {
            Ok(client) => {
                last_failure = None;
                session(client, &discord).await;
            }
            Err(reason) => {
                // Discord is often not running yet: say each reason once.
                if last_failure.as_ref() != Some(&reason) {
                    info!(%reason, "cannot read the Discord server icons yet");
                    last_failure = Some(reason);
                }
            }
        }
        sleep(RETRY).await;
    }
}

/// Read the icons now and every `REFRESH` until Discord closes the connection.
async fn session(mut client: Client, discord: &DiscordIcons) {
    loop {
        match client.command("GET_GUILDS", json!({})).await {
            Ok(data) => discord.set(guild_icons(&data)),
            Err(err) => {
                warn!(%err, "cannot read the Discord servers");
                return;
            }
        }
        let refresh = sleep(REFRESH);
        tokio::pin!(refresh);
        loop {
            tokio::select! {
                event = client.next_event() => if event.is_none() { return },
                () = &mut refresh => break,
            }
        }
    }
}
