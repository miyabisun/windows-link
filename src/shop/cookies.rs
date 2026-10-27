//! A shop's sign-in from the panel: the cookies its login window got (`PUT
//! /{shop}/session`), kept in a file of windows-link's own. The shops' bot checks pass
//! only in a real browser, so windows-link never signs in with a password.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// As the panel's login window (Edge's `WebView2`) says, so the shop sees the same
/// browser its cookies came from.
pub const PANEL_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0";

/// A cookie as the panel hands it over and as it is kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// The site it belongs to, sent to its subdomains too (`dmm.co.jp`).
    pub domain: String,
    #[serde(default = "root_path")]
    pub path: String,
    /// Unix time; `None` lasts until the shop replaces it.
    #[serde(default)]
    pub expires: Option<i64>,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub http_only: bool,
}

pub fn root_path() -> String {
    "/".to_owned()
}

#[derive(Serialize, Deserialize)]
struct Saved {
    cookies: Vec<Cookie>,
}

/// The saved cookies, none when there is no file.
pub fn load(path: &Path) -> Vec<Cookie> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Saved>(&text).ok())
        .map(|saved| saved.cookies)
        .unwrap_or_default()
}

pub fn save(path: &Path, cookies: &[Cookie]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text = serde_json::to_string(&Saved {
        cookies: cookies.to_vec(),
    })
    .map_err(|err| err.to_string())?;
    std::fs::write(path, text).map_err(|err| format!("{}: {err}", path.display()))
}

/// Whether a cookie can go into a `Cookie` header as it is: a name of plain characters,
/// and a value without `;`, spaces or control characters.
pub fn well_formed(cookie: &Cookie) -> bool {
    let plain = |c: char| !c.is_control() && !c.is_whitespace() && c != ';';
    !cookie.name.is_empty()
        && cookie
            .name
            .chars()
            .all(|c| plain(c) && c != '=' && c != ',')
        && cookie.value.chars().all(plain)
}

/// Whether a cookie's domain is `site` or below it (`.dmm.co.jp` for `dmm.co.jp`).
pub fn on_site(domain: &str, site: &str) -> bool {
    let bare = domain.trim_start_matches('.').to_ascii_lowercase();
    bare == site
        || bare
            .strip_suffix(site)
            .is_some_and(|rest| rest.ends_with('.'))
}
