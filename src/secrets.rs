//! `%LOCALAPPDATA%\windows-link\secrets.yaml` (or `WINDOWS_LINK_SECRETS`): credentials
//! for outside services, one section per service.
//!
//! ```yaml
//! discord:
//!   client_id: 1234567890123456789
//!   client_secret: abcdef
//! steam:
//!   api_key: 0123456789ABCDEF0123456789ABCDEF
//! ```

use std::{
    env, fmt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Deserializer, de};

#[derive(Debug, Default, Deserialize)]
pub struct Secrets {
    #[serde(default)]
    pub discord: Option<DiscordApp>,
    #[serde(default)]
    pub steam: Option<SteamKey>,
}

/// A Steam Web API key (<https://steamcommunity.com/dev/apikey>), to list owned games.
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SteamKey {
    #[serde(deserialize_with = "id_text")]
    pub api_key: String,
}

impl fmt::Debug for SteamKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SteamKey").finish_non_exhaustive()
    }
}

/// The Discord application windows-link signs in as (Developer Portal → `OAuth2`).
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiscordApp {
    #[serde(deserialize_with = "id_text")]
    pub client_id: String,
    #[serde(deserialize_with = "id_text")]
    pub client_secret: String,
}

impl fmt::Debug for DiscordApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscordApp")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// IDs and keys are often pasted without quotes, which YAML may read as a number.
pub fn id_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Text(String),
        Number(u64),
    }
    let text = match Raw::deserialize(deserializer)? {
        Raw::Text(text) => text.trim().to_owned(),
        Raw::Number(number) => number.to_string(),
    };
    if text.is_empty() {
        return Err(de::Error::custom("must not be empty"));
    }
    Ok(text)
}

pub fn default_path() -> PathBuf {
    if let Some(path) = env::var_os("WINDOWS_LINK_SECRETS") {
        return PathBuf::from(path);
    }
    let base = env::var_os("LOCALAPPDATA").map_or_else(|| PathBuf::from("."), PathBuf::from);
    base.join("windows-link").join("secrets.yaml")
}

pub fn parse(text: &str) -> Result<Secrets, String> {
    if text.trim().is_empty() {
        return Ok(Secrets::default());
    }
    serde_norway::from_str(text).map_err(|err| format!("invalid YAML: {err}"))
}

/// A missing file means no secrets; the services that need them say so.
pub fn load(path: &Path) -> Result<Secrets, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map_err(|err| format!("{}: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Secrets::default()),
        Err(err) => Err(format!("cannot read {}: {err}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn reads_the_discord_app_quoted_or_not() {
        let secrets =
            parse("discord:\n  client_id: 1424242424242424242\n  client_secret: s3cr3t\n").unwrap();
        let app = secrets.discord.unwrap();
        assert_eq!(app.client_id, "1424242424242424242");
        assert_eq!(app.client_secret, "s3cr3t");

        let quoted = parse("discord:\n  client_id: \"42\"\n  client_secret: \"x\"\n").unwrap();
        assert_eq!(quoted.discord.unwrap().client_id, "42");
    }

    #[test]
    fn an_empty_file_or_missing_section_has_no_discord_app() {
        assert!(parse("").unwrap().discord.is_none());
        assert!(parse("# nothing yet\n").unwrap().discord.is_none());
        assert!(parse("steam:\n  api_key: x\n").unwrap().discord.is_none());
    }

    #[test]
    fn rejects_blank_or_misspelled_values() {
        assert!(parse("discord:\n  client_id: \"\"\n  client_secret: x\n").is_err());
        assert!(parse("discord:\n  client_id: 1\n").is_err());
        assert!(parse("discord:\n  clientid: 1\n  client_secret: x\n").is_err());
    }

    #[test]
    fn reads_the_steam_api_key_and_hides_it_from_debug_output() {
        let secrets = parse("steam:\n  api_key: 0123ABCD\n").unwrap();
        let steam = secrets.steam.unwrap();
        assert_eq!(steam.api_key, "0123ABCD");
        assert!(!format!("{steam:?}").contains("0123ABCD"));
        assert!(parse("").unwrap().steam.is_none());
        assert!(parse("steam:\n  apikey: x\n").is_err());
        assert!(parse("steam:\n  api_key: \"\"\n").is_err());
    }

    #[test]
    fn debug_output_hides_the_secret() {
        let secrets = parse("discord:\n  client_id: 1\n  client_secret: hunter2\n").unwrap();
        assert!(!format!("{:?}", secrets.discord.unwrap()).contains("hunter2"));
    }
}
