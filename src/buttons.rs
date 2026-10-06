//! Button state and press logic, independent of HTTP and of the audio backend.

use std::path::PathBuf;

use serde::Serialize;

use crate::{
    audio::{Audio, AudioError, Device},
    config::{ButtonConfig, ButtonSpec, Config},
    discord::ServerIcons,
    launch::{Launcher, Processes, Program, is_running},
    library::{Pictures, Pin, Pinned},
};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OutputOption {
    pub alias: String,
    pub name: Option<String>,
    pub connected: bool,
    /// What the device is, when the button says (`device_icons`).
    pub icon: Option<crate::config::DeviceIcon>,
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
    /// The default output's mute; `volume` is its volume (0-1).
    Mute {
        muted: bool,
        volume: f64,
    },
    /// The mixer the panel opens (`GET /audio/mixer`) instead of pressing, with the
    /// default output's volume and mute.
    Mixer {
        muted: bool,
        volume: f64,
    },
    /// A program, shortcut or URL to open; `running` while its `process` runs, when
    /// the button names one (a press then brings it to the front). A Discord server
    /// button is `running` while Discord runs.
    Launch {
        running: bool,
    },
    /// A Steam game: a press starts it, or closes it while `running`.
    Game {
        running: bool,
    },
    /// A game library the panel opens (`GET /buttons/{id}/library`) instead of
    /// pressing; `pins` are the games pinned to the button's tab, in order,
    /// `pictures` says whether the games' pictures are store art or program icons, and
    /// `license_keys` whether a game's license keys can be read (`GET …/keys`).
    Library {
        pins: Vec<Pin>,
        pictures: Pictures,
        license_keys: bool,
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
    /// The desktop (by name) whose tab shows this button; `None` for shared buttons,
    /// which every tab shows except those named in `except`.
    pub desktop: Option<String>,
    pub except: Vec<String>,
    /// Whether `GET /buttons/{id}/icon` has a picture.
    pub icon: bool,
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
    Launch(String),
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

pub fn round2(value: f32) -> f64 {
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

/// DLsite's favicon, which a `dlsite.library` button shows unless it names another icon.
pub const DLSITE_ICON: &str = "https://www.dlsite.com/images/web/common/favicon.ico";

/// The Discord desktop app's executable.
const DISCORD_EXE: &str = "Discord.exe";

/// Discord's launcher, which starts the installed version (or hands its arguments to the
/// running one).
pub fn discord_update_exe() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map_or_else(PathBuf::new, PathBuf::from);
    base.join("Discord").join("Update.exe")
}

/// The picture on the web the button shows: its `icon` when that is an `http(s)` URL,
/// else DLsite's favicon for a DLsite library, or a Discord server's icon once read.
pub fn icon_url(button: &ButtonConfig, server_icons: &ServerIcons) -> Option<String> {
    match (&button.icon, &button.spec) {
        (Some(icon), _) => {
            let icon = icon.to_string_lossy();
            (icon.starts_with("https://") || icon.starts_with("http://")).then(|| icon.into_owned())
        }
        (None, ButtonSpec::DlsiteLibrary { .. }) => Some(DLSITE_ICON.to_owned()),
        (None, ButtonSpec::DiscordServer { guild_id }) => server_icons.get(guild_id).cloned(),
        (None, _) => None,
    }
}

/// The file whose Windows icon the button shows: its `icon` (unless that is on the
/// web), or what an `app.launch` button opens when that is a file.
pub fn icon_path(button: &ButtonConfig) -> Option<PathBuf> {
    if icon_url(button, &ServerIcons::new()).is_some() {
        return None;
    }
    if let Some(icon) = &button.icon {
        return Some(icon.clone());
    }
    match &button.spec {
        ButtonSpec::AppLaunch { target, .. } if !target.contains("://") => {
            Some(PathBuf::from(target))
        }
        _ => None,
    }
}

/// Everything read once per refresh that the button states need.
pub struct Readings<'a> {
    pub audio: &'a AudioSnapshot,
    /// Discord servers' icons read so far.
    pub server_icons: &'a ServerIcons,
    pub processes: &'a Processes,
    pub pins: &'a Pinned,
}

pub fn view(
    config: &Config,
    button: &ButtonConfig,
    readings: &Readings<'_>,
    audio: &dyn Audio,
) -> ButtonView {
    ButtonView {
        id: button.id.clone(),
        kind: button.spec.type_name(),
        label: button.label.clone(),
        desktop: button.desktop.clone(),
        except: button.except.clone(),
        icon: icon_url(button, readings.server_icons).is_some()
            || icon_path(button).is_some_and(|path| {
                path.to_string_lossy().to_lowercase().starts_with("shell:") || path.exists()
            }),
        state: state(config, button, readings, audio),
    }
}

fn state(
    config: &Config,
    button: &ButtonConfig,
    readings: &Readings<'_>,
    audio: &dyn Audio,
) -> ButtonState {
    let spec = &button.spec;
    let snapshot = readings.audio;
    match spec {
        ButtonSpec::OutputToggle {
            devices,
            device_icons,
        } => {
            let options = devices
                .iter()
                .enumerate()
                .map(|(index, alias)| {
                    let device = config
                        .devices
                        .get(alias)
                        .and_then(|id| snapshot.devices.iter().find(|d| &d.id == id));
                    OutputOption {
                        alias: alias.clone(),
                        name: device.map(|d| d.name.clone()),
                        connected: device.is_some_and(|d| d.connected),
                        icon: device_icons.map(|icons| icons[index]),
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
        ButtonSpec::MuteToggle {} | ButtonSpec::Mixer {} => match audio.master() {
            Ok(master) => {
                let (muted, volume) = (master.muted, round2(master.volume));
                if matches!(spec, ButtonSpec::Mixer {}) {
                    ButtonState::Mixer { muted, volume }
                } else {
                    ButtonState::Mute { muted, volume }
                }
            }
            Err(error) => ButtonState::Error {
                message: error.to_string(),
            },
        },
        ButtonSpec::DiscordServer { .. } => ButtonState::Launch {
            running: is_running(readings.processes, DISCORD_EXE),
        },
        ButtonSpec::AppLaunch { process, .. } => ButtonState::Launch {
            running: process
                .as_deref()
                .is_some_and(|p| is_running(readings.processes, p)),
        },
        ButtonSpec::SteamGame { process, .. } => ButtonState::Game {
            running: is_running(readings.processes, process),
        },
        ButtonSpec::SteamLibrary { .. } | ButtonSpec::DlsiteLibrary { .. } => {
            ButtonState::Library {
                pins: readings.pins.get(&button.id).cloned().unwrap_or_default(),
                pictures: spec.pictures().unwrap_or_default(),
                license_keys: matches!(spec, ButtonSpec::DlsiteLibrary { .. }),
            }
        }
    }
}

/// Make the other of the two devices the default output, when it is connected.
fn press_output(
    config: &Config,
    devices: &[String; 2],
    audio: &dyn Audio,
) -> Result<(), PressError> {
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

pub fn press(
    config: &Config,
    id: &str,
    audio: &dyn Audio,
    launcher: &dyn Launcher,
) -> Result<(), PressError> {
    let button = config
        .buttons
        .iter()
        .find(|b| b.id == id)
        .ok_or(PressError::NotFound)?;
    match &button.spec {
        ButtonSpec::OutputToggle { devices, .. } => press_output(config, devices, audio),
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
        ButtonSpec::MuteToggle {} => {
            let master = audio.master().map_err(PressError::Audio)?;
            audio
                .set_master(None, Some(!master.muted))
                .map_err(PressError::Audio)
        }
        ButtonSpec::Mixer {} => Err(PressError::Conflict {
            code: "not_pressable",
            message: "the mixer opens on the panel; use GET /audio/mixer".into(),
        }),
        ButtonSpec::DiscordServer { guild_id } => {
            // Discord starts if needed and shows the server. Not through `discord://`: its
            // registration names a version folder that Discord removes when it updates.
            // Discord may come up behind the window that had the focus or on another
            // virtual desktop, so bring it here too.
            let args = format!(
                "--processStart {DISCORD_EXE} --process-start-args \"--url -- discord://-/channels/{guild_id}\""
            );
            launcher
                .open(&discord_update_exe().to_string_lossy(), Some(&args), false)
                .map_err(PressError::Launch)?;
            launcher.focus_when_ready(Program::Exe(DISCORD_EXE.into()));
            Ok(())
        }
        ButtonSpec::AppLaunch {
            target,
            args,
            process,
            admin,
        } => {
            if let Some(process) = process
                && is_running(&launcher.processes(), process)
                && launcher
                    .focus(&Program::Exe(process.clone()))
                    .map_err(PressError::Launch)?
            {
                return Ok(());
            }
            launcher
                .open(target, args.as_deref(), *admin)
                .map_err(PressError::Launch)
        }
        ButtonSpec::SteamLibrary { .. } | ButtonSpec::DlsiteLibrary { .. } => {
            Err(PressError::Conflict {
                code: "not_pressable",
                message: "a library opens on the panel; use GET /buttons/{id}/library".into(),
            })
        }
        ButtonSpec::SteamGame { app_id, process } => {
            if is_running(&launcher.processes(), process) {
                match launcher.close(process).map_err(PressError::Launch)? {
                    0 => Err(PressError::Conflict {
                        code: "no_window",
                        message: format!("{process} has no window to close yet"),
                    }),
                    _ => Ok(()),
                }
            } else {
                launcher
                    .open(&format!("steam://rungameid/{app_id}"), None, false)
                    .map_err(PressError::Launch)?;
                launcher.focus_when_ready(Program::Exe(process.clone()));
                Ok(())
            }
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
        launch::{Launcher, Processes, Program, fake::FakeLauncher},
        library::{Pictures, Pin, Pinned},
    };

    static NO_PROCESSES: std::sync::LazyLock<Processes> = std::sync::LazyLock::new(Processes::new);
    static NO_PINS: std::sync::LazyLock<Pinned> = std::sync::LazyLock::new(Pinned::new);
    static NO_ICONS: std::sync::LazyLock<crate::discord::ServerIcons> =
        std::sync::LazyLock::new(crate::discord::ServerIcons::new);

    fn readings(audio: &AudioSnapshot) -> super::Readings<'_> {
        super::Readings {
            audio,
            server_icons: &NO_ICONS,
            processes: &NO_PROCESSES,
            pins: &NO_PINS,
        }
    }

    #[test]
    fn output_options_carry_the_icon_given_to_each_device() {
        use super::OutputOption;
        use crate::config::DeviceIcon;

        let config = config::parse(
            "devices:\n  motu: id-motu\n  jbl: id-jbl\nbuttons:\n  - id: output\n    label: Output\n    type: audio.output_toggle\n    devices: [motu, jbl]\n    device_icons: [speaker, headphones]\n  - id: plain\n    label: Plain\n    type: audio.output_toggle\n    devices: [motu, jbl]\n",
        )
        .unwrap();
        let audio = FakeAudio::new(devices(), Some("id-jbl"));
        let snapshot = AudioSnapshot::read(&audio).unwrap();
        let options = |index: usize| match view(
            &config,
            &config.buttons[index],
            &readings(&snapshot),
            &audio,
        )
        .state
        {
            ButtonState::Output { options, .. } => options,
            other => panic!("{other:?}"),
        };
        let icons: Vec<_> = options(0).iter().map(|o: &OutputOption| o.icon).collect();
        assert_eq!(
            icons,
            [Some(DeviceIcon::Speaker), Some(DeviceIcon::Headphones)]
        );
        assert!(options(1).iter().all(|o| o.icon.is_none()));
        assert!(config::parse(
            "devices:\n  a: id-a\n  b: id-b\nbuttons:\n  - id: o\n    label: O\n    type: audio.output_toggle\n    devices: [a, b]\n    device_icons: [speaker, tuba]\n",
        )
        .is_err());
    }

    #[test]
    fn icons_from_the_web_and_dlsites_own() {
        use super::{DLSITE_ICON, icon_url};

        let config = config::parse(
            "buttons:\n  - id: web\n    label: Web\n    type: app.launch\n    target: https://example.com\n    icon: https://example.com/favicon.ico\n  - id: dlsite\n    label: DLsite\n    type: dlsite.library\n  - id: steam\n    label: Steam\n    type: steam.library\n",
        )
        .unwrap();
        let urls: Vec<_> = config
            .buttons
            .iter()
            .map(|b| icon_url(b, &crate::discord::ServerIcons::new()))
            .collect();
        assert_eq!(
            urls,
            [
                Some("https://example.com/favicon.ico".to_owned()),
                Some(DLSITE_ICON.to_owned()),
                None
            ]
        );
        let audio = FakeAudio::new(devices(), None);
        let snapshot = AudioSnapshot::read(&audio).unwrap();
        let shows = |index: usize| {
            view(
                &config,
                &config.buttons[index],
                &readings(&snapshot),
                &audio,
            )
            .icon
        };
        assert!(shows(0) && shows(1) && !shows(2));
    }

    #[test]
    fn mute_toggles_the_output_and_the_mixer_opens_on_the_panel() {
        let config = config::parse(
            "buttons:\n  - id: mute\n    label: Mute\n    type: audio.mute_toggle\n  - id: mixer\n    label: Mixer\n    type: audio.mixer\n",
        )
        .unwrap();
        let audio = FakeAudio::new(devices(), Some("id-motu"));
        let state = |index: usize| {
            let snapshot = AudioSnapshot::read(&audio).unwrap();
            view(
                &config,
                &config.buttons[index],
                &readings(&snapshot),
                &audio,
            )
            .state
        };
        assert_eq!(
            state(0),
            ButtonState::Mute {
                muted: false,
                volume: 0.5
            }
        );
        let launcher = FakeLauncher::default();
        press(&config, "mute", &audio, &launcher).unwrap();
        assert!(audio.master().unwrap().muted);
        assert_eq!(
            state(1),
            ButtonState::Mixer {
                muted: true,
                volume: 0.5
            }
        );
        press(&config, "mute", &audio, &launcher).unwrap();
        assert!(!audio.master().unwrap().muted);
        assert!(matches!(
            press(&config, "mixer", &audio, &launcher),
            Err(PressError::Conflict {
                code: "not_pressable",
                ..
            })
        ));
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
        press(&config, "output", &audio, &FakeLauncher::default()).unwrap();
        assert_eq!(audio.default_output().unwrap().as_deref(), Some("id-jbl"));

        let snapshot = AudioSnapshot::read(&audio).unwrap();
        let view = view(&config, &config.buttons[0], &readings(&snapshot), &audio);
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
        let error = press(&config, "output", &audio, &FakeLauncher::default()).unwrap_err();
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
        let view = view(&config, &config.buttons[0], &readings(&snapshot), &audio);
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
        press(&config, "sf6", &audio, &FakeLauncher::default()).unwrap();
        let snapshot = AudioSnapshot::read(&audio).unwrap();
        let view = view(&config, &config.buttons[1], &readings(&snapshot), &audio);
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
        let view = view(&config, &config.buttons[1], &readings(&snapshot), &audio);
        assert!(matches!(
            view.state,
            ButtonState::Volume {
                running: false,
                volume: None,
                ..
            }
        ));
        assert!(matches!(
            press(&config, "sf6", &audio, &FakeLauncher::default()),
            Err(PressError::Conflict {
                code: "not_running",
                ..
            })
        ));
    }

    #[test]
    fn discord_server_buttons_bring_discord_up_on_their_server_with_its_icon() {
        use super::icon_url;
        use crate::discord::ServerIcons;

        let config = config::parse(
            "buttons:\n  - id: uf4\n    label: UF4\n    type: discord.server\n    guild_id: 1533\n  - id: other\n    label: Other\n    type: discord.server\n    guild_id: 99\n",
        )
        .unwrap();
        let audio = FakeAudio::new(devices(), None);
        let snapshot = AudioSnapshot::read(&audio).unwrap();
        let picture = "https://cdn.discordapp.com/icons/1533/d08.webp?size=128";
        let icons = ServerIcons::from([("1533".to_owned(), picture.to_owned())]);
        let launcher = FakeLauncher::default();
        launcher
            .running
            .lock()
            .unwrap()
            .insert("discord.exe".into());
        let processes = launcher.processes();
        let readings = super::Readings {
            audio: &snapshot,
            server_icons: &icons,
            processes: &processes,
            pins: &NO_PINS,
        };
        let uf4 = view(&config, &config.buttons[0], &readings, &audio);
        assert_eq!(uf4.state, ButtonState::Launch { running: true });
        assert!(uf4.icon);
        assert_eq!(
            icon_url(&config.buttons[0], &icons).as_deref(),
            Some(picture)
        );
        // A server whose icon has not been read (yet) has none.
        assert!(!view(&config, &config.buttons[1], &readings, &audio).icon);
        assert_eq!(
            view(
                &config,
                &config.buttons[0],
                &self::readings(&snapshot),
                &audio
            )
            .state,
            ButtonState::Launch { running: false }
        );

        // Through Discord's own launcher: the `discord://` registration goes stale when
        // Discord updates itself.
        press(&config, "uf4", &audio, &launcher).unwrap();
        assert_eq!(
            *launcher.opened.lock().unwrap(),
            [format!(
                "{} --processStart Discord.exe --process-start-args \"--url -- discord://-/channels/1533\"",
                super::discord_update_exe().display()
            )]
        );
        assert!(super::discord_update_exe().ends_with(r"Discord\Update.exe"));
        assert_eq!(
            *launcher.awaited.lock().unwrap(),
            [Program::Exe("Discord.exe".into())]
        );
    }

    #[test]
    fn launch_buttons_open_their_target_and_show_its_icon() {
        let config = config::parse(
            "buttons:\n  - id: ba\n    label: BA\n    type: app.launch\n    target: C:/Games/ba.exe\n    args: --fast\n  - id: site\n    label: Site\n    type: app.launch\n    target: https://example.com/\n",
        )
        .unwrap();
        let launcher = FakeLauncher::default();
        let audio = FakeAudio::new(devices(), None);
        press(&config, "ba", &audio, &launcher).unwrap();
        press(&config, "site", &audio, &launcher).unwrap();
        assert_eq!(
            *launcher.opened.lock().unwrap(),
            ["C:/Games/ba.exe --fast", "https://example.com/"]
        );
        assert_eq!(
            super::icon_path(&config.buttons[0]),
            Some(std::path::PathBuf::from("C:/Games/ba.exe"))
        );
        assert_eq!(super::icon_path(&config.buttons[1]), None);
    }

    #[test]
    fn launch_buttons_bring_a_running_app_forward_instead_of_starting_it_again() {
        let config = config::parse(
            "buttons:\n  - id: ba\n    label: BA\n    type: app.launch\n    target: C:/Games/launcher.exe\n    process: BlueArchive.exe\n  - id: admin\n    label: Admin\n    type: app.launch\n    target: wt.exe\n    admin: true\n    icon: shell:AppsFolder\\Microsoft.WindowsTerminal_8wekyb3d8bbwe!App\n",
        )
        .unwrap();
        let launcher = FakeLauncher::default();
        let audio = FakeAudio::new(devices(), None);
        press(&config, "ba", &audio, &launcher).unwrap();
        assert_eq!(*launcher.opened.lock().unwrap(), ["C:/Games/launcher.exe"]);

        launcher
            .running
            .lock()
            .unwrap()
            .insert("bluearchive.exe".into());
        let snapshot = AudioSnapshot::read(&audio).unwrap();
        let processes = launcher.processes();
        let readings = super::Readings {
            audio: &snapshot,
            server_icons: &NO_ICONS,
            processes: &processes,
            pins: &NO_PINS,
        };
        assert_eq!(
            view(&config, &config.buttons[0], &readings, &audio).state,
            ButtonState::Launch { running: true }
        );
        press(&config, "ba", &audio, &launcher).unwrap();
        assert_eq!(
            *launcher.focused.lock().unwrap(),
            [Program::Exe("BlueArchive.exe".into())]
        );
        assert_eq!(launcher.opened.lock().unwrap().len(), 1);

        press(&config, "admin", &audio, &launcher).unwrap();
        assert_eq!(launcher.opened.lock().unwrap()[1], "wt.exe (admin)");
        assert!(view(&config, &config.buttons[1], &readings, &audio).icon);
    }

    #[test]
    fn steam_games_start_through_steam_and_close_while_running() {
        let config = config::parse(
            "buttons:\n  - id: sf6\n    label: SF6\n    type: steam.game\n    app_id: 1364780\n    process: StreetFighter6.exe\n    icon: C:/sf6.exe\n",
        )
        .unwrap();
        let launcher = FakeLauncher::default();
        let audio = FakeAudio::new(devices(), None);
        let snapshot = AudioSnapshot::read(&audio).unwrap();
        let state = |launcher: &FakeLauncher| {
            let processes = launcher.processes();
            let readings = super::Readings {
                audio: &snapshot,
                server_icons: &NO_ICONS,
                processes: &processes,
                pins: &NO_PINS,
            };
            view(&config, &config.buttons[0], &readings, &audio).state
        };
        assert_eq!(state(&launcher), ButtonState::Game { running: false });
        press(&config, "sf6", &audio, &launcher).unwrap();
        assert_eq!(
            *launcher.opened.lock().unwrap(),
            ["steam://rungameid/1364780"]
        );
        assert_eq!(
            *launcher.awaited.lock().unwrap(),
            [Program::Exe("StreetFighter6.exe".into())]
        );

        launcher
            .running
            .lock()
            .unwrap()
            .insert("streetfighter6.exe".into());
        assert_eq!(state(&launcher), ButtonState::Game { running: true });
        press(&config, "sf6", &audio, &launcher).unwrap();
        assert_eq!(state(&launcher), ButtonState::Game { running: false });
        assert_eq!(launcher.opened.lock().unwrap().len(), 1);
    }

    #[test]
    fn library_buttons_show_their_pins_and_open_on_the_panel_instead_of_pressing() {
        let config = config::parse(
            "buttons:\n  - id: library\n    label: Library\n    type: steam.library\n  - id: other\n    label: Other\n    type: dlsite.library\n",
        )
        .unwrap();
        let audio = FakeAudio::new(devices(), None);
        let snapshot = AudioSnapshot::read(&audio).unwrap();
        let pinned = Pin {
            id: "1364780".into(),
            name: "Street Fighter 6".into(),
        };
        let pins: Pinned = [("library".to_owned(), vec![pinned.clone()])].into();
        let readings = super::Readings {
            audio: &snapshot,
            server_icons: &NO_ICONS,
            processes: &NO_PROCESSES,
            pins: &pins,
        };
        assert_eq!(
            view(&config, &config.buttons[0], &readings, &audio).state,
            ButtonState::Library {
                pins: vec![pinned],
                pictures: Pictures::Cover,
                license_keys: false
            }
        );
        assert_eq!(
            view(&config, &config.buttons[1], &readings, &audio).state,
            ButtonState::Library {
                pins: vec![],
                pictures: Pictures::Whole,
                license_keys: true
            }
        );
        assert!(matches!(
            press(&config, "library", &audio, &FakeLauncher::default()),
            Err(PressError::Conflict {
                code: "not_pressable",
                ..
            })
        ));
    }

    #[test]
    fn unknown_button_is_not_found() {
        let config = config::parse(CONFIG).unwrap();
        let audio = FakeAudio::new(devices(), None);
        assert_eq!(
            press(&config, "nope", &audio, &FakeLauncher::default()),
            Err(PressError::NotFound)
        );
    }
}
