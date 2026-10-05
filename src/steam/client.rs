//! Changing Steam collections through the Steam client itself. Its library UI runs in
//! Chromium (`SharedJSContext`) and keeps the collections in `collectionStore`; with
//! `.cef-enable-remote-debugging` in the Steam folder, Steam opens the Chrome
//! `DevTools` protocol on `127.0.0.1:8080`, where scripts can call that store. Steam
//! then saves and syncs the change as if it was made in its own window.

use std::{collections::HashSet, time::Duration};

use serde_json::{Value, json};

use super::Collection;

pub const DEBUG_URL: &str = "http://127.0.0.1:8080";
pub const NOT_RUNNING: &str = "Steam is not running";
pub const NO_REMOTE: &str = "Steam does not accept remote control: create .cef-enable-remote-debugging in the Steam folder and restart Steam";
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, PartialEq)]
pub enum ClientError {
    /// Steam cannot be reached; says why.
    Unavailable(String),
    /// The script failed inside Steam, with its message.
    Script(String),
}

/// Runs a script in Steam's library page and returns its JSON result.
pub trait Client: Send + Sync + 'static {
    fn evaluate(&self, script: &str) -> Result<Value, ClientError>;
}

/// The `webSocketDebuggerUrl` of the `SharedJSContext` page in `GET /json`.
pub fn shared_context(targets: &Value) -> Option<String> {
    targets
        .as_array()?
        .iter()
        .find(|t| t["title"] == "SharedJSContext")
        .and_then(|t| t["webSocketDebuggerUrl"].as_str())
        .map(str::to_owned)
}

/// The value of a `Runtime.evaluate` reply, or the script's error message.
pub fn evaluation(reply: &Value) -> Result<Value, String> {
    if let Some(error) = reply.get("error") {
        return Err(format!("DevTools error: {error}"));
    }
    let result = &reply["result"];
    if let Some(details) = result.get("exceptionDetails") {
        let message = details["exception"]["description"]
            .as_str()
            .or_else(|| details["text"].as_str())
            .unwrap_or("the script failed");
        return Err(message.to_owned());
    }
    Ok(result["result"]
        .get("value")
        .cloned()
        .unwrap_or(Value::Null))
}

/// Scripts for `collectionStore`. Values go in as JSON literals, never as code.
pub mod scripts {
    use serde_json::json;

    const STORE: &str = "const cs = globalThis.collectionStore; \
        if (!cs) throw new Error(\"STORE_NOT_READY\"); \
        const found = (id) => { const c = cs.GetCollection(id); \
        if (!c) throw new Error(\"NOT_FOUND\"); return c; };";

    /// Steam's own collections (favorites, hidden) and the user's static ones, each
    /// with its app IDs. `userCollections` also has Steam's own groupings (soundtracks,
    /// uncategorized), which cannot be deleted; only the user's can.
    pub fn collections() -> String {
        format!(
            "(() => {{ {STORE} \
             const pick = (c) => c && !c.bIsDynamic \
               ? {{ id: c.id, name: c.displayName || c.id, apps: (c.allApps || []).map((a) => a.appid) }} \
               : null; \
             const fixed = [\"favorite\", \"hidden\"].map((id) => pick(cs.GetCollection(id))); \
             const own = (cs.userCollections || []).filter((c) => c.bIsDeletable).map(pick); \
             const seen = new Set(); \
             return fixed.concat(own).filter((c) => c && !seen.has(c.id) && seen.add(c.id)); \
             }})()"
        )
    }

    /// The name Steam shows for each app (in the language Steam is set to), by app ID.
    pub fn names(apps: &[u32]) -> String {
        format!(
            "(() => {{ const s = globalThis.appStore; \
             if (!s) throw new Error(\"STORE_NOT_READY\"); \
             return Object.fromEntries({}.map((id) => [id, s.GetAppOverviewByAppID(id)?.display_name]) \
               .filter(([, name]) => name)); }})()",
            json!(apps)
        )
    }

    pub fn create(name: &str) -> String {
        format!(
            "(async () => {{ {STORE} \
             const c = cs.NewUnsavedCollection({}, undefined, []); \
             await cs.SaveCollection(c); \
             return {{ id: c.id, name: c.displayName }}; }})()",
            json!(name)
        )
    }

    /// Steam keeps the name in `m_strName` and has no setter for it.
    pub fn rename(id: &str, name: &str) -> String {
        format!(
            "(async () => {{ {STORE} \
             const c = found({}); \
             if (!c.bIsEditable) throw new Error(\"NOT_EDITABLE\"); \
             c.m_strName = {}; \
             await c.Save(); \
             return true; }})()",
            json!(id),
            json!(name)
        )
    }

    pub fn delete(id: &str) -> String {
        format!(
            "(async () => {{ {STORE} \
             const c = found({id}); \
             if (!c.bIsDeletable) throw new Error(\"NOT_EDITABLE\"); \
             await c.AsDeletableCollection().Delete(); \
             return true; }})()",
            id = json!(id)
        )
    }

    /// `AddOrRemoveApp` takes app IDs and works for Steam's own collections too, which
    /// save themselves; the user's are saved after it.
    pub fn set(id: &str, app_id: u32, on: bool) -> String {
        format!(
            "(async () => {{ {STORE} \
             const c = found({id}); \
             if (!globalThis.appStore.GetAppOverviewByAppID({app_id})) throw new Error(\"NOT_FOUND\"); \
             cs.AddOrRemoveApp([{app_id}], {on}, {id}); \
             if (c.bIsDeletable) await c.Save(); \
             return true; }})()",
            id = json!(id)
        )
    }
}

/// The collections a `scripts::collections` run returned.
pub fn parse_collections(value: &Value) -> Result<Vec<Collection>, String> {
    #[derive(serde::Deserialize)]
    struct Raw {
        id: String,
        name: String,
        apps: Vec<u32>,
    }
    let raw: Vec<Raw> = serde_json::from_value(value.clone())
        .map_err(|err| format!("unexpected collections from Steam: {err}"))?;
    Ok(raw
        .into_iter()
        .map(|c| Collection {
            id: c.id,
            name: c.name,
            apps: c.apps.into_iter().collect::<HashSet<u32>>(),
        })
        .collect())
}

/// The names a `scripts::names` run returned, by app ID.
pub fn parse_names(value: &Value) -> Result<std::collections::HashMap<u32, String>, String> {
    let names = value
        .as_object()
        .ok_or_else(|| format!("unexpected names from Steam: {value}"))?;
    Ok(names
        .iter()
        .filter_map(|(id, name)| {
            let name = name.as_str().filter(|name| !name.is_empty())?;
            Some((id.parse().ok()?, name.to_owned()))
        })
        .collect())
}

/// The Steam client on this PC, over its `DevTools` port.
pub struct CefClient {
    pub base: String,
}

impl CefClient {
    pub fn new() -> Self {
        Self {
            base: DEBUG_URL.to_owned(),
        }
    }

    fn unavailable() -> ClientError {
        use crate::launch::Launcher;
        let running = crate::launch::windows::WindowsLauncher
            .processes()
            .contains("steam.exe");
        ClientError::Unavailable(if running { NO_REMOTE } else { NOT_RUNNING }.to_owned())
    }
}

impl Default for CefClient {
    fn default() -> Self {
        Self::new()
    }
}

impl Client for CefClient {
    fn evaluate(&self, script: &str) -> Result<Value, ClientError> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .build()
            .into();
        let listing = agent
            .get(format!("{}/json", self.base))
            .call()
            .map_err(|_| Self::unavailable())?
            .body_mut()
            .read_to_string()
            .map_err(|err| {
                ClientError::Script(format!("cannot read the DevTools listing: {err}"))
            })?;
        let targets: Value = serde_json::from_str(&listing)
            .map_err(|err| ClientError::Script(format!("unexpected DevTools listing: {err}")))?;
        let url = shared_context(&targets)
            .ok_or_else(|| ClientError::Unavailable("Steam's library is not ready yet".into()))?;
        let (mut socket, _) = tungstenite::connect(url.as_str())
            .map_err(|err| ClientError::Unavailable(format!("cannot reach Steam: {err}")))?;
        if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_mut() {
            let _ = stream.set_read_timeout(Some(TIMEOUT));
        }
        let request = json!({
            "id": 1,
            "method": "Runtime.evaluate",
            "params": { "expression": script, "awaitPromise": true, "returnByValue": true },
        });
        socket
            .send(tungstenite::Message::Text(request.to_string().into()))
            .map_err(|err| ClientError::Script(format!("cannot send to Steam: {err}")))?;
        loop {
            let message = socket
                .read()
                .map_err(|err| ClientError::Script(format!("no answer from Steam: {err}")))?;
            let tungstenite::Message::Text(text) = message else {
                continue;
            };
            let reply: Value = serde_json::from_str(text.as_str())
                .map_err(|err| ClientError::Script(format!("unexpected answer: {err}")))?;
            if reply["id"] == 1 {
                let _ = socket.close(None);
                return evaluation(&reply).map_err(ClientError::Script);
            }
        }
    }
}

#[cfg(test)]
pub mod fake {
    use std::sync::Mutex;

    use serde_json::Value;

    use super::{Client, ClientError};

    /// Answers every script with the next canned result, and keeps the scripts.
    #[derive(Default)]
    pub struct FakeClient {
        pub answers: Mutex<Vec<Result<Value, ClientError>>>,
        pub scripts: Mutex<Vec<String>>,
    }

    impl Client for FakeClient {
        fn evaluate(&self, script: &str) -> Result<Value, ClientError> {
            self.scripts.lock().unwrap().push(script.to_owned());
            let mut answers = self.answers.lock().unwrap();
            if answers.is_empty() {
                Err(ClientError::Unavailable(super::NOT_RUNNING.into()))
            } else {
                answers.remove(0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{evaluation, parse_collections, parse_names, scripts, shared_context};

    #[test]
    fn finds_the_library_page_among_steam_pages() {
        let targets = json!([
            {"title": "Steam", "webSocketDebuggerUrl": "ws://127.0.0.1:8080/devtools/page/A"},
            {"title": "SharedJSContext", "webSocketDebuggerUrl": "ws://127.0.0.1:8080/devtools/page/B"}
        ]);
        assert_eq!(
            shared_context(&targets).as_deref(),
            Some("ws://127.0.0.1:8080/devtools/page/B")
        );
        assert_eq!(shared_context(&json!([{"title": "Steam"}])), None);
        assert_eq!(shared_context(&json!({})), None);
    }

    #[test]
    fn reads_a_value_or_the_script_error() {
        let ok =
            json!({"id": 1, "result": {"result": {"type": "object", "value": {"id": "uc-1"}}}});
        assert_eq!(evaluation(&ok), Ok(json!({"id": "uc-1"})));
        let undefined = json!({"id": 1, "result": {"result": {"type": "undefined"}}});
        assert_eq!(evaluation(&undefined), Ok(serde_json::Value::Null));
        let thrown = json!({"id": 1, "result": {
            "result": {"type": "object"},
            "exceptionDetails": {"text": "Uncaught", "exception": {"description": "Error: NOT_FOUND\n    at <anonymous>"}}
        }});
        assert!(
            evaluation(&thrown)
                .unwrap_err()
                .starts_with("Error: NOT_FOUND")
        );
        let protocol = json!({"id": 1, "error": {"code": -32000, "message": "bad"}});
        assert!(evaluation(&protocol).is_err());
    }

    #[test]
    fn names_go_into_scripts_as_string_literals() {
        let name = "a\"); throw 1; (\"\n";
        let script = scripts::create(name);
        assert!(script.contains(&serde_json::to_string(name).unwrap()));
        assert!(!script.contains("a\"); throw 1;"));
        let script = scripts::rename("uc-\"x", name);
        assert!(script.contains("found(\"uc-\\\"x\")"));
        assert!(
            scripts::set("hidden", 105_600, true)
                .contains("AddOrRemoveApp([105600], true, \"hidden\")")
        );
    }

    #[test]
    fn reads_the_names_steam_shows() {
        let names = parse_names(
            &json!({"1869270": "多砲塔神教", "105600": "Terraria", "x": "bad", "7": ""}),
        )
        .unwrap();
        assert_eq!(
            names,
            [
                (1_869_270, "多砲塔神教".to_owned()),
                (105_600, "Terraria".to_owned())
            ]
            .into()
        );
        assert!(parse_names(&json!([1, 2])).is_err());
        assert!(scripts::names(&[1_869_270, 105_600]).contains("[1869270,105600]"));
    }

    #[test]
    fn parses_the_collections_steam_returns() {
        let value = json!([
            {"id": "hidden", "name": "非表示", "apps": [1, 2]},
            {"id": "uc-1", "name": "RPG", "apps": []}
        ]);
        let found = parse_collections(&value).unwrap();
        assert_eq!(found[0].id, "hidden");
        assert_eq!(found[0].apps, [1, 2].into());
        assert_eq!(found[1].name, "RPG");
        assert!(parse_collections(&json!({"oops": 1})).is_err());
    }
}
