//! HTTPS requests to the release feed: where its latest release page sends to, and
//! downloads with size limits on what is read.

use std::time::Duration;

use ureq::Agent;

const TIMEOUT: Duration = Duration::from_mins(2);

/// HTTPS only, redirects followed or not. Plain HTTP is accepted only for a feed on
/// this machine (`WINDOWS_LINK_UPDATE_URL` pointing at a local test server).
fn agent(url: &str, redirects: u32) -> Agent {
    Agent::config_builder()
        .https_only(!is_loopback(url))
        .max_redirects(redirects)
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

/// Where `url` sends to: the `Location` of its redirect.
pub fn fetch_location(url: &str) -> Result<String, String> {
    let response = agent(url, 0)
        .get(url)
        .call()
        .map_err(|err| format!("GET {url}: {err}"))?;
    if !response.status().is_redirection() {
        return Err(format!(
            "GET {url}: {} instead of a redirect",
            response.status()
        ));
    }
    response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| format!("GET {url}: a redirect without a location"))
}

pub fn fetch_bytes(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut response = agent(url, 10)
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
        assert!(!is_loopback("https://github.com/x/y/releases/latest"));
    }
}
