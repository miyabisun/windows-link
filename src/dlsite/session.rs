//! DLsite's sign-in: the cookies DLsite gave the panel's login window, kept in
//! `%LOCALAPPDATA%\windows-link\dlsite-session.json`. DLsite's login page stops programs
//! (a Cloudflare check since 2026-10-18), but DLsite Play does not: each use puts the
//! cookies in a fresh connection and checks DLsite Play's authorization first, going
//! through DLsite's login once when it has lapsed. Values DLsite changed are saved.

use std::{
    fmt::Write as _,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

use super::play::{self, Session};
use crate::shop::cookies::{self, Cookie};

#[derive(Debug, PartialEq, Eq)]
pub enum DlsiteError {
    /// The user has to sign in again through the panel.
    SignIn,
    Failed(String),
}

impl std::fmt::Display for DlsiteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SignIn => f.write_str("DLsite needs signing in again through the panel"),
            Self::Failed(reason) => f.write_str(reason),
        }
    }
}

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map_or_else(|| PathBuf::from("."), PathBuf::from);
    base.join("windows-link").join("dlsite-session.json")
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// A kept cookie as a connection takes it in: its `Set-Cookie` line and the page that
/// sent it. `None` once it has expired.
pub fn set_cookie(cookie: &Cookie, now: i64) -> Option<(String, String)> {
    let mut line = format!(
        "{}={}; Domain={}; Path={}",
        cookie.name, cookie.value, cookie.domain, cookie.path
    );
    if let Some(at) = cookie.expires {
        if at <= now {
            return None;
        }
        let _ = write!(line, "; Max-Age={}", at - now);
    }
    if cookie.secure {
        line.push_str("; Secure");
    }
    if cookie.http_only {
        line.push_str("; HttpOnly");
    }
    Some((line, format!("https://{}{}", cookie.domain, cookie.path)))
}

/// Whether DLsite Play's `/api/authorize` answer is a signed-in user: it answers `[]`
/// (or `{}`) to a visitor, as DLsite Play's own page reads it.
pub fn authorized(answer: &Value) -> bool {
    answer.as_object().is_some_and(|user| !user.is_empty())
}

pub struct DlsiteSession {
    path: PathBuf,
    cookies: Mutex<Vec<Cookie>>,
    /// Going through the login failed: only the user can sign in, through the panel.
    signed_out: AtomicBool,
}

impl DlsiteSession {
    /// The session with the cookies kept at `path`.
    pub fn open(path: PathBuf) -> Self {
        let cookies = cookies::load(&path);
        Self {
            path,
            cookies: Mutex::new(cookies),
            signed_out: AtomicBool::new(false),
        }
    }

    fn cookies(&self) -> std::sync::MutexGuard<'_, Vec<Cookie>> {
        self.cookies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether DLsite can be used without the user signing in again.
    pub fn signed_in(&self) -> bool {
        !self.signed_out.load(Ordering::Relaxed) && !self.cookies().is_empty()
    }

    /// Keep the cookies the panel's login window got (DLsite's only), in place of the
    /// old ones. Returns how many were kept.
    pub fn keep(&self, cookies: Vec<Cookie>) -> Result<usize, String> {
        let kept: Vec<Cookie> = cookies
            .into_iter()
            .filter(|c| cookies::on_site(&c.domain, "dlsite.com") && cookies::well_formed(c))
            .map(|c| Cookie {
                domain: c.domain.trim_start_matches('.').to_ascii_lowercase(),
                ..c
            })
            .collect();
        if kept.is_empty() {
            return Err("no DLsite cookies came from the login window".into());
        }
        cookies::save(&self.path, &kept)?;
        let count = kept.len();
        *self.cookies() = kept;
        self.signed_out.store(false, Ordering::Relaxed);
        Ok(count)
    }

    /// A signed-in connection to DLsite from the kept cookies.
    pub fn resume(&self) -> Result<Session, DlsiteError> {
        if !self.signed_in() {
            return Err(DlsiteError::SignIn);
        }
        let agent = play::agent();
        {
            let mut jar = agent.cookie_jar_lock();
            for cookie in self.cookies().iter() {
                let Some((line, page)) = set_cookie(cookie, now()) else {
                    continue;
                };
                if let Ok(page) = page.parse::<ureq::http::Uri>()
                    && let Ok(cookie) = ureq::Cookie::parse(line, &page)
                {
                    let _ = jar.insert(cookie, &page);
                }
            }
        }
        let session = Session::new(agent);
        // A lapsed session is renewed through DLsite's login, which lets a signed-in
        // visitor through; a login page means only the user can sign in again.
        let signed = session.authorized().map_err(DlsiteError::Failed)?
            || (session.enter() && session.authorized().map_err(DlsiteError::Failed)?);
        if !signed {
            self.signed_out.store(true, Ordering::Relaxed);
            return Err(DlsiteError::SignIn);
        }
        self.keep_changes(&session);
        Ok(session)
    }

    /// Save the values DLsite changed in the connection's cookies.
    fn keep_changes(&self, session: &Session) {
        let mut kept = self.cookies();
        let mut changed = false;
        {
            let jar = session.cookie_jar();
            for cookie in kept.iter_mut() {
                if let Some(now) = jar.get(&cookie.domain, &cookie.path, &cookie.name)
                    && now.value() != cookie.value
                {
                    now.value().clone_into(&mut cookie.value);
                    changed = true;
                }
            }
        }
        if changed && let Err(reason) = cookies::save(&self.path, &kept) {
            tracing::warn!(%reason, "cannot keep the DLsite session");
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{DlsiteError, DlsiteSession, authorized, set_cookie};
    use crate::shop::cookies::Cookie;

    fn cookie(name: &str, domain: &str, expires: Option<i64>) -> Cookie {
        Cookie {
            name: name.into(),
            value: "v".into(),
            domain: domain.into(),
            path: "/".into(),
            expires,
            secure: true,
            http_only: true,
        }
    }

    fn file(tag: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "windows-link-dlsite-session-{tag}-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn a_kept_cookie_goes_back_into_a_connection_until_it_expires() {
        assert_eq!(
            set_cookie(&cookie("__DLsite_SID", "dlsite.com", Some(1_100)), 1_000),
            Some((
                "__DLsite_SID=v; Domain=dlsite.com; Path=/; Max-Age=100; Secure; HttpOnly".into(),
                "https://dlsite.com/".into()
            ))
        );
        // One that lasts the browser's session, under a path.
        let play = Cookie {
            path: "/api".into(),
            secure: false,
            http_only: false,
            ..cookie("play_session", "play.dlsite.com", None)
        };
        assert_eq!(
            set_cookie(&play, 1_000),
            Some((
                "play_session=v; Domain=play.dlsite.com; Path=/api".into(),
                "https://play.dlsite.com/api".into()
            ))
        );
        assert_eq!(
            set_cookie(&cookie("old", "dlsite.com", Some(1_000)), 1_000),
            None
        );
    }

    #[test]
    fn dlsite_plays_authorization_names_a_user_only_when_signed_in() {
        assert!(authorized(&json!({"user_id": 1, "name": "someone"})));
        assert!(!authorized(&json!([])));
        assert!(!authorized(&json!({})));
        assert!(!authorized(&json!(null)));
    }

    #[test]
    fn only_dlsites_cookies_that_fit_a_header_are_kept() {
        let path = file("keep");
        let session = DlsiteSession::open(path.clone());
        assert!(!session.signed_in());
        // Without cookies there is nothing to resume, and no request goes out.
        assert!(matches!(session.resume(), Err(DlsiteError::SignIn)));
        assert!(
            session
                .keep(vec![cookie("tracker", ".example.com", None)])
                .is_err()
        );
        assert!(!session.signed_in());

        let kept = session.keep(vec![
            cookie("__DLsite_SID", ".dlsite.com", Some(4_102_444_800)),
            cookie("play_session", "play.dlsite.com", None),
            cookie("tracker", ".example.com", None),
            cookie("look", "evildlsite.com", None),
            Cookie {
                value: "a b".into(),
                ..cookie("broken", ".dlsite.com", None)
            },
        ]);
        assert_eq!(kept, Ok(2));
        assert!(session.signed_in());
        // Kept for the next start, with the site written plainly.
        let again = DlsiteSession::open(path.clone());
        assert!(again.signed_in());
        let domains: Vec<String> = again.cookies().iter().map(|c| c.domain.clone()).collect();
        assert_eq!(domains, ["dlsite.com", "play.dlsite.com"]);
        let _ = std::fs::remove_file(path);
    }
}
