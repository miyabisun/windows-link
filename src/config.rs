//! Machine-local YAML configuration: device aliases and shared buttons in `config.yaml`,
//! and each virtual desktop's own buttons in `desktops/<desktop name>.yaml` next to it.

use std::{
    collections::{BTreeMap, HashSet},
    env, fmt,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::library::Pictures;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Alias -> audio endpoint ID (see `windows-link devices`).
    #[serde(default)]
    pub devices: BTreeMap<String, String>,
    /// Shared buttons from `config.yaml`, then each desktop file's buttons.
    #[serde(default)]
    pub buttons: Vec<ButtonConfig>,
    /// Names of the desktops that have a file in `desktops/`, sorted.
    #[serde(skip)]
    pub desktops: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ButtonConfig {
    pub id: String,
    pub label: String,
    /// Shared buttons only: desktop names whose tab leaves this button out.
    #[serde(default)]
    pub except: Vec<String>,
    /// File (exe, shortcut, image) whose Windows icon the button shows, or a picture's
    /// `http(s)` URL.
    #[serde(default)]
    pub icon: Option<PathBuf>,
    /// The desktop whose file defines the button; `None` for shared buttons.
    #[serde(skip)]
    pub desktop: Option<String>,
    #[serde(flatten)]
    pub spec: ButtonSpec,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DesktopFile {
    #[serde(default)]
    buttons: Vec<ButtonConfig>,
}

/// What an output device is, for its icon on the panel.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceIcon {
    Speaker,
    Headphones,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ButtonSpec {
    /// Toggle the Windows default output between two device aliases.
    #[serde(rename = "audio.output_toggle")]
    OutputToggle {
        devices: [String; 2],
        /// What each device is, in the same order, for the panel's icon.
        #[serde(default)]
        device_icons: Option<[DeviceIcon; 2]>,
    },
    /// Toggle one process's session volume between two levels (0.0-1.0).
    #[serde(rename = "audio.app_volume_toggle")]
    AppVolumeToggle { process: String, levels: [f32; 2] },
    /// Mute the default output, or unmute it.
    #[serde(rename = "audio.mute_toggle")]
    MuteToggle {},
    /// The mixer: the panel opens the default output's volume and each app's
    /// (`GET /audio/mixer`) instead of pressing.
    #[serde(rename = "audio.mixer")]
    Mixer {},
    /// Join one Discord voice channel, or leave it when already there.
    #[serde(rename = "discord.voice")]
    DiscordVoice {
        #[serde(deserialize_with = "crate::secrets::id_text")]
        channel_id: String,
    },
    /// Open a program, shortcut or URL the way double-clicking it in Explorer does.
    #[serde(rename = "app.launch")]
    AppLaunch {
        target: String,
        #[serde(default)]
        args: Option<String>,
        /// Executable file name; while it runs, a press brings its window to the front
        /// instead of opening `target` again.
        #[serde(default)]
        process: Option<String>,
        /// Open as administrator (Windows asks for consent).
        #[serde(default)]
        admin: bool,
    },
    /// Search the owned Steam games on the panel, start them, and pin them to the tab.
    #[serde(rename = "steam.library")]
    SteamLibrary {
        /// Collections whose games are listed only while their label is selected.
        #[serde(default)]
        hide: Vec<String>,
    },
    /// Search the DLsite games in a folder on the panel, start them, and pin them to the
    /// tab.
    #[serde(rename = "dlsite.library")]
    DlsiteLibrary {
        /// `<maker>\<title>` folders; DLsiteNest's `D:\DLsiteNest\Game` by default.
        #[serde(default)]
        root: Option<PathBuf>,
        /// Labels whose games are listed only while the label is selected.
        #[serde(default)]
        hide: Vec<String>,
    },
    /// Start a Steam game, or close it while it runs.
    #[serde(rename = "steam.game")]
    SteamGame {
        #[serde(deserialize_with = "crate::secrets::id_text")]
        app_id: String,
        /// Executable file name of the running game, case-insensitive.
        process: String,
    },
}

impl ButtonSpec {
    /// How a library button's pictures are shown: Steam's wide art fills a tile, DLsite's
    /// pictures and icons are shown whole.
    pub fn pictures(&self) -> Option<Pictures> {
        match self {
            Self::SteamLibrary { .. } => Some(Pictures::Cover),
            Self::DlsiteLibrary { .. } => Some(Pictures::Whole),
            _ => None,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::OutputToggle { .. } => "audio.output_toggle",
            Self::AppVolumeToggle { .. } => "audio.app_volume_toggle",
            Self::MuteToggle {} => "audio.mute_toggle",
            Self::Mixer {} => "audio.mixer",
            Self::DiscordVoice { .. } => "discord.voice",
            Self::AppLaunch { .. } => "app.launch",
            Self::SteamGame { .. } => "steam.game",
            Self::SteamLibrary { .. } => "steam.library",
            Self::DlsiteLibrary { .. } => "dlsite.library",
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

/// `WINDOWS_LINK_CONFIG`, or `%LOCALAPPDATA%\windows-link\config.yaml` (device IDs are
/// machine-specific, so the file does not roam).
pub fn default_path() -> PathBuf {
    if let Some(path) = env::var_os("WINDOWS_LINK_CONFIG") {
        return PathBuf::from(path);
    }
    let base = env::var_os("LOCALAPPDATA").map_or_else(|| PathBuf::from("."), PathBuf::from);
    base.join("windows-link").join("config.yaml")
}

/// A missing file is an empty configuration so the server can start before setup.
/// Desktop files are read from `desktops/` next to `path`.
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(ConfigError(format!(
                "cannot read {}: {err}",
                path.display()
            )));
        }
    };
    let dir = path.parent().unwrap_or(Path::new(".")).join("desktops");
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let file = entry.path();
            let is_yaml = file
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("yaml"));
            let Some(name) = file.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if is_yaml {
                let text = std::fs::read_to_string(&file)
                    .map_err(|err| ConfigError(format!("cannot read {}: {err}", file.display())))?;
                files.push((name.to_owned(), text));
            }
        }
    }
    parse_all(&text, &files)
}

pub fn parse(text: &str) -> Result<Config, ConfigError> {
    parse_all(text, &[])
}

/// `config.yaml` and the desktop files as `(desktop name, text)`.
pub fn parse_all(text: &str, desktops: &[(String, String)]) -> Result<Config, ConfigError> {
    let mut config: Config = if text.trim().is_empty() {
        Config::default()
    } else {
        serde_norway::from_str(text).map_err(|err| ConfigError(format!("invalid YAML: {err}")))?
    };
    let mut desktops: Vec<&(String, String)> = desktops.iter().collect();
    desktops.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, text) in desktops {
        let file: DesktopFile = if text.trim().is_empty() {
            DesktopFile {
                buttons: Vec::new(),
            }
        } else {
            serde_norway::from_str(text)
                .map_err(|err| ConfigError(format!("desktops/{name}.yaml: invalid YAML: {err}")))?
        };
        for mut button in file.buttons {
            if !button.except.is_empty() {
                return Err(ConfigError(format!(
                    "desktops/{name}.yaml: button {:?}: except is only for shared buttons in config.yaml",
                    button.id
                )));
            }
            button.desktop = Some(name.clone());
            config.buttons.push(button);
        }
        config.desktops.push(name.clone());
    }
    validate(&config)?;
    Ok(config)
}

fn validate(config: &Config) -> Result<(), ConfigError> {
    let mut ids = HashSet::new();
    for button in &config.buttons {
        let id = &button.id;
        if id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(ConfigError(format!(
                "button id {id:?} must use only letters, digits, '-' or '_'"
            )));
        }
        if !ids.insert(id.as_str()) {
            return Err(ConfigError(format!("duplicate button id {id:?}")));
        }
        match &button.spec {
            ButtonSpec::OutputToggle { devices, .. } => {
                if devices[0] == devices[1] {
                    return Err(ConfigError(format!(
                        "button {id:?}: the two devices must differ"
                    )));
                }
                for alias in devices {
                    if !config.devices.contains_key(alias) {
                        return Err(ConfigError(format!(
                            "button {id:?}: unknown device alias {alias:?}"
                        )));
                    }
                }
            }
            ButtonSpec::AppVolumeToggle { process, levels } => {
                if process.trim().is_empty() {
                    return Err(ConfigError(format!("button {id:?}: process is empty")));
                }
                if levels.iter().any(|l| !(0.0..=1.0).contains(l)) {
                    return Err(ConfigError(format!(
                        "button {id:?}: levels must be between 0.0 and 1.0"
                    )));
                }
                if (levels[0] - levels[1]).abs() < f32::EPSILON {
                    return Err(ConfigError(format!(
                        "button {id:?}: the two levels must differ"
                    )));
                }
            }
            ButtonSpec::AppLaunch { target, .. } => {
                if target.trim().is_empty() {
                    return Err(ConfigError(format!("button {id:?}: target is empty")));
                }
            }
            ButtonSpec::SteamGame { app_id, process } => {
                if !app_id.bytes().all(|b| b.is_ascii_digit()) || process.trim().is_empty() {
                    return Err(ConfigError(format!(
                        "button {id:?}: app_id must be a Steam app ID and process an exe name"
                    )));
                }
            }
            ButtonSpec::SteamLibrary { .. }
            | ButtonSpec::DlsiteLibrary { .. }
            | ButtonSpec::MuteToggle {}
            | ButtonSpec::Mixer {} => {}
            ButtonSpec::DiscordVoice { channel_id } => {
                if !channel_id.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(ConfigError(format!(
                        "button {id:?}: channel_id must be a Discord channel ID (digits; see `windows-link discord-channels`)"
                    )));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ButtonSpec, parse};

    const SAMPLE: &str = r#"
devices:
  motu: "{0.0.0.00000000}.{aaaa}"
  jbl: "{0.0.0.00000000}.{bbbb}"
buttons:
  - id: output
    label: 出力切替
    type: audio.output_toggle
    devices: [motu, jbl]
  - id: sf6-volume
    label: スト6 音量
    type: audio.app_volume_toggle
    process: StreetFighter6.exe
    levels: [0.2, 1.0]
"#;

    #[test]
    #[allow(clippy::float_cmp, reason = "levels are parsed from exact literals")]
    fn parses_both_button_types_in_order() {
        let config = parse(SAMPLE).unwrap();
        assert_eq!(config.devices.len(), 2);
        assert_eq!(config.buttons.len(), 2);
        assert_eq!(config.buttons[0].id, "output");
        assert!(matches!(
            &config.buttons[0].spec,
            ButtonSpec::OutputToggle { devices, .. } if devices == &["motu".to_owned(), "jbl".to_owned()]
        ));
        assert!(matches!(
            &config.buttons[1].spec,
            ButtonSpec::AppVolumeToggle { process, levels } if process == "StreetFighter6.exe" && *levels == [0.2, 1.0]
        ));
    }

    #[test]
    fn empty_text_is_an_empty_config() {
        let config = parse("{}").unwrap();
        assert!(config.buttons.is_empty());
    }

    #[test]
    fn rejects_unknown_device_alias() {
        let text = SAMPLE.replace("[motu, jbl]", "[motu, missing]");
        assert!(parse(&text).unwrap_err().0.contains("unknown device alias"));
    }

    #[test]
    fn rejects_duplicate_and_malformed_ids() {
        let dup = SAMPLE.replace("id: sf6-volume", "id: output");
        assert!(parse(&dup).unwrap_err().0.contains("duplicate"));
        let bad = SAMPLE.replace("id: output", "id: out put");
        assert!(parse(&bad).unwrap_err().0.contains("button id"));
    }

    #[test]
    fn rejects_out_of_range_or_equal_levels() {
        let high = SAMPLE.replace("[0.2, 1.0]", "[0.2, 1.5]");
        assert!(parse(&high).unwrap_err().0.contains("between 0.0 and 1.0"));
        let same = SAMPLE.replace("[0.2, 1.0]", "[0.5, 0.5]");
        assert!(parse(&same).unwrap_err().0.contains("must differ"));
    }

    #[test]
    fn rejects_identical_output_devices() {
        let text = SAMPLE.replace("[motu, jbl]", "[motu, motu]");
        assert!(parse(&text).unwrap_err().0.contains("must differ"));
    }

    #[test]
    fn discord_voice_buttons_take_one_channel_each_quoted_or_not() {
        let text = format!(
            "{SAMPLE}  - id: vc-apex
    label: APEX
    type: discord.voice
    channel_id: 1234567890123456789
  - id: vc-sf6
    label: SF6
    type: discord.voice
    channel_id: \"987\"
"
        );
        let config = parse(&text).unwrap();
        assert!(matches!(
            &config.buttons[2].spec,
            ButtonSpec::DiscordVoice { channel_id } if channel_id == "1234567890123456789"
        ));
        assert!(matches!(
            &config.buttons[3].spec,
            ButtonSpec::DiscordVoice { channel_id } if channel_id == "987"
        ));
        let bad = text.replace("\"987\"", "general");
        assert!(parse(&bad).unwrap_err().0.contains("channel_id"));
    }

    #[test]
    fn desktop_files_add_their_buttons_after_the_shared_ones() {
        let shared = "devices:\n  a: id-a\n  b: id-b\nbuttons:\n  - id: output\n    label: Output\n    type: audio.output_toggle\n    devices: [a, b]\n    except: [dev]\n";
        let files = vec![
            (
                "SF6".to_owned(),
                "buttons:\n  - id: sf6\n    label: SF6\n    type: steam.game\n    app_id: 1364780\n    process: StreetFighter6.exe\n    icon: C:/Games/SF6/StreetFighter6.exe\n".to_owned(),
            ),
            (
                "ブルアカ".to_owned(),
                "buttons:\n  - id: ba\n    label: BA\n    type: app.launch\n    target: C:/YostarGames/launcher.exe\n".to_owned(),
            ),
            ("dev".to_owned(), String::new()),
        ];
        let config = super::parse_all(shared, &files).unwrap();
        let ids: Vec<_> = config
            .buttons
            .iter()
            .map(|b| (b.id.as_str(), b.desktop.as_deref()))
            .collect();
        assert_eq!(
            ids,
            [
                ("output", None),
                ("sf6", Some("SF6")),
                ("ba", Some("ブルアカ"))
            ]
        );
        assert_eq!(config.buttons[0].except, ["dev"]);
        assert_eq!(config.desktops, ["SF6", "dev", "ブルアカ"]);
        assert!(config.buttons[1].icon.is_some());
    }

    #[test]
    fn desktop_files_cannot_use_except_or_reuse_ids() {
        let shared = "buttons:\n  - id: x\n    label: X\n    type: app.launch\n    target: a.exe\n";
        let except = vec![(
            "SF6".to_owned(),
            "buttons:\n  - id: y\n    label: Y\n    type: app.launch\n    target: b.exe\n    except: [dev]\n".to_owned(),
        )];
        assert!(
            super::parse_all(shared, &except)
                .unwrap_err()
                .0
                .contains("except")
        );
        let dup = vec![(
            "SF6".to_owned(),
            "buttons:\n  - id: x\n    label: X2\n    type: app.launch\n    target: b.exe\n"
                .to_owned(),
        )];
        assert!(
            super::parse_all(shared, &dup)
                .unwrap_err()
                .0
                .contains("duplicate")
        );
        let typo = vec![("SF6".to_owned(), "button: []\n".to_owned())];
        assert!(
            super::parse_all(shared, &typo)
                .unwrap_err()
                .0
                .contains("desktops/SF6.yaml")
        );
    }

    #[test]
    fn steam_library_buttons_name_the_labels_they_hide() {
        let files = vec![(
            "ゲーム".to_owned(),
            "buttons:\n  - id: library\n    label: ゲーム検索\n    type: steam.library\n    hide: [outdate]\n  - id: all\n    label: All\n    type: steam.library\n".to_owned(),
        )];
        let config = super::parse_all("", &files).unwrap();
        assert!(matches!(
            &config.buttons[0].spec,
            ButtonSpec::SteamLibrary { hide } if hide == &["outdate"]
        ));
        assert!(matches!(
            &config.buttons[1].spec,
            ButtonSpec::SteamLibrary { hide } if hide.is_empty()
        ));
        assert_eq!(config.buttons[0].spec.type_name(), "steam.library");
    }

    #[test]
    fn dlsite_library_buttons_take_an_optional_folder() {
        let files = vec![(
            "アダルト".to_owned(),
            "buttons:\n  - id: dlsite\n    label: DLsite\n    type: dlsite.library\n    hide: [非表示]\n  - id: other\n    label: Other\n    type: dlsite.library\n    root: E:/Games\n".to_owned(),
        )];
        let config = super::parse_all("", &files).unwrap();
        assert!(matches!(
            &config.buttons[0].spec,
            ButtonSpec::DlsiteLibrary { root: None, hide } if hide == &["非表示"]
        ));
        assert!(matches!(
            &config.buttons[1].spec,
            ButtonSpec::DlsiteLibrary { root: Some(root), .. } if root == std::path::Path::new("E:/Games")
        ));
        assert_eq!(config.buttons[0].spec.type_name(), "dlsite.library");
    }

    #[test]
    fn rejects_unknown_button_type() {
        let text = SAMPLE.replace("audio.app_volume_toggle", "audio.unknown");
        assert!(parse(&text).unwrap_err().0.contains("invalid YAML"));
    }
}
