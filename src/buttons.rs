//! Button state and press logic, independent of HTTP and of the audio backend.

use serde::Serialize;

use crate::{
    audio::{Audio, AudioError, Device},
    config::{ButtonConfig, ButtonSpec, Config},
    discord::{Voice, VoiceError, VoiceStatus},
};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OutputOption {
    pub alias: String,
    pub name: Option<String>,
    pub connected: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ButtonState {
    /// `current` is the configured alias of the default output, or `None` when the
    /// default is some other device (`current_name` still names it).
    Output {
        current: Option<String>,
        current_name: Option<String>,
        options: Vec<OutputOption>,
    },
    /// `volume` is `None` while the process has no audio session.
    Volume {
        running: bool,
        volume: Option<f64>,
        levels: [f64; 2],
    },
    /// A Discord voice channel: `available` is false while Discord cannot be reached
    /// (`reason` says why); a press still tries, starting Discord if needed.
    Voice {
        available: bool,
        joined: bool,
        reason: Option<String>,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ButtonView {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub label: String,
    pub desktop: Option<String>,
    pub state: ButtonState,
}

#[derive(Debug, PartialEq)]
pub enum PressError {
    NotFound,
    /// The press cannot be carried out in the current situation (409).
    Conflict {
        code: &'static str,
        message: String,
    },
    Audio(AudioError),
    Discord(String),
}

/// Default-output target after a press: the second device when the first is the
/// default, otherwise the first.
pub fn next_output<'a>(current: Option<&str>, first: &'a str, second: &'a str) -> &'a str {
    if current == Some(first) {
        second
    } else {
        first
    }
}

/// Volume after a press: the lower level when above the midpoint, otherwise the higher.
pub fn next_volume(current: f32, levels: [f32; 2]) -> f32 {
    let (low, high) = if levels[0] <= levels[1] {
        (levels[0], levels[1])
    } else {
        (levels[1], levels[0])
    };
    if current > f32::midpoint(low, high) {
        low
    } else {
        high
    }
}

fn round2(value: f32) -> f64 {
    (f64::from(value) * 100.0).round() / 100.0
}

/// Snapshot of what the output buttons need, read once per refresh.
pub struct AudioSnapshot {
    pub devices: Vec<Device>,
    pub default_output: Option<String>,
}

impl AudioSnapshot {
    pub fn read(audio: &dyn Audio) -> Result<Self, AudioError> {
        Ok(Self {
            devices: audio.devices()?,
            default_output: audio.default_output()?,
        })
    }
}

pub fn view(
    config: &Config,
    button: &ButtonConfig,
    snapshot: &AudioSnapshot,
    audio: &dyn Audio,
    voice: &VoiceStatus,
) -> ButtonView {
    ButtonView {
        id: button.id.clone(),
        kind: button.spec.type_name(),
        label: button.label.clone(),
        desktop: button.desktop.clone(),
        state: state(config, &button.spec, snapshot, audio, voice),
    }
}

fn state(
    config: &Config,
    spec: &ButtonSpec,
    snapshot: &AudioSnapshot,
    audio: &dyn Audio,
    voice: &VoiceStatus,
) -> ButtonState {
    match spec {
        ButtonSpec::OutputToggle { devices } => {
            let options = devices
                .iter()
                .map(|alias| {
                    let device = config
                        .devices
                        .get(alias)
                        .and_then(|id| snapshot.devices.iter().find(|d| &d.id == id));
                    OutputOption {
                        alias: alias.clone(),
                        name: device.map(|d| d.name.clone()),
                        connected: device.is_some_and(|d| d.connected),
                    }
                })
                .collect();
            let default = snapshot.default_output.as_deref();
            let current = devices
                .iter()
                .find(|alias| config.devices.get(*alias).map(String::as_str) == default)
                .cloned();
            let current_name = default.and_then(|id| {
                snapshot
                    .devices
                    .iter()
                    .find(|d| d.id == id)
                    .map(|d| d.name.clone())
            });
            ButtonState::Output {
                current,
                current_name,
                options,
            }
        }
        ButtonSpec::AppVolumeToggle { process, levels } => match audio.app_volume(process) {
            Ok(volume) => ButtonState::Volume {
                running: volume.is_some(),
                volume: volume.map(round2),
                levels: [round2(levels[0]), round2(levels[1])],
            },
            Err(error) => ButtonState::Error {
                message: error.to_string(),
            },
        },
        ButtonSpec::DiscordVoice { channel_id } => ButtonState::Voice {
            available: voice.available,
            joined: voice.available && voice.selected.as_deref() == Some(channel_id.as_str()),
            reason: voice.reason.clone(),
        },
    }
}

pub fn press(
    config: &Config,
    id: &str,
    audio: &dyn Audio,
    voice: &dyn Voice,
) -> Result<(), PressError> {
    let button = config
        .buttons
        .iter()
        .find(|b| b.id == id)
        .ok_or(PressError::NotFound)?;
    match &button.spec {
        ButtonSpec::OutputToggle { devices } => {
            let first = &config.devices[&devices[0]];
            let second = &config.devices[&devices[1]];
            let current = audio.default_output().map_err(PressError::Audio)?;
            let target = next_output(current.as_deref(), first, second);
            let known = audio.devices().map_err(PressError::Audio)?;
            let device = known.iter().find(|d| d.id == target);
            if !device.is_some_and(|d| d.connected) {
                let alias = if target == first {
                    &devices[0]
                } else {
                    &devices[1]
                };
                let name = device.map_or(alias.as_str(), |d| d.name.as_str());
                return Err(PressError::Conflict {
                    code: "device_unavailable",
                    message: format!("{name} is not connected"),
                });
            }
            audio.set_default_output(target).map_err(PressError::Audio)
        }
        ButtonSpec::AppVolumeToggle { process, levels } => {
            let Some(current) = audio.app_volume(process).map_err(PressError::Audio)? else {
                return Err(PressError::Conflict {
                    code: "not_running",
                    message: format!("{process} has no audio session"),
                });
            };
            audio
                .set_app_volume(process, next_volume(current, *levels))
                .map_err(PressError::Audio)?;
            Ok(())
        }
        ButtonSpec::DiscordVoice { channel_id } => {
            voice.toggle(channel_id).map_err(|error| match error {
                VoiceError::Unavailable(message) => PressError::Conflict {
                    code: "discord_unavailable",
                    message,
                },
                VoiceError::Rejected => PressError::Conflict {
                    code: "discord_rejected",
                    message: error.to_string(),
                },
                VoiceError::Failed(message) => PressError::Discord(message),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ButtonState, PressError, next_output, next_volume, press, view};
    use crate::{
        audio::{Audio, Device, fake::FakeAudio},
        buttons::AudioSnapshot,
        config,
        discord::{Voice, VoiceStatus, fake::FakeVoice},
    };

    fn no_voice() -> VoiceStatus {
        VoiceStatus::unavailable("no Discord buttons")
    }

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
  - id: vc-apex
    label: APEX
    type: discord.voice
    channel_id: 111
  - id: vc-sf6
    label: SF6 VC
    type: discord.voice
    channel_id: 222
";

    fn devices() -> Vec<Device> {
        vec![
            Device {
                id: "id-motu".into(),
                name: "MOTU M Series".into(),
                connected: true,
            },
            Device {
                id: "id-jbl".into(),
                name: "JBL Tour Pro 3".into(),
                connected: true,
            },
            Device {
                id: "id-hdmi".into(),
                name: "HDMI".into(),
                connected: true,
            },
        ]
    }

    #[test]
    fn output_toggles_between_the_two_and_falls_back_to_the_first() {
        assert_eq!(next_output(Some("a"), "a", "b"), "b");
        assert_eq!(next_output(Some("b"), "a", "b"), "a");
        assert_eq!(next_output(Some("other"), "a", "b"), "a");
        assert_eq!(next_output(None, "a", "b"), "a");
    }

    #[test]
    fn volume_toggles_around_the_midpoint_in_either_level_order() {
        assert!((next_volume(1.0, [0.2, 1.0]) - 0.2).abs() < f32::EPSILON);
        assert!((next_volume(0.2, [0.2, 1.0]) - 1.0).abs() < f32::EPSILON);
        assert!((next_volume(0.61, [1.0, 0.2]) - 0.2).abs() < f32::EPSILON);
        assert!((next_volume(0.6, [0.2, 1.0]) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn output_press_switches_the_default_and_the_state_follows() {
        let config = config::parse(CONFIG).unwrap();
        let audio = FakeAudio::new(devices(), Some("id-motu"));
        press(&config, "output", &audio, &FakeVoice::new(false, None)).unwrap();
        assert_eq!(audio.default_output().unwrap().as_deref(), Some("id-jbl"));

        let snapshot = AudioSnapshot::read(&audio).unwrap();
        let view = view(&config, &config.buttons[0], &snapshot, &audio, &no_voice());
        assert!(matches!(
            view.state,
            ButtonState::Output { current: Some(ref a), current_name: Some(ref n), .. }
                if a == "jbl" && n == "JBL Tour Pro 3"
        ));
    }

    #[test]
    fn output_press_to_a_disconnected_device_is_a_conflict() {
        let config = config::parse(CONFIG).unwrap();
        let audio = FakeAudio::new(devices(), Some("id-motu"));
        audio.set_connected("id-jbl", false);
        let error = press(&config, "output", &audio, &FakeVoice::new(false, None)).unwrap_err();
        assert!(matches!(
            error,
            PressError::Conflict { code: "device_unavailable", ref message } if message.contains("JBL")
        ));
        assert_eq!(audio.default_output().unwrap().as_deref(), Some("id-motu"));
    }

    #[test]
    fn other_default_device_has_no_current_alias_but_keeps_its_name() {
        let config = config::parse(CONFIG).unwrap();
        let audio = FakeAudio::new(devices(), Some("id-hdmi"));
        let snapshot = AudioSnapshot::read(&audio).unwrap();
        let view = view(&config, &config.buttons[0], &snapshot, &audio, &no_voice());
        assert!(matches!(
            view.state,
            ButtonState::Output { current: None, current_name: Some(ref n), .. } if n == "HDMI"
        ));
    }

    #[test]
    fn volume_press_toggles_and_reports_rounded_state() {
        let config = config::parse(CONFIG).unwrap();
        let audio = FakeAudio::new(devices(), None);
        audio.set_volume("streetfighter6.exe", 1.0);
        press(&config, "sf6", &audio, &FakeVoice::new(false, None)).unwrap();
        let snapshot = AudioSnapshot::read(&audio).unwrap();
        let view = view(&config, &config.buttons[1], &snapshot, &audio, &no_voice());
        assert_eq!(
            view.state,
            ButtonState::Volume {
                running: true,
                volume: Some(0.2),
                levels: [0.2, 1.0],
            }
        );
    }

    #[test]
    fn volume_without_a_session_is_not_running() {
        let config = config::parse(CONFIG).unwrap();
        let audio = FakeAudio::new(devices(), None);
        let snapshot = AudioSnapshot::read(&audio).unwrap();
        let view = view(&config, &config.buttons[1], &snapshot, &audio, &no_voice());
        assert!(matches!(
            view.state,
            ButtonState::Volume {
                running: false,
                volume: None,
                ..
            }
        ));
        assert!(matches!(
            press(&config, "sf6", &audio, &FakeVoice::new(false, None)),
            Err(PressError::Conflict {
                code: "not_running",
                ..
            })
        ));
    }

    #[test]
    fn voice_buttons_each_show_whether_their_own_channel_is_joined() {
        let config = config::parse(CONFIG).unwrap();
        let audio = FakeAudio::new(devices(), None);
        let snapshot = AudioSnapshot::read(&audio).unwrap();
        let voice = FakeVoice::new(true, Some("111"));
        let state = |i: usize| {
            view(
                &config,
                &config.buttons[i],
                &snapshot,
                &audio,
                &voice.status(),
            )
            .state
        };
        assert_eq!(
            state(2),
            ButtonState::Voice {
                available: true,
                joined: true,
                reason: None
            }
        );
        assert_eq!(
            state(3),
            ButtonState::Voice {
                available: true,
                joined: false,
                reason: None
            }
        );

        // Pressing the other button moves there; pressing it again leaves.
        press(&config, "vc-sf6", &audio, &voice).unwrap();
        assert_eq!(
            state(2),
            ButtonState::Voice {
                available: true,
                joined: false,
                reason: None
            }
        );
        assert_eq!(
            state(3),
            ButtonState::Voice {
                available: true,
                joined: true,
                reason: None
            }
        );
        press(&config, "vc-sf6", &audio, &voice).unwrap();
        assert_eq!(voice.status().selected, None);
        assert_eq!(*voice.presses.lock().unwrap(), ["222", "222"]);
    }

    #[test]
    fn voice_buttons_report_an_unreachable_discord() {
        let config = config::parse(CONFIG).unwrap();
        let audio = FakeAudio::new(devices(), None);
        let snapshot = AudioSnapshot::read(&audio).unwrap();
        let voice = FakeVoice::new(false, None);
        assert_eq!(
            view(
                &config,
                &config.buttons[2],
                &snapshot,
                &audio,
                &voice.status()
            )
            .state,
            ButtonState::Voice {
                available: false,
                joined: false,
                reason: Some("Discord is not running".into())
            }
        );
        assert!(matches!(
            press(&config, "vc-apex", &audio, &voice),
            Err(PressError::Conflict {
                code: "discord_unavailable",
                ..
            })
        ));
    }

    #[test]
    fn unknown_button_is_not_found() {
        let config = config::parse(CONFIG).unwrap();
        let audio = FakeAudio::new(devices(), None);
        assert_eq!(
            press(&config, "nope", &audio, &FakeVoice::new(false, None)),
            Err(PressError::NotFound)
        );
    }
}
