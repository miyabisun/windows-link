//! FANZA's sign-in: the cookies DMM gave the panel's login window, kept in
//! `%LOCALAPPDATA%\windows-link\fanza-session.json`. DMM replaces its login cookies as
//! they are used, so every cookie it sets is taken in and saved at once.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A cookie as the panel hands it over (`PUT /fanza/session`) and as it is kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// The site it belongs to, sent to its subdomains too (`dmm.co.jp`).
    pub domain: String,
    #[serde(default = "root_path")]
    pub path: String,
    /// Unix time; `None` lasts until DMM replaces it.
    #[serde(default)]
    pub expires: Option<i64>,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub http_only: bool,
}

fn root_path() -> String {
    "/".to_owned()
}

#[derive(Serialize, Deserialize)]
struct Saved {
    cookies: Vec<Cookie>,
}

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map_or_else(|| PathBuf::from("."), PathBuf::from);
    base.join("windows-link").join("fanza-session.json")
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

/// Whether a cookie's domain is DMM's (`dmm.co.jp` or below it).
pub fn is_dmm(domain: &str) -> bool {
    let bare = domain.trim_start_matches('.').to_ascii_lowercase();
    bare == "dmm.co.jp" || bare.ends_with(".dmm.co.jp")
}

/// Whether the cookies sign in to DMM: a login cookie that has not expired.
pub fn signed_in(cookies: &[Cookie], now: i64) -> bool {
    cookies.iter().any(|cookie| {
        matches!(cookie.name.as_str(), "login_secure_id" | "login_session_id")
            && cookie.expires.is_none_or(|at| at > now)
    })
}

fn sent_to(cookie: &Cookie, host: &str, path: &str) -> bool {
    let domain = cookie.domain.as_str();
    let site = host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|rest| rest.ends_with('.'));
    let under = path.starts_with(&cookie.path)
        && (cookie.path.ends_with('/')
            || path.len() == cookie.path.len()
            || path[cookie.path.len()..].starts_with('/'));
    site && under
}

/// The `Cookie` header for a request to `host` at `path`, leaving out expired cookies.
pub fn header(cookies: &[Cookie], host: &str, path: &str, now: i64) -> String {
    cookies
        .iter()
        .filter(|c| c.expires.is_none_or(|at| at > now) && sent_to(c, host, path))
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Take in a `Set-Cookie` header that `host` sent: a new or changed cookie, or the end
/// of one (expired, or `Max-Age` zero).
pub fn absorb(cookies: &mut Vec<Cookie>, host: &str, set_cookie: &str, now: i64) {
    let mut parts = set_cookie.split(';');
    let Some((name, value)) = parts.next().and_then(|pair| pair.split_once('=')) else {
        return;
    };
    let mut cookie = Cookie {
        name: name.trim().to_owned(),
        value: value.trim().to_owned(),
        domain: host.to_ascii_lowercase(),
        path: root_path(),
        expires: None,
        secure: false,
        http_only: false,
    };
    let mut max_age = None;
    for part in parts {
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "domain" if !value.is_empty() => {
                cookie.domain = value.trim_start_matches('.').to_ascii_lowercase();
            }
            "path" if value.starts_with('/') => value.clone_into(&mut cookie.path),
            "expires" => cookie.expires = http_date(value).or(cookie.expires),
            "max-age" => max_age = value.parse::<i64>().ok(),
            "secure" => cookie.secure = true,
            "httponly" => cookie.http_only = true,
            _ => {}
        }
    }
    if let Some(seconds) = max_age {
        cookie.expires = Some(now.saturating_add(seconds));
    }
    let same =
        |c: &Cookie| c.name == cookie.name && c.domain == cookie.domain && c.path == cookie.path;
    if cookie.expires.is_some_and(|at| at <= now) {
        cookies.retain(|c| !same(c));
    } else if let Some(old) = cookies.iter_mut().find(|c| same(c)) {
        *old = cookie;
    } else {
        cookies.push(cookie);
    }
}

/// A date in an HTTP header, such as `Wed, 06 Oct 2027 17:00:00 GMT`, as Unix time.
pub fn http_date(text: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let date = text.split_once(',').map_or(text, |(_, rest)| rest);
    let mut fields = date.split([' ', '-']).filter(|f| !f.is_empty());
    let day: i64 = fields.next()?.parse().ok()?;
    let month = fields.next()?.to_ascii_lowercase();
    let month = MONTHS.iter().position(|m| month.starts_with(m))?;
    let year: i64 = fields.next()?.parse().ok()?;
    let year = match year {
        0..70 => 2000 + year,
        70..100 => 1900 + year,
        _ => year,
    };
    let mut clock = fields.next()?.split(':').map(str::parse::<i64>);
    let (hour, minute, second) = (
        clock.next()?.ok()?,
        clock.next()?.ok()?,
        clock.next()?.ok()?,
    );
    // Days since 1970-01-01, by Howard Hinnant's days_from_civil.
    let month = i64::try_from(month).ok()? + 1;
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let days = era * 146_097 + of_era * 365 + of_era / 4 - of_era / 100 + of_year - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

#[cfg(test)]
mod tests {
    use super::{Cookie, absorb, header, http_date, is_dmm, signed_in};

    fn cookie(name: &str, value: &str, domain: &str, expires: Option<i64>) -> Cookie {
        Cookie {
            name: name.into(),
            value: value.into(),
            domain: domain.into(),
            path: "/".into(),
            expires,
            secure: true,
            http_only: true,
        }
    }

    const NOW: i64 = 1_791_000_000;

    #[test]
    fn reads_http_dates() {
        assert_eq!(
            http_date("Wed, 06 Oct 2027 17:00:00 GMT"),
            Some(1_822_842_000)
        );
        assert_eq!(http_date("Thu, 01 Jan 1970 00:00:01 GMT"), Some(1));
        // The older form some servers still write.
        assert_eq!(
            http_date("Wednesday, 06-Oct-27 17:00:00 GMT"),
            Some(1_822_842_000)
        );
        assert_eq!(
            http_date("Wed, 06-Oct-2027 17:00:00 GMT"),
            Some(1_822_842_000)
        );
        assert_eq!(http_date("tomorrow"), None);
    }

    #[test]
    fn sends_a_sites_cookies_to_it_and_below_it_but_not_expired_ones() {
        let cookies = [
            cookie("login_secure_id", "a", "dmm.co.jp", Some(NOW + 60)),
            cookie("laravel_session", "b", "dlsoft.dmm.co.jp", None),
            cookie("old", "c", "dmm.co.jp", Some(NOW - 1)),
            cookie("other", "d", "accounts.dmm.co.jp", None),
            Cookie {
                path: "/dc".into(),
                ..cookie("ec_session", "e", "www.dmm.co.jp", None)
            },
        ];
        assert_eq!(
            header(&cookies, "dlsoft.dmm.co.jp", "/ajax/v1/library", NOW),
            "login_secure_id=a; laravel_session=b"
        );
        assert_eq!(
            header(&cookies, "accounts.dmm.co.jp", "/service/login/token", NOW),
            "login_secure_id=a; other=d"
        );
        assert_eq!(
            header(&cookies, "www.dmm.co.jp", "/", NOW),
            "login_secure_id=a"
        );
        assert_eq!(
            header(&cookies, "www.dmm.co.jp", "/dc/doujin", NOW),
            "login_secure_id=a; ec_session=e"
        );
        // A look-alike site gets nothing.
        assert_eq!(header(&cookies, "evildmm.co.jp", "/", NOW), "");
    }

    #[test]
    fn takes_in_new_changed_and_ended_cookies() {
        let mut cookies = vec![
            cookie("login_secure_id", "old", "dmm.co.jp", Some(NOW + 60)),
            cookie("guest_id", "g", "dlsoft.dmm.co.jp", None),
        ];
        // DMM replaces its login cookie for the whole site.
        absorb(
            &mut cookies,
            "accounts.dmm.co.jp",
            "login_secure_id=new; expires=Wed, 06 Oct 2027 17:00:00 GMT; Max-Age=31536000; path=/; domain=.dmm.co.jp; secure; HttpOnly",
            NOW,
        );
        // A cookie for the answering site only.
        absorb(
            &mut cookies,
            "dlsoft.dmm.co.jp",
            "XSRF-TOKEN=x%3D; path=/; secure",
            NOW,
        );
        // Ended by a date in the past, and by Max-Age zero.
        absorb(
            &mut cookies,
            "dlsoft.dmm.co.jp",
            "guest_id=deleted; expires=Thu, 01 Jan 1970 00:00:01 GMT; path=/",
            NOW,
        );
        assert_eq!(
            cookies,
            [
                Cookie {
                    expires: Some(NOW + 31_536_000),
                    ..cookie("login_secure_id", "new", "dmm.co.jp", None)
                },
                Cookie {
                    http_only: false,
                    ..cookie("XSRF-TOKEN", "x%3D", "dlsoft.dmm.co.jp", None)
                },
            ]
        );
        absorb(
            &mut cookies,
            "dlsoft.dmm.co.jp",
            "XSRF-TOKEN=; Max-Age=0",
            NOW,
        );
        assert_eq!(cookies.len(), 1);
        // Nonsense changes nothing.
        absorb(&mut cookies, "dlsoft.dmm.co.jp", "no equals sign", NOW);
        assert_eq!(cookies.len(), 1);
    }

    #[test]
    fn signed_in_while_a_login_cookie_lasts() {
        let login = |expires| vec![cookie("login_secure_id", "a", "dmm.co.jp", expires)];
        assert!(signed_in(&login(Some(NOW + 1)), NOW));
        assert!(signed_in(&login(None), NOW));
        assert!(!signed_in(&login(Some(NOW - 1)), NOW));
        assert!(!signed_in(
            &[cookie("guest_id", "g", "dmm.co.jp", None)],
            NOW
        ));
    }

    #[test]
    fn only_dmms_cookies_are_kept() {
        assert!(is_dmm("dmm.co.jp"));
        assert!(is_dmm(".dmm.co.jp"));
        assert!(is_dmm("dlsoft.dmm.co.jp"));
        assert!(!is_dmm("dmm.com"));
        assert!(!is_dmm("evildmm.co.jp"));
    }

    #[test]
    fn cookies_that_would_break_the_header_are_refused() {
        use super::well_formed;

        let ok = cookie("XSRF-TOKEN", "eyJpdiI6%3D%3D", "dmm.co.jp", None);
        assert!(well_formed(&ok));
        for (name, value) in [
            ("a", "b; injected=1"),
            ("a;b", "c"),
            ("", "c"),
            ("a b", "c"),
            ("a", "line\r\nbreak"),
        ] {
            assert!(
                !well_formed(&cookie(name, value, "dmm.co.jp", None)),
                "{name:?}={value:?}"
            );
        }
    }
}
