//! Discord voice channels through the desktop app's local RPC: which channel the user is
//! in, and joining or leaving one.

pub mod client;
pub mod proto;
pub mod service;
pub mod token;

use std::fmt::Write as _;

use serde_json::Value;

/// What the `discord.voice` buttons show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VoiceStatus {
    /// Connected and signed in to Discord.
    pub available: bool,
    /// Why not, when unavailable.
    pub reason: Option<String>,
    /// The voice channel the user is in.
    pub selected: Option<String>,
}

impl VoiceStatus {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            selected: None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum VoiceError {
    /// Discord cannot be reached or used right now (409).
    Unavailable(String),
    /// The user turned down Discord's authorization dialog (409).
    Rejected,
    Failed(String),
}

impl std::fmt::Display for VoiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(message) | Self::Failed(message) => f.write_str(message),
            Self::Rejected => f.write_str("the authorization was turned down in Discord"),
        }
    }
}

pub trait Voice: Send + Sync + 'static {
    fn status(&self) -> VoiceStatus;
    /// Join `channel_id`, or leave it when already in it. Blocking: may start Discord
    /// and wait for the user to approve windows-link the first time.
    fn toggle(&self, channel_id: &str) -> Result<(), VoiceError>;
}

/// Used when no button needs Discord, or Discord cannot be set up.
pub struct NoVoice(pub String);

impl Voice for NoVoice {
    fn status(&self) -> VoiceStatus {
        VoiceStatus::unavailable(self.0.clone())
    }

    fn toggle(&self, _: &str) -> Result<(), VoiceError> {
        Err(VoiceError::Unavailable(self.0.clone()))
    }
}

/// Discord channel types that are voice channels (voice and stage).
const VOICE_TYPES: [i64; 2] = [2, 13];

/// `windows-link discord-channels` output: each server with its voice channels.
pub fn channel_listing(guilds: &[(Value, Value)]) -> String {
    let mut out = String::new();
    for (guild, channels) in guilds {
        let voice: Vec<&Value> = channels["channels"]
            .as_array()
            .map(|all| {
                all.iter()
                    .filter(|c| VOICE_TYPES.contains(&c["type"].as_i64().unwrap_or(-1)))
                    .collect()
            })
            .unwrap_or_default();
        if voice.is_empty() {
            continue;
        }
        out.push_str(guild["name"].as_str().unwrap_or("?"));
        out.push('\n');
        for channel in voice {
            let _ = writeln!(
                out,
                "  {}  {}",
                channel["id"].as_str().unwrap_or("?"),
                channel["name"].as_str().unwrap_or("?")
            );
        }
    }
    out
}

#[cfg(test)]
pub mod fake {
    use std::sync::Mutex;

    use super::{Voice, VoiceError, VoiceStatus, proto::toggle_target};

    /// In-memory Discord: `available: false` makes presses fail as unavailable.
    pub struct FakeVoice {
        pub status: Mutex<VoiceStatus>,
        pub presses: Mutex<Vec<String>>,
    }

    impl FakeVoice {
        pub fn new(available: bool, selected: Option<&str>) -> Self {
            Self {
                status: Mutex::new(VoiceStatus {
                    available,
                    reason: (!available).then(|| "Discord is not running".to_owned()),
                    selected: selected.map(str::to_owned),
                }),
                presses: Mutex::new(Vec::new()),
            }
        }
    }

    impl Voice for FakeVoice {
        fn status(&self) -> VoiceStatus {
            self.status.lock().unwrap().clone()
        }

        fn toggle(&self, channel_id: &str) -> Result<(), VoiceError> {
            self.presses.lock().unwrap().push(channel_id.to_owned());
            let mut status = self.status.lock().unwrap();
            if !status.available {
                return Err(VoiceError::Unavailable(
                    status.reason.clone().unwrap_or_default(),
                ));
            }
            status.selected =
                toggle_target(status.selected.as_deref(), channel_id).map(str::to_owned);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::channel_listing;

    #[test]
    fn lists_voice_and_stage_channels_by_server_and_skips_the_rest() {
        let guilds = vec![
            (
                json!({ "id": "1", "name": "APEX部" }),
                json!({ "channels": [
                    { "id": "11", "name": "雑談", "type": 0 },
                    { "id": "12", "name": "ランク", "type": 2 },
                    { "id": "13", "name": "大会", "type": 13 },
                ] }),
            ),
            (
                json!({ "id": "2", "name": "文字だけ" }),
                json!({ "channels": [{ "id": "21", "name": "general", "type": 0 }] }),
            ),
        ];
        assert_eq!(
            channel_listing(&guilds),
            "APEX部\n  12  ランク\n  13  大会\n"
        );
    }
}
