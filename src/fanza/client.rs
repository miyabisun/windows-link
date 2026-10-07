//! Talking to FANZA with the kept cookies. Redirects are followed one at a time so every
//! cookie DMM sets is taken in and saved; when the site's own session has ended, the
//! login cookie signs in again on the way (DMM's own pages do the same); when that no
//! longer works, only the user can sign in again, through the panel.

use std::{
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

use super::session::{self, Cookie};

const LIBRARY: &str = "https://dlsoft.dmm.co.jp/library/";
const TIMEOUT: Duration = Duration::from_secs(30);
const LIMIT: u64 = 32 * 1024 * 1024;
/// The redirects one request may take (DMM's sign-in takes three).
const HOPS: usize = 10;
/// As the panel's login window (Edge's `WebView2`) says, so DMM sees the same browser.
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0";

#[derive(Debug, PartialEq, Eq)]
pub enum FanzaError {
    /// The user has to sign in again through the panel.
    SignIn,
    Failed(String),
}

impl std::fmt::Display for FanzaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SignIn => f.write_str("FANZA needs signing in again through the panel"),
            Self::Failed(reason) => f.write_str(reason),
        }
    }
}

fn failed(err: impl std::fmt::Display) -> FanzaError {
    FanzaError::Failed(err.to_string())
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Whether a page is DMM's login page, where only the user can go on.
fn login_page(url: &ureq::http::Uri) -> bool {
    url.host() == Some("accounts.dmm.co.jp") && url.path().starts_with("/service/login")
}

/// What one request came to after its redirects.
struct Reply {
    url: ureq::http::Uri,
    response: ureq::http::Response<ureq::Body>,
}

pub struct Client {
    path: PathBuf,
    cookies: Mutex<Vec<Cookie>>,
    /// Held through each request with its redirects: DMM replaces its login cookie as
    /// it is used, so two requests at once would race to use the old one.
    turn: Mutex<()>,
    /// Signing in again failed: only the user can, through the panel.
    signed_out: AtomicBool,
    agent: ureq::Agent,
}

impl Client {
    /// The client with the cookies kept at `path`.
    pub fn open(path: PathBuf) -> Self {
        let cookies = session::load(&path);
        Self {
            path,
            cookies: Mutex::new(cookies),
            turn: Mutex::new(()),
            signed_out: AtomicBool::new(false),
            agent: ureq::Agent::config_builder()
                .https_only(true)
                .timeout_global(Some(TIMEOUT))
                .max_redirects(0)
                .http_status_as_error(false)
                .user_agent(USER_AGENT)
                .build()
                .into(),
        }
    }

    fn cookies(&self) -> std::sync::MutexGuard<'_, Vec<Cookie>> {
        self.cookies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether FANZA can be used without the user signing in again.
    pub fn signed_in(&self) -> bool {
        !self.signed_out.load(Ordering::Relaxed) && session::signed_in(&self.cookies(), now())
    }

    /// Keep the cookies the panel's login window got (DMM's only), in place of the old
    /// ones. Returns how many were kept.
    pub fn replace(&self, cookies: Vec<Cookie>) -> Result<usize, String> {
        let mut kept: Vec<Cookie> = cookies
            .into_iter()
            .filter(|cookie| session::is_dmm(&cookie.domain) && session::well_formed(cookie))
            .map(|cookie| Cookie {
                domain: cookie.domain.trim_start_matches('.').to_ascii_lowercase(),
                ..cookie
            })
            .collect();
        if !session::signed_in(&kept, now()) {
            return Err("the cookies do not sign in to DMM".into());
        }
        // FANZA's pages ask adults only once.
        if !kept.iter().any(|c| c.name == "age_check_done") {
            kept.push(Cookie {
                name: "age_check_done".into(),
                value: "1".into(),
                domain: "dmm.co.jp".into(),
                path: "/".into(),
                expires: None,
                secure: false,
                http_only: true,
            });
        }
        let _turn = self.turn.lock();
        session::save(&self.path, &kept)?;
        let count = kept.len();
        *self.cookies() = kept;
        self.signed_out.store(false, Ordering::Relaxed);
        Ok(count)
    }

    /// GET `url`, following redirects when `follow`, taking in and saving every cookie.
    fn get(&self, url: &str, follow: bool) -> Result<Reply, FanzaError> {
        let _turn = self
            .turn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut url: ureq::http::Uri = url.parse().map_err(failed)?;
        for _ in 0..HOPS {
            let host = url.host().unwrap_or_default().to_owned();
            let cookie = session::header(&self.cookies(), &host, url.path(), now());
            let xsrf = self
                .cookies()
                .iter()
                .find(|c| c.name == "XSRF-TOKEN" && c.domain == host)
                .and_then(|c| super::api::percent_decoded(&c.value));
            let mut request = self
                .agent
                .get(&url)
                .header("Cookie", cookie)
                .header("Accept", "application/json, text/html")
                .header("Accept-Language", "ja")
                .header("X-Requested-With", "XMLHttpRequest");
            if let Some(xsrf) = xsrf {
                request = request.header("X-XSRF-TOKEN", xsrf);
            }
            let response = request.call().map_err(failed)?;
            {
                let mut cookies = self.cookies();
                for line in response.headers().get_all("set-cookie") {
                    if let Ok(line) = line.to_str() {
                        session::absorb(&mut cookies, &host, line, now());
                    }
                }
                session::save(&self.path, &cookies).map_err(FanzaError::Failed)?;
            }
            let next = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .filter(|_| follow && response.status().is_redirection())
                .map(|location| resolve(&url, location))
                .transpose()?;
            match next {
                Some(next) => url = next,
                None => return Ok(Reply { url, response }),
            }
        }
        Err(failed(format!("{url}: too many redirects")))
    }

    /// Sign in again from the login cookie, as opening the library page does.
    fn sign_in_again(&self) -> Result<(), FanzaError> {
        let reply = self.get(LIBRARY, true)?;
        if login_page(&reply.url) {
            self.signed_out.store(true, Ordering::Relaxed);
            return Err(FanzaError::SignIn);
        }
        if reply.response.status().is_success() {
            Ok(())
        } else {
            Err(failed(format!(
                "FANZA's library: HTTP {}",
                reply.response.status()
            )))
        }
    }

    /// A JSON answer from FANZA's library API at `path` (with its query), signing in again
    /// once when the site's session has ended.
    pub fn api(&self, path: &str) -> Result<Value, FanzaError> {
        if !self.signed_in() {
            return Err(FanzaError::SignIn);
        }
        let url = format!("https://dlsoft.dmm.co.jp{path}");
        let mut reply = self.get(&url, true)?;
        if reply.response.status() == 401 || login_page(&reply.url) {
            self.sign_in_again()?;
            reply = self.get(&url, true)?;
        }
        if login_page(&reply.url) {
            self.signed_out.store(true, Ordering::Relaxed);
            return Err(FanzaError::SignIn);
        }
        let status = reply.response.status();
        let text = reply
            .response
            .body_mut()
            .with_config()
            .limit(LIMIT)
            .read_to_string()
            .map_err(failed)?;
        let json: Value = serde_json::from_str(&text)
            .map_err(|err| failed(format!("{path}: HTTP {status}: {err}")))?;
        Ok(json)
    }

    /// Where a download link on `dlsoft.dmm.co.jp` leads: the file on DMM's servers,
    /// fetched without cookies.
    pub fn download_url(&self, path: &str) -> Result<String, FanzaError> {
        if !self.signed_in() {
            return Err(FanzaError::SignIn);
        }
        let url = format!("https://dlsoft.dmm.co.jp{path}");
        for attempt in 0..2 {
            let reply = self.get(&url, false)?;
            let location = reply
                .response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .map(|location| resolve(&reply.url, location))
                .transpose()?;
            match location {
                Some(file)
                    if file.host() != Some("accounts.dmm.co.jp")
                        && file.host() != Some("dlsoft.dmm.co.jp") =>
                {
                    return Ok(file.to_string());
                }
                // Sent to sign in: the site's session has ended.
                Some(_) if attempt == 0 => self.sign_in_again()?,
                _ => {
                    return Err(failed(format!(
                        "{path}: FANZA gave no file (HTTP {})",
                        reply.response.status()
                    )));
                }
            }
        }
        Err(FanzaError::SignIn)
    }
}

/// A `Location` header against the page that gave it.
fn resolve(base: &ureq::http::Uri, location: &str) -> Result<ureq::http::Uri, FanzaError> {
    if location.starts_with("https://") || location.starts_with("http://") {
        return location.parse().map_err(failed);
    }
    let host = base
        .host()
        .ok_or_else(|| failed("a redirect without a site"))?;
    let path = if location.starts_with('/') {
        location.to_owned()
    } else {
        let dir = base.path().rsplit_once('/').map_or("", |(dir, _)| dir);
        format!("{dir}/{location}")
    };
    format!("https://{host}{path}").parse().map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::{login_page, resolve};

    #[test]
    fn redirects_resolve_against_the_page_that_gave_them() {
        let page: ureq::http::Uri = "https://dlsoft.dmm.co.jp/download/?filePath=x"
            .parse()
            .unwrap();
        let to = |location: &str| resolve(&page, location).unwrap().to_string();
        assert_eq!(
            to("https://cdn001-paid-contents.games.dmm.com/bb/pcgame/a.zip?sig=1"),
            "https://cdn001-paid-contents.games.dmm.com/bb/pcgame/a.zip?sig=1"
        );
        assert_eq!(to("/library/"), "https://dlsoft.dmm.co.jp/library/");
        assert_eq!(to("next?x=1"), "https://dlsoft.dmm.co.jp/download/next?x=1");
    }

    #[test]
    fn dmms_login_page_is_where_only_the_user_goes_on() {
        let at = |url: &str| login_page(&url.parse().unwrap());
        assert!(at(
            "https://accounts.dmm.co.jp/service/login/password?path=x"
        ));
        assert!(!at("https://dlsoft.dmm.co.jp/library/"));
        assert!(!at("https://accounts.dmm.co.jp/service/other"));
    }
}
