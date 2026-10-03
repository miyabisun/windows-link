//! HTTPS downloads from the release feed, with size limits on what is read.

use std::time::Duration;

use serde_json::Value;
use ureq::Agent;

const TIMEOUT: Duration = Duration::from_mins(2);

/// HTTPS only, redirects included. Plain HTTP is accepted only for a feed on this
/// machine (`WINDOWS_LINK_UPDATE_URL` pointing at a local test server).
fn agent(url: &str) -> Agent {
    Agent::config_builder()
        .https_only(!is_loopback(url))
        .timeout_global(Some(TIMEOUT))
        .user_agent(concat!("windows-link/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn is_loopback(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match host.find(']') {
        Some(end) => &host[..=end],
        None => host.rsplit_once(':').map_or(host, |(name, _port)| name),
    };
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

pub fn fetch_bytes(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut response = agent(url)
        .get(url)
        .call()
        .map_err(|err| format!("GET {url}: {err}"))?;
    response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|err| format!("GET {url}: {err}"))
}

pub fn fetch_json(url: &str, limit: u64) -> Result<Value, String> {
    let mut response = agent(url)
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|err| match err {
            ureq::Error::StatusCode(404) => format!("GET {url}: no release is published"),
            err => format!("GET {url}: {err}"),
        })?;
    let text = response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_string()
        .map_err(|err| format!("GET {url}: {err}"))?;
    serde_json::from_str(&text).map_err(|err| format!("GET {url}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::is_loopback;

    #[test]
    fn plain_http_is_only_for_this_machine() {
        assert!(is_loopback("http://127.0.0.1:4799/latest"));
        assert!(is_loopback("http://localhost/latest"));
        assert!(is_loopback("http://[::1]:80/x"));
        assert!(is_loopback("http://[::1]/x"));
        assert!(!is_loopback("http://192.168.1.100/latest"));
        assert!(!is_loopback("http://127.0.0.1.example.com/latest"));
        assert!(!is_loopback(
            "https://api.github.com/repos/x/y/releases/latest"
        ));
    }
}
