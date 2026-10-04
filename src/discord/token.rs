//! The `OAuth2` token windows-link holds for the Discord app, kept in
//! `%LOCALAPPDATA%\windows-link\discord-token.json` and refreshed before it expires.

use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::secrets::DiscordApp;

const TOKEN_URL: &str = "https://discord.com/api/oauth2/token";
/// Must match a redirect registered for the app (Developer Portal → `OAuth2`).
pub const REDIRECT_URI: &str = "http://127.0.0.1";
/// Refresh a token this long before it expires (they last 7 days).
const REFRESH_MARGIN_SECS: u64 = 24 * 60 * 60;

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix time in seconds.
    pub expires_at: u64,
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Token")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl Token {
    pub fn needs_refresh(&self, now: u64) -> bool {
        self.expires_at <= now.saturating_add(REFRESH_MARGIN_SECS)
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Read Discord's token endpoint answer.
pub fn from_response(json: &Value, now: u64) -> Result<Token, String> {
    let text = |key: &str| {
        json[key]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("token response has no {key}"))
    };
    let expires_in = json["expires_in"]
        .as_u64()
        .ok_or("token response has no expires_in")?;
    Ok(Token {
        access_token: text("access_token")?,
        refresh_token: text("refresh_token")?,
        expires_at: now.saturating_add(expires_in),
    })
}

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map_or_else(|| PathBuf::from("."), PathBuf::from);
    base.join("windows-link").join("discord-token.json")
}

pub fn load(path: &Path) -> Option<Token> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save(path: &Path, token: &Token) -> Result<(), String> {
    let text = serde_json::to_string(token).map_err(|err| err.to_string())?;
    std::fs::write(path, text).map_err(|err| format!("cannot write {}: {err}", path.display()))
}

pub fn forget(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[derive(Debug, PartialEq, Eq)]
pub enum GrantError {
    /// Discord refused the code or refresh token; a new authorization is needed.
    Rejected(String),
    Failed(String),
}

impl std::fmt::Display for GrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(message) | Self::Failed(message) => f.write_str(message),
        }
    }
}

/// Trade an authorization code from `AUTHORIZE` for a token. Blocking.
pub fn exchange(app: &DiscordApp, code: &str) -> Result<Token, GrantError> {
    grant(
        app,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", REDIRECT_URI),
        ],
    )
}

/// Renew a token before it expires. Blocking.
pub fn refresh(app: &DiscordApp, token: &Token) -> Result<Token, GrantError> {
    grant(
        app,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", token.refresh_token.as_str()),
        ],
    )
}

fn grant(app: &DiscordApp, fields: &[(&str, &str)]) -> Result<Token, GrantError> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into();
    let mut form = vec![
        ("client_id", app.client_id.as_str()),
        ("client_secret", app.client_secret.as_str()),
    ];
    form.extend_from_slice(fields);
    let mut response = agent
        .post(TOKEN_URL)
        .send_form(form)
        .map_err(|err| GrantError::Failed(format!("token request failed: {err}")))?;
    let status = response.status().as_u16();
    let body: Value = response
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_string()
        .map_err(|err| err.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|err| err.to_string()))
        .map_err(|err| GrantError::Failed(format!("token response unreadable: {err}")))?;
    match status {
        200 => from_response(&body, now()).map_err(GrantError::Failed),
        400 | 401 => Err(GrantError::Rejected(format!(
            "Discord refused the token request: {}",
            body["error"].as_str().unwrap_or("unknown")
        ))),
        _ => Err(GrantError::Failed(format!(
            "Discord answered the token request with HTTP {status}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Token, from_response, load, save};

    #[test]
    fn reads_the_token_response_into_an_expiry_time() {
        let response = json!({ "access_token": "a", "refresh_token": "r", "expires_in": 604_800, "scope": "rpc", "token_type": "Bearer" });
        let token = from_response(&response, 1_000).unwrap();
        assert_eq!(token.expires_at, 605_800);
        assert_eq!(
            (token.access_token.as_str(), token.refresh_token.as_str()),
            ("a", "r")
        );
        assert!(from_response(&json!({ "access_token": "a" }), 0).is_err());
    }

    #[test]
    fn refreshes_within_a_day_of_expiry() {
        let token = Token {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 100_000,
        };
        assert!(!token.needs_refresh(100_000 - 86_401));
        assert!(token.needs_refresh(100_000 - 86_400));
        assert!(token.needs_refresh(200_000));
    }

    #[test]
    fn saves_and_loads_and_never_prints_the_secrets() {
        let path =
            std::env::temp_dir().join(format!("windows-link-token-{}.json", std::process::id()));
        let token = Token {
            access_token: "secret-a".into(),
            refresh_token: "secret-r".into(),
            expires_at: 9,
        };
        save(&path, &token).unwrap();
        assert_eq!(load(&path), Some(token.clone()));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(load(&path), None);
        assert!(!format!("{token:?}").contains("secret"));
    }
}
