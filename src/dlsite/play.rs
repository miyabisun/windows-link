//! The user's DLsite purchases, through DLsite Play with the session the panel signed in
//! (`super::session`): list the works bought with each work's name, maker, main picture,
//! type, latest version and purchase date, and download a work's files.
//!
//! DLsite Play's authorization follows dlsite-async (MIT,
//! <https://github.com/bhrevol/dlsite-async>). Downloads follow what dlsite-manager (MIT,
//! <https://github.com/AcrylicShrimp/dlsite-manager>) found: DLsite Play's download link
//! leads to the archive, or to a page listing the parts of a split one.

use std::{collections::HashMap, path::Path, time::Duration};

use serde_json::Value;
use ureq::ResponseExt;

pub use crate::shop::Work;
use crate::shop::{cookies::PANEL_USER_AGENT, download};

const TIMEOUT: Duration = Duration::from_secs(30);
const LIMIT: u64 = 32 * 1024 * 1024;
/// How many works DLsite Play describes in one request (its `page_limit`).
const WORKS_PER_REQUEST: usize = 50;

/// HTTPS with a cookie jar, which carries the session across DLsite's sites.
pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .timeout_global(Some(TIMEOUT))
        .user_agent(PANEL_USER_AGENT)
        .build()
        .into()
}

fn text(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    what: &str,
) -> Result<String, String> {
    response
        .map_err(|err| format!("{what}: {err}"))?
        .body_mut()
        .with_config()
        .limit(LIMIT)
        .read_to_string()
        .map_err(|err| format!("{what}: {err}"))
}

fn json(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    what: &str,
) -> Result<Value, String> {
    serde_json::from_str(&text(response, what)?).map_err(|err| format!("{what}: {err}"))
}

/// A signed-in DLsite session, whose cookies let DLsite Play and the downloads in.
pub struct Session {
    agent: ureq::Agent,
}

impl Session {
    /// A session on `agent`, whose cookie jar holds what signs in to DLsite.
    pub fn new(agent: ureq::Agent) -> Self {
        Self { agent }
    }

    pub fn cookie_jar(&self) -> ureq::CookieJar<'_> {
        self.agent.cookie_jar_lock()
    }

    /// Whether DLsite Play knows the user (`/api/authorize`, which also renews its own
    /// session from DLsite's).
    pub fn authorized(&self) -> Result<bool, String> {
        Ok(super::session::authorized(&json(
            self.agent
                .get("https://play.dlsite.com/api/authorize")
                .header("Referer", "https://play.dlsite.com/")
                .call(),
            "DLsite Play's authorization",
        )?))
    }

    /// Go through DLsite's login as DLsite Play does, which lets a visitor DLsite knows
    /// through. False when it stops at a page, such as the login form.
    pub fn enter(&self) -> bool {
        self.agent
            .get("https://play.dlsite.com/login/")
            .call()
            .is_ok_and(|response| response.get_uri().host() == Some("play.dlsite.com"))
    }
}

/// A file of a work's download.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteFile {
    pub name: String,
    pub url: String,
}

impl Session {
    /// Every purchased work, with when it was bought.
    pub fn purchases(&self) -> Result<Vec<Work>, String> {
        let bought = sales(&json(
            self.agent
                .get("https://play.dlsite.com/api/v3/content/sales?last=0")
                .call(),
            "the DLsite purchases",
        )?);
        let ids: Vec<&str> = bought.iter().map(|(id, _)| id.as_str()).collect();
        let when: HashMap<&str, i64> = bought
            .iter()
            .filter_map(|(id, at)| Some((id.as_str(), (*at)?)))
            .collect();
        let mut found = Vec::new();
        for batch in ids.chunks(WORKS_PER_REQUEST) {
            let body = serde_json::to_string(batch).unwrap_or_default();
            found.extend(works(&json(
                self.agent
                    .post("https://play.dlsite.com/api/v3/content/works")
                    .header("Content-Type", "application/json")
                    .send(body),
                "the DLsite works",
            )?));
        }
        for work in &mut found {
            work.bought = when.get(work.id.as_str()).copied();
        }
        Ok(found)
    }

    /// The download pages of a work, in order: one archive, or the parts of a split one.
    /// A work with serial numbers downloads like any other (the numbers are on DLsite).
    /// Each page is `resolve`d just before its file is fetched: DLsite serves the part
    /// whose page was opened last.
    pub fn parts(&self, id: &str) -> Result<Vec<String>, String> {
        let start = self.location(&format!(
            "https://play.dlsite.com/api/v3/download?workno={id}"
        ))?;
        let pages = if start.contains("/download/split/") {
            let parts = split_parts(&text(
                self.agent.get(&start).call(),
                "DLsite's page of a split download",
            )?);
            if parts.is_empty() {
                return Err(format!("DLsite lists no parts for {id}"));
            }
            parts
        } else if start.contains("/serial/") {
            vec![format!(
                "https://www.dlsite.com/home/download/=/product_id/{id}.html"
            )]
        } else {
            vec![start]
        };
        Ok(pages)
    }

    /// The license keys DLsite keeps for a work: its download leads to a page showing
    /// them when it has any.
    pub fn license_keys(&self, id: &str) -> Result<Vec<crate::library::LicenseKey>, String> {
        let start = self.location(&format!(
            "https://play.dlsite.com/api/v3/download?workno={id}"
        ))?;
        if !start.contains("/serial/") {
            return Ok(Vec::new());
        }
        Ok(license_keys(&text(
            self.agent.get(&start).call(),
            "DLsite's page of license keys",
        )?))
    }

    /// The file a download page gives.
    pub fn resolve(&self, page: &str) -> Result<RemoteFile, String> {
        let url = self.location(page)?;
        let name = file_name(&url).ok_or_else(|| format!("{page} does not give a file: {url}"))?;
        Ok(RemoteFile { name, url })
    }

    /// Where `url` leads.
    fn location(&self, url: &str) -> Result<String, String> {
        let response = self
            .agent
            .get(url)
            .config()
            .max_redirects(0)
            .build()
            .call()
            .map_err(|err| format!("{url}: {err}"))?;
        let location = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok());
        match location {
            Some(path) if path.starts_with('/') => Ok(format!("https://www.dlsite.com{path}")),
            Some(url) => Ok(url.to_owned()),
            None => Err(format!("{url}: no download (HTTP {})", response.status())),
        }
    }

    /// Download `file` to `path`, going on from a part already there. `progress`
    /// hears the bytes on disk and the whole size now and then.
    pub fn fetch(
        &self,
        file: &RemoteFile,
        path: &Path,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<(), String> {
        download::fetch(&self.agent, &file.url, &file.name, path, progress)
    }
}

/// Main pictures of works by ID, from DLsite's public product information.
pub fn fetch_public_images(ids: &[String]) -> Result<HashMap<String, String>, String> {
    let agent = agent();
    let mut found = HashMap::new();
    for batch in ids.chunks(50) {
        let url = format!(
            "https://www.dlsite.com/maniax/product/info/ajax?product_id={}&cdn_cache_min=1",
            batch.join(",")
        );
        found.extend(public_images(&json(
            agent.get(&url).call(),
            "DLsite's product information",
        )?));
    }
    Ok(found)
}

/// The license keys on DLsite's page of a work that has them (`/home/serial/…`): the
/// table rows whose heading names a key or serial number.
pub fn license_keys(html: &str) -> Vec<crate::library::LicenseKey> {
    let named = |label: &str| {
        let label = label.to_lowercase();
        ["キー", "シリアル", "serial", "license", "key"]
            .iter()
            .any(|word| label.contains(word))
    };
    html.split("<tr")
        .skip(1)
        .filter_map(|row| {
            let row = &row[..row.find("</tr>").unwrap_or(row.len())];
            let label = cell(row, "th")?;
            let value = cell(row, "td")?;
            (named(&label) && !value.is_empty())
                .then_some(crate::library::LicenseKey { label, value })
        })
        .collect()
}

/// The text of the first `<tag>` cell in a table row, without markup, entities decoded
/// and spaces collapsed.
fn cell(row: &str, tag: &str) -> Option<String> {
    let start = row.find(&format!("<{tag}"))?;
    let open = start + row[start..].find('>')? + 1;
    let close = open + row[open..].find(&format!("</{tag}>"))?;
    let mut text = String::new();
    let mut in_tag = false;
    for c in row[open..close].chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }
    let text = text
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&amp;", "&");
    Some(text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The download links on DLsite's page for a work split into parts, in order.
pub fn split_parts(html: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    for link in html
        .split("href=\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
    {
        if !link.contains("/download/=/number/") {
            continue;
        }
        let link = if link.starts_with('/') {
            format!("https://www.dlsite.com{link}")
        } else {
            link.to_owned()
        };
        if !parts.contains(&link) {
            parts.push(link);
        }
    }
    parts
}

/// The name of the file a download URL serves (`…/file/<name>/…`), when it is a plain
/// file name.
pub fn file_name(url: &str) -> Option<String> {
    let name = url.split("/file/").nth(1)?.split(['/', '?']).next()?;
    let plain =
        !name.is_empty() && name.chars().any(|c| c != '.') && !name.contains(['\\', ':', '\0']);
    plain.then(|| name.to_owned())
}

/// The work IDs in `/api/v3/content/sales`, each with when it was bought (Unix
/// seconds) when DLsite says.
pub fn sales(answer: &Value) -> Vec<(String, Option<i64>)> {
    answer
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|sale| {
            let at = sale["sales_date"]
                .as_str()
                .and_then(super::works::parse_date)
                .and_then(crate::shop::unix_seconds);
            Some((sale["workno"].as_str()?.to_owned(), at))
        })
        .collect()
}

/// A name in Japanese, else in any language given.
fn localized(name: &Value) -> Option<String> {
    name["ja_JP"]
        .as_str()
        .or_else(|| name.as_object()?.values().find_map(Value::as_str))
        .map(str::to_owned)
}

/// The works in `/api/v3/content/works`. Names are in Japanese, else in any language
/// given.
pub fn works(answer: &Value) -> Vec<Work> {
    answer["works"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|work| {
            let date = |key: &str| work[key].as_str().filter(|d| !d.is_empty());
            Some(Work {
                id: work["workno"].as_str()?.to_owned(),
                name: localized(&work["name"]).unwrap_or_default(),
                maker: localized(&work["maker"]["name"]).unwrap_or_default(),
                image: work["work_files"]["main"].as_str().map(str::to_owned),
                kind: work["work_type"].as_str().unwrap_or_default().to_owned(),
                windows: work["os"]
                    .as_array()
                    .is_some_and(|os| os.iter().any(|o| o == "windows")),
                version: date("upgrade_date")
                    .or_else(|| date("regist_date"))
                    .unwrap_or_default()
                    .to_owned(),
                bought: None,
            })
        })
        .collect()
}

/// Main pictures from DLsite's public product information (`product/info/ajax`), for
/// works known without signing in.
pub fn public_images(answer: &Value) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = answer
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(id, info)| {
            let image = info["work_image"].as_str()?;
            let image = image
                .strip_prefix("//")
                .map_or_else(|| image.to_owned(), |rest| format!("https://{rest}"));
            Some((id.clone(), image))
        })
        .collect();
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Work, file_name, license_keys, public_images, sales, split_parts, works};

    #[test]
    fn reads_sales_and_works() {
        let answer = json!([
            {"workno": "RJ01464588", "sales_date": "2026-01-02T03:04:05+09:00"},
            {"sales_date": "2026-01-02T03:04:05+09:00"},
            {"workno": "RJ316251"}
        ]);
        assert_eq!(
            sales(&answer),
            [
                ("RJ01464588".to_owned(), Some(1_767_290_645)),
                ("RJ316251".to_owned(), None)
            ]
        );

        let answer = json!({"works": [
            {
                "workno": "RJ01464588",
                "name": {"ja_JP": "ギャルベヤ！?", "en_US": "Gal Room"},
                "maker": {"id": "RG01", "name": {"ja_JP": "サークル"}},
                "work_files": {"main": "https://img.dlsite.jp/a_img_main.jpg", "sam": "https://img.dlsite.jp/a_img_sam.jpg"},
                "work_type": "RPG",
                "os": ["android", "windows"],
                "regist_date": "2023-05-17T15:00:00.000000Z",
                "upgrade_date": "2025-11-09T15:00:00.000000Z"
            },
            {
                "workno": "VJ0001",
                "name": {"en_US": "Only English"},
                "maker": {"name": {"en_US": "Brand"}},
                "work_files": {},
                "work_type": "ADV",
                "os": ["android"],
                "regist_date": "2005-10-02T15:00:00.000000Z",
                "upgrade_date": null
            },
            {"name": {"ja_JP": "no id"}}
        ]});
        assert_eq!(
            works(&answer),
            [
                Work {
                    id: "RJ01464588".into(),
                    name: "ギャルベヤ！?".into(),
                    maker: "サークル".into(),
                    image: Some("https://img.dlsite.jp/a_img_main.jpg".into()),
                    kind: "RPG".into(),
                    windows: true,
                    version: "2025-11-09T15:00:00.000000Z".into(),
                    bought: None,
                },
                Work {
                    id: "VJ0001".into(),
                    name: "Only English".into(),
                    maker: "Brand".into(),
                    image: None,
                    kind: "ADV".into(),
                    windows: false,
                    version: "2005-10-02T15:00:00.000000Z".into(),
                    bought: None,
                },
            ]
        );
    }

    #[test]
    fn reads_public_pictures_with_their_scheme() {
        let answer = json!({
            "RJ316251": {"work_name": "トラトリトル!", "work_image": "//img.dlsite.jp/modpub/images2/work/doujin/RJ317000/RJ316251_img_main.jpg"},
            "RJ1": {"work_name": "no picture"}
        });
        assert_eq!(
            public_images(&answer),
            [(
                "RJ316251".to_owned(),
                "https://img.dlsite.jp/modpub/images2/work/doujin/RJ317000/RJ316251_img_main.jpg"
                    .to_owned()
            )]
        );
    }

    #[test]
    fn reads_the_license_keys_on_dlsites_page() {
        let html = r#"<h2>ライセンスキー</h2>
            <div class="table_inframe_box_fix"><div class="table_inframe_box_inner">
            <table>
              <tr>
                <th>ライセンスキー</th>
                <td><strong class="color_02">ABCD-1234-EFGH-5678</strong></td>
              </tr>
              <tr><th>シリアル番号 &amp; 2</th><td> XY12
                 34 </td></tr>
              <tr><th>注意</th><td>大切に保管してください</td></tr>
              <tr><th>ライセンスキー</th><td></td></tr>
            </table></div></div>"#;
        let keys: Vec<(String, String)> = license_keys(html)
            .into_iter()
            .map(|k| (k.label, k.value))
            .collect();
        assert_eq!(
            keys,
            [
                (
                    "ライセンスキー".to_owned(),
                    "ABCD-1234-EFGH-5678".to_owned()
                ),
                ("シリアル番号 & 2".to_owned(), "XY12 34".to_owned()),
            ]
        );
        assert!(license_keys("<p>no keys</p>").is_empty());
        // The key stays out of logs.
        let key = &license_keys(html)[0];
        assert!(!format!("{key:?}").contains("ABCD"));
    }

    #[test]
    fn finds_the_parts_of_a_split_download() {
        let html = r#"<table>
            <a href="https://www.dlsite.com/home/download/=/number/1/product_id/RJ263258.html">part1</a>
            <a href="https://www.dlsite.com/home/download/=/number/2/product_id/RJ263258.html">part2</a>
            <a href="https://www.dlsite.com/home/download/=/number/2/product_id/RJ263258.html">again</a>
            <a href="https://www.dlsite.com/home/work/=/product_id/RJ263258.html">the work</a>
            <a href="/home/download/=/number/3/product_id/RJ263258.html">part3</a>
        </table>"#;
        assert_eq!(
            split_parts(html),
            [
                "https://www.dlsite.com/home/download/=/number/1/product_id/RJ263258.html",
                "https://www.dlsite.com/home/download/=/number/2/product_id/RJ263258.html",
                "https://www.dlsite.com/home/download/=/number/3/product_id/RJ263258.html",
            ]
        );
        assert!(split_parts("<p>no parts</p>").is_empty());
    }

    #[test]
    fn names_the_downloaded_file() {
        assert_eq!(
            file_name("https://download.dlsite.com/get/=/type/work/domain/doujin/dir/RJ264000/file/RJ263258.part1.exe/_/20200522121604?update_date=20200522121604").as_deref(),
            Some("RJ263258.part1.exe")
        );
        assert_eq!(
            file_name("https://download.dlsite.com/get/=/file/RJ004727.zip/_/1").as_deref(),
            Some("RJ004727.zip")
        );
        assert_eq!(
            file_name("https://download.dlsite.com/get/=/file/../_/1"),
            None
        );
        assert_eq!(
            file_name("https://www.dlsite.com/home/download/=/product_id/RJ1.html"),
            None
        );
    }
}
