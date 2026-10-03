//! Machine-local YAML configuration: device aliases and the button list.

use std::{
    collections::{BTreeMap, HashSet},
    env, fmt,
    path::{Path, PathBuf},
};

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Alias -> audio endpoint ID (see `windows-link devices`).
    #[serde(default)]
    pub devices: BTreeMap<String, String>,
    #[serde(default)]
    pub buttons: Vec<ButtonConfig>,
}

#[derive(Debug, Deserialize)]
pub struct ButtonConfig {
    pub id: String,
    pub label: String,
    #[serde(flatten)]
    pub spec: ButtonSpec,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ButtonSpec {
    /// Toggle the Windows default output between two device aliases.
    #[serde(rename = "audio.output_toggle")]
    OutputToggle { devices: [String; 2] },
    /// Toggle one process's session volume between two levels (0.0-1.0).
    #[serde(rename = "audio.app_volume_toggle")]
    AppVolumeToggle { process: String, levels: [f32; 2] },
}

impl ButtonSpec {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::OutputToggle { .. } => "audio.output_toggle",
            Self::AppVolumeToggle { .. } => "audio.app_volume_toggle",
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
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(err) => Err(ConfigError(format!(
            "cannot read {}: {err}",
            path.display()
        ))),
    }
}

pub fn parse(text: &str) -> Result<Config, ConfigError> {
    let config: Config =
        serde_norway::from_str(text).map_err(|err| ConfigError(format!("invalid YAML: {err}")))?;
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
            ButtonSpec::OutputToggle { devices } => {
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
            ButtonSpec::OutputToggle { devices } if devices == &["motu".to_owned(), "jbl".to_owned()]
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
    fn rejects_unknown_button_type() {
        let text = SAMPLE.replace("audio.app_volume_toggle", "audio.unknown");
        assert!(parse(&text).unwrap_err().0.contains("invalid YAML"));
    }
}
