//! The user's DLsite purchases, through DLsite Play: sign in with the account in
//! `secrets.yaml`, list the works bought, and read each work's name, maker and main
//! picture. The same session will be needed to download and update games.
//!
//! The flow follows dlsite-async (MIT, <https://github.com/bhrevol/dlsite-async>): the
//! login form's `_token`, a form post, then DLsite Play's authorization.

use std::{collections::HashMap, time::Duration};

use serde_json::Value;

use crate::secrets::DlsiteAccount;

const TIMEOUT: Duration = Duration::from_secs(30);
const LIMIT: u64 = 32 * 1024 * 1024;

/// HTTPS with a cookie jar, which carries the session from the login to DLsite Play.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .timeout_global(Some(TIMEOUT))
        .user_agent(concat!("windows-link/", env!("CARGO_PKG_VERSION")))
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

/// Sign in and list every purchased work.
pub fn fetch_purchases(account: &DlsiteAccount) -> Result<Vec<Work>, String> {
    let agent = agent();
    let page = text(
        agent.get("https://login.dlsite.com/login?user=self").call(),
        "DLsite's login page",
    )?;
    let token = login_token(&page).ok_or("DLsite's login page has no _token")?;
    let answer = text(
        agent.post("https://login.dlsite.com/login").send_form([
            ("_token", token.as_str()),
            ("login_id", account.login_id.as_str()),
            ("password", account.password.as_str()),
        ]),
        "signing in to DLsite",
    )?;
    if !signed_in(&answer) {
        return Err(
            "DLsite did not accept the login ID and password in secrets.yaml (or asked for more, such as a CAPTCHA)"
                .into(),
        );
    }
    text(
        agent.get("https://play.dlsite.com/login/").call(),
        "DLsite Play",
    )?;
    text(
        agent
            .get("https://play.dlsite.com/api/authorize")
            .header("Referer", "https://play.dlsite.com/")
            .call(),
        "DLsite Play's authorization",
    )?;
    let bought = sales(&json(
        agent
            .get("https://play.dlsite.com/api/v3/content/sales?last=0")
            .call(),
        "the DLsite purchases",
    )?);
    let mut found = Vec::new();
    for batch in bought.chunks(100) {
        let body = serde_json::to_string(batch).unwrap_or_default();
        found.extend(works(&json(
            agent
                .post("https://play.dlsite.com/api/v3/content/works")
                .header("Content-Type", "application/json")
                .send(body),
            "the DLsite works",
        )?));
    }
    Ok(found)
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

/// A purchased work.
#[derive(Clone, Debug, PartialEq)]
pub struct Work {
    /// Such as `RJ01464588`.
    pub id: String,
    pub name: String,
    pub maker: String,
    /// The main picture's URL.
    pub image: Option<String>,
}

/// The `_token` hidden field of DLsite's login form.
pub fn login_token(html: &str) -> Option<String> {
    html.split('<')
        .filter(|tag| tag.starts_with("input") && tag.contains(r#"name="_token""#))
        .find_map(|tag| {
            let value = tag.split(r#"value=""#).nth(1)?;
            Some(value[..value.find('"')?].to_owned())
        })
}

/// Whether DLsite's answer to the login form says the user is signed in.
pub fn signed_in(html: &str) -> bool {
    html.contains("ログイン中です")
}

/// The work IDs in `/api/v3/content/sales`.
pub fn sales(answer: &Value) -> Vec<String> {
    answer
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|sale| sale["workno"].as_str().map(str::to_owned))
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
            Some(Work {
                id: work["workno"].as_str()?.to_owned(),
                name: localized(&work["name"]).unwrap_or_default(),
                maker: localized(&work["maker"]["name"]).unwrap_or_default(),
                image: work["work_files"]["main"].as_str().map(str::to_owned),
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

    use super::{Work, login_token, public_images, sales, signed_in, works};

    #[test]
    fn finds_the_login_forms_token() {
        let html = r#"<form method="POST" action="https://login.dlsite.com/login">
            <input type="hidden" name="_token" value="abc123XYZ">
            <input name="login_id"></form>"#;
        assert_eq!(login_token(html).as_deref(), Some("abc123XYZ"));
        let reordered = r#"<input value="t0k" type="hidden" name="_token" />"#;
        assert_eq!(login_token(reordered).as_deref(), Some("t0k"));
        assert_eq!(login_token("<form></form>"), None);
    }

    #[test]
    fn tells_a_signed_in_page() {
        assert!(signed_in("<p>ログイン中です</p>"));
        assert!(!signed_in("<p>ログインIDかパスワードが間違っています</p>"));
    }

    #[test]
    fn reads_sales_and_works() {
        let answer = json!([
            {"workno": "RJ01464588", "sales_date": "2026-01-02T03:04:05+09:00"},
            {"sales_date": "2026-01-02T03:04:05+09:00"},
            {"workno": "RJ316251"}
        ]);
        assert_eq!(sales(&answer), ["RJ01464588", "RJ316251"]);

        let answer = json!({"works": [
            {
                "workno": "RJ01464588",
                "name": {"ja_JP": "ギャルベヤ！?", "en_US": "Gal Room"},
                "maker": {"id": "RG01", "name": {"ja_JP": "サークル"}},
                "work_files": {"main": "https://img.dlsite.jp/a_img_main.jpg", "sam": "https://img.dlsite.jp/a_img_sam.jpg"}
            },
            {
                "workno": "VJ0001",
                "name": {"en_US": "Only English"},
                "maker": {"name": {"en_US": "Brand"}},
                "work_files": {}
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
                },
                Work {
                    id: "VJ0001".into(),
                    name: "Only English".into(),
                    maker: "Brand".into(),
                    image: None,
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
}
