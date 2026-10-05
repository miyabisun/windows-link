//! The Steam library: owned games from the Steam Web API, install state from the Steam
//! folder's manifests, and the user's collections, for a `steam.library` button.

pub mod vdf;

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use tracing::{info, warn};

use crate::{
    library::{GameLibrary, Item, Listing, Picture, Start},
    secrets::SteamKey,
};

/// `SteamID64` of account ID 0; userdata folders are named by account ID.
const STEAM_ID64_BASE: u64 = 76_561_197_960_265_728;
/// How often the owned games are fetched again (purchases show up after this).
const REFRESH_INTERVAL: Duration = Duration::from_mins(30);
const API_TIMEOUT: Duration = Duration::from_secs(30);
const API_LIMIT: u64 = 16 * 1024 * 1024;

/// Steam's folder: `HKCU\Software\Valve\Steam\SteamPath`, or the default install folder.
pub fn steam_dir() -> PathBuf {
    use windows::{
        Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW},
        core::w,
    };
    let mut buffer = [0u16; 1024];
    let mut size = u32::try_from(std::mem::size_of_val(&buffer)).unwrap_or(0);
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Valve\\Steam"),
            w!("SteamPath"),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&raw mut size),
        )
    };
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(0);
    if read.is_ok() && len > 0 {
        PathBuf::from(String::from_utf16_lossy(&buffer[..len]))
    } else {
        PathBuf::from(r"C:\Program Files (x86)\Steam")
    }
}

/// The library as Steam on this PC knows it. Install state and collections are read
/// from Steam's files on every listing; the owned games come from the Web API every
/// `REFRESH_INTERVAL`.
pub struct SteamLibrary {
    dir: PathBuf,
    key: Option<String>,
    /// The last fetched owned games, or why there are none.
    owned: Mutex<Result<Vec<Owned>, String>>,
}

impl SteamLibrary {
    pub fn new(dir: PathBuf, key: Option<SteamKey>) -> Self {
        let reason = if key.is_some() {
            "the owned games are not fetched yet"
        } else {
            "secrets.yaml has no steam api_key"
        };
        Self {
            dir,
            key: key.map(|k| k.api_key),
            owned: Mutex::new(Err(reason.to_owned())),
        }
    }

    fn account(&self) -> Option<u64> {
        let text = fs::read_to_string(self.dir.join("config").join("loginusers.vdf")).ok()?;
        signed_in_account(&text)
    }

    /// Fetch the owned games again. A failure keeps the games fetched before.
    pub fn refresh(&self) {
        let Some(key) = &self.key else {
            return;
        };
        let fetched = self
            .account()
            .ok_or_else(|| "no Steam account has signed in on this PC".to_owned())
            .and_then(|account| fetch_owned(key, account));
        let mut owned = self.owned.lock().unwrap_or_else(PoisonError::into_inner);
        match fetched {
            Ok(games) => {
                info!(games = games.len(), "owned Steam games fetched");
                *owned = Ok(games);
            }
            Err(reason) => {
                warn!(%reason, "cannot fetch the owned Steam games");
                if owned.is_err() {
                    *owned = Err(reason);
                }
            }
        }
    }

    fn installed(&self) -> Vec<Installed> {
        let listed = fs::read_to_string(self.dir.join("steamapps").join("libraryfolders.vdf"))
            .map(|text| library_folders(&text))
            .unwrap_or_default();
        let libraries = if listed.is_empty() {
            vec![self.dir.clone()]
        } else {
            listed
        };
        let mut found: Vec<Installed> = Vec::new();
        for library in libraries {
            let Ok(entries) = fs::read_dir(library.join("steamapps")) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !(name.starts_with("appmanifest_") && name.ends_with(".acf")) {
                    continue;
                }
                let app = fs::read_to_string(entry.path())
                    .ok()
                    .and_then(|text| installed_app(&text, &library));
                if let Some(app) = app
                    && !found.iter().any(|f| f.app_id == app.app_id)
                {
                    found.push(app);
                }
            }
        }
        found
    }

    fn collections(&self) -> Vec<Collection> {
        let Some(account) = self
            .account()
            .and_then(|id| id.checked_sub(STEAM_ID64_BASE))
        else {
            return Vec::new();
        };
        let path = self
            .dir
            .join("userdata")
            .join(account.to_string())
            .join("config")
            .join("cloudstorage")
            .join("cloud-storage-namespace-1.json");
        fs::read_to_string(path)
            .map(|text| collections(&text))
            .unwrap_or_default()
    }
}

impl GameLibrary for SteamLibrary {
    fn listing(&self) -> Listing {
        let installed = self.installed();
        let collections = self.collections();
        let owned = self.owned.lock().unwrap_or_else(PoisonError::into_inner);
        listing(
            owned.as_deref().map_err(String::as_str),
            &installed,
            &collections,
        )
    }

    /// An installed game runs through Steam; an owned one opens Steam's install dialog.
    fn start(&self, id: &str) -> Option<Start> {
        let app_id: u32 = id.parse().ok()?;
        if let Some(game) = self.installed().into_iter().find(|g| g.app_id == app_id) {
            return Some(Start {
                open: format!("steam://rungameid/{app_id}"),
                folder: Some(game.folder),
            });
        }
        let owned = self.owned.lock().unwrap_or_else(PoisonError::into_inner);
        owned
            .as_ref()
            .ok()?
            .iter()
            .any(|g| g.app_id == app_id)
            .then(|| Start {
                open: format!("steam://install/{app_id}"),
                folder: None,
            })
    }

    fn picture(&self, id: &str) -> Option<Picture> {
        let app_id: u32 = id.parse().ok()?;
        Some(picture(
            &self.dir.join("appcache").join("librarycache"),
            app_id,
        ))
    }
}

/// A game's header picture (460×215): the newest `library_header*.jpg` Steam keeps in
/// `librarycache` (in the client's language when the game has one), or the store's.
/// The store moved newer games' pictures to addresses only its API knows, but Steam
/// caches those once its library has shown them.
pub fn picture(librarycache: &Path, app_id: u32) -> Picture {
    let newest = fs::read_dir(librarycache.join(app_id.to_string()))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .flat_map(|dir| fs::read_dir(dir.path()).into_iter().flatten().flatten())
        .filter(|file| {
            let name = file.file_name();
            let name = name.to_string_lossy();
            name.starts_with("library_header") && name.ends_with(".jpg")
        })
        .filter_map(|file| Some((file.metadata().ok()?.modified().ok()?, file.path())))
        .max_by_key(|(modified, _)| *modified);
    match newest {
        Some((_, path)) => Picture::File(path),
        None => Picture::Url(format!(
            "https://cdn.cloudflare.steamstatic.com/steam/apps/{app_id}/header.jpg"
        )),
    }
}

/// Fetch the owned games now and every `REFRESH_INTERVAL`.
pub async fn run_refresher(library: Arc<SteamLibrary>) {
    loop {
        let refreshing = library.clone();
        let _ = tokio::task::spawn_blocking(move || refreshing.refresh()).await;
        tokio::time::sleep(REFRESH_INTERVAL).await;
    }
}

fn fetch_owned(key: &str, steam_id: u64) -> Result<Vec<Owned>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .timeout_global(Some(API_TIMEOUT))
        .user_agent(concat!("windows-link/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    // The key is in the URL, so keep it out of error messages.
    let hide = |text: String| text.replace(key, "***");
    let mut response = agent
        .get("https://api.steampowered.com/IPlayerService/GetOwnedGames/v1/")
        .query("key", key)
        .query("steamid", steam_id.to_string())
        .query("include_appinfo", "1")
        .query("include_played_free_games", "1")
        .query("format", "json")
        .call()
        .map_err(|err| match err {
            ureq::Error::StatusCode(401 | 403) => "the Steam Web API rejected the key".to_owned(),
            err => hide(format!("the Steam Web API cannot be reached: {err}")),
        })?;
    let text = response
        .body_mut()
        .with_config()
        .limit(API_LIMIT)
        .read_to_string()
        .map_err(|err| hide(format!("cannot read the Steam Web API answer: {err}")))?;
    owned_games(&text)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Owned {
    pub app_id: u32,
    pub name: String,
    /// Unix time of the last play, 0 when never played.
    pub last_played: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Installed {
    pub app_id: u32,
    pub name: String,
    /// The game's own folder (`steamapps\common\<installdir>`).
    pub folder: PathBuf,
    pub last_played: u64,
}

/// A collection the user made by hand (dynamic collections are left out).
#[derive(Clone, Debug, PartialEq)]
pub struct Collection {
    pub name: String,
    pub apps: HashSet<u32>,
}

/// `StateFlags` bit of a game whose files are all in place.
const FULLY_INSTALLED: u32 = 4;
/// `StateFlags` bit of a game being downloaded.
const DOWNLOADING: u32 = 1024;

/// The `SteamID64` of the account that signed in last (`config\loginusers.vdf`).
pub fn signed_in_account(loginusers: &str) -> Option<u64> {
    let file = vdf::parse(loginusers).ok()?;
    let users: Vec<(u64, &vdf::Map)> = file
        .map("users")?
        .maps()
        .filter_map(|(id, user)| Some((id.parse().ok()?, user)))
        .collect();
    let number = |user: &vdf::Map, key| {
        user.text(key)
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    };
    users
        .iter()
        .find(|(_, user)| number(user, "MostRecent") == 1)
        .or_else(|| {
            users
                .iter()
                .max_by_key(|(_, user)| number(user, "Timestamp"))
        })
        .map(|(id, _)| *id)
}

/// Library folders from `steamapps\libraryfolders.vdf`.
pub fn library_folders(text: &str) -> Vec<PathBuf> {
    let Ok(file) = vdf::parse(text) else {
        return Vec::new();
    };
    file.map("libraryfolders")
        .into_iter()
        .flat_map(vdf::Map::maps)
        .filter_map(|(_, folder)| folder.text("path").map(PathBuf::from))
        .collect()
}

/// A fully installed game from its `appmanifest_<id>.acf` in `library`.
pub fn installed_app(acf: &str, library: &Path) -> Option<Installed> {
    let file = vdf::parse(acf).ok()?;
    let app = file.map("AppState")?;
    let flags: u32 = app.text("StateFlags")?.parse().ok()?;
    if flags & FULLY_INSTALLED == 0 || flags & DOWNLOADING != 0 {
        return None;
    }
    Some(Installed {
        app_id: app.text("appid")?.parse().ok()?,
        name: app.text("name")?.trim().to_owned(),
        folder: library
            .join("steamapps")
            .join("common")
            .join(app.text("installdir")?),
        last_played: app
            .text("LastPlayed")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    })
}

/// Collections from `userdata\<account>\config\cloudstorage\cloud-storage-namespace-1.json`.
pub fn collections(cloud_storage: &str) -> Vec<Collection> {
    #[derive(serde::Deserialize)]
    struct Entry {
        #[serde(default)]
        is_deleted: bool,
        value: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Value {
        name: String,
        #[serde(default)]
        added: Vec<u32>,
        #[serde(default)]
        removed: Vec<u32>,
        #[serde(rename = "filterSpec")]
        filter_spec: Option<serde_json::Value>,
    }
    let Ok(entries) = serde_json::from_str::<Vec<(String, Entry)>>(cloud_storage) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .filter(|(key, entry)| key.starts_with("user-collections.") && !entry.is_deleted)
        .filter_map(|(_, entry)| serde_json::from_str::<Value>(&entry.value?).ok())
        .filter(|value| value.filter_spec.is_none())
        .map(|value| {
            let removed: HashSet<u32> = value.removed.into_iter().collect();
            Collection {
                name: value.name,
                apps: value
                    .added
                    .into_iter()
                    .filter(|id| !removed.contains(id))
                    .collect(),
            }
        })
        .collect()
}

/// Games in an `IPlayerService/GetOwnedGames` response.
pub fn owned_games(response: &str) -> Result<Vec<Owned>, String> {
    #[derive(serde::Deserialize)]
    struct Body {
        response: Response,
    }
    #[derive(serde::Deserialize)]
    struct Response {
        games: Option<Vec<Game>>,
    }
    #[derive(serde::Deserialize)]
    struct Game {
        appid: u32,
        #[serde(default)]
        name: String,
        #[serde(default)]
        rtime_last_played: u64,
    }
    let body: Body = serde_json::from_str(response)
        .map_err(|err| format!("unexpected answer from the Steam Web API: {err}"))?;
    let games = body.response.games.ok_or_else(|| {
        "the Steam Web API listed no games (is the profile's game details private?)".to_owned()
    })?;
    Ok(games
        .into_iter()
        .map(|game| Owned {
            app_id: game.appid,
            name: game.name.trim().to_owned(),
            last_played: game.rtime_last_played,
        })
        .collect())
}

/// The listing a panel shows: every owned game when the Web API answered, otherwise
/// the installed ones with the reason; recently played first.
pub fn listing(
    owned: Result<&[Owned], &str>,
    installed: &[Installed],
    collections: &[Collection],
) -> Listing {
    let local = |id: u32| installed.iter().find(|game| game.app_id == id);
    let (games, partial): (Vec<(u32, &str, u64)>, _) = match owned {
        Ok(owned) => (
            owned
                .iter()
                .map(|game| {
                    let played = local(game.app_id).map_or(0, |l| l.last_played);
                    (
                        game.app_id,
                        game.name.as_str(),
                        game.last_played.max(played),
                    )
                })
                .collect(),
            None,
        ),
        Err(reason) => (
            installed
                .iter()
                .map(|game| (game.app_id, game.name.as_str(), game.last_played))
                .collect(),
            Some(reason.to_owned()),
        ),
    };
    let mut games = games;
    games.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
    });
    let items: Vec<Item> = games
        .into_iter()
        .map(|(id, name, _)| Item {
            id: id.to_string(),
            name: name.to_owned(),
            installed: local(id).is_some(),
            labels: collections
                .iter()
                .filter(|c| c.apps.contains(&id))
                .map(|c| c.name.clone())
                .collect(),
        })
        .collect();
    let labels = collections
        .iter()
        .filter(|c| items.iter().any(|item| item.labels.contains(&c.name)))
        .map(|c| c.name.clone())
        .collect();
    Listing {
        items,
        labels,
        partial,
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        Collection, Installed, Owned, collections, installed_app, library_folders, listing,
        owned_games, picture, signed_in_account,
    };
    use crate::library::Picture;

    #[test]
    fn the_signed_in_account_is_the_most_recent_or_the_newest() {
        let one = "\"users\"\n{\n\t\"76561198046531357\"\n\t{\n\t\t\"AccountName\"\t\t\"a\"\n\t\t\"Timestamp\"\t\t\"1791116151\"\n\t}\n}\n";
        assert_eq!(signed_in_account(one), Some(76_561_198_046_531_357));
        let two = "users { 76561198000000001 { MostRecent 0 Timestamp 9 } 76561198000000002 { MostRecent 1 Timestamp 5 } }";
        assert_eq!(signed_in_account(two), Some(76_561_198_000_000_002));
        let neither =
            "users { 76561198000000001 { Timestamp 5 } 76561198000000002 { Timestamp 9 } }";
        assert_eq!(signed_in_account(neither), Some(76_561_198_000_000_002));
        assert_eq!(signed_in_account("users { }"), None);
        assert_eq!(signed_in_account("not vdf {"), None);
    }

    #[test]
    fn library_folders_list_each_path() {
        let text = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\"\n\t\t\"apps\" { \"1364780\" \"1\" }\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n\t}\n}\n";
        assert_eq!(
            library_folders(text),
            [
                PathBuf::from(r"C:\Program Files (x86)\Steam"),
                PathBuf::from(r"D:\SteamLibrary")
            ]
        );
        assert!(library_folders("broken {").is_empty());
    }

    #[test]
    fn a_manifest_gives_the_game_folder_only_when_fully_installed() {
        let acf = "\"AppState\"\n{\n\t\"appid\"\t\t\"1364780\"\n\t\"name\"\t\t\"Street Fighter™ 6\"\n\t\"StateFlags\"\t\t\"4\"\n\t\"installdir\"\t\t\"Street Fighter 6\"\n\t\"LastPlayed\"\t\t\"1791160203\"\n}\n";
        let library = Path::new(r"C:\Steam");
        assert_eq!(
            installed_app(acf, library),
            Some(Installed {
                app_id: 1_364_780,
                name: "Street Fighter™ 6".into(),
                folder: PathBuf::from(r"C:\Steam\steamapps\common\Street Fighter 6"),
                last_played: 1_791_160_203,
            })
        );
        // Needing an update (4 | 2) still counts as installed; downloading (1026) does not.
        assert!(installed_app(&acf.replace("\"4\"", "\"6\""), library).is_some());
        assert_eq!(
            installed_app(&acf.replace("\"4\"", "\"1026\""), library),
            None
        );
        assert_eq!(installed_app("broken {", library), None);
    }

    #[test]
    fn collections_are_the_hand_made_ones_without_removed_games() {
        let value = |v: &str| serde_json::to_string(v).unwrap();
        let text = format!(
            "[[\"user-collections.favorite\",{{\"key\":\"user-collections.favorite\",\"value\":{}}}],\
             [\"user-collections.uc-1\",{{\"key\":\"user-collections.uc-1\",\"value\":{}}}],\
             [\"user-collections.uc-2\",{{\"key\":\"user-collections.uc-2\",\"value\":{}}}],\
             [\"user-collections.uc-3\",{{\"key\":\"user-collections.uc-3\",\"is_deleted\":true}}],\
             [\"showcases.1\",{{\"key\":\"showcases.1\",\"value\":\"{{}}\"}}]]",
            value(r#"{"id":"favorite","name":"お気に入り","added":[],"removed":[]}"#),
            value(r#"{"id":"uc-1","name":"outdate","added":[10,20,30],"removed":[20]}"#),
            value(r#"{"id":"uc-2","name":"Recent","added":[],"removed":[],"filterSpec":{}}"#),
        );
        let found = collections(&text);
        assert_eq!(
            found,
            [
                Collection {
                    name: "お気に入り".into(),
                    apps: [].into()
                },
                Collection {
                    name: "outdate".into(),
                    apps: [10, 30].into()
                },
            ]
        );
        assert!(collections("not json").is_empty());
    }

    #[test]
    fn owned_games_come_from_the_api_response() {
        let text = r#"{"response":{"game_count":2,"games":[
            {"appid":1364780,"name":"Street Fighter™ 6","playtime_forever":10,"rtime_last_played":1791160203},
            {"appid":105600,"name":"Terraria","playtime_forever":0}]}}"#;
        assert_eq!(
            owned_games(text).unwrap(),
            [
                Owned {
                    app_id: 1_364_780,
                    name: "Street Fighter™ 6".into(),
                    last_played: 1_791_160_203
                },
                Owned {
                    app_id: 105_600,
                    name: "Terraria".into(),
                    last_played: 0
                },
            ]
        );
        // A private profile answers with no games at all.
        assert!(
            owned_games(r#"{"response":{}}"#)
                .unwrap_err()
                .contains("no games")
        );
        assert!(owned_games("<html>Forbidden</html>").is_err());
    }

    fn installed(app_id: u32, name: &str, last_played: u64) -> Installed {
        Installed {
            app_id,
            name: name.into(),
            folder: PathBuf::from(format!(r"C:\Steam\steamapps\common\{name}")),
            last_played,
        }
    }

    #[test]
    fn the_listing_has_every_owned_game_with_install_state_and_labels() {
        let owned = [
            Owned {
                app_id: 1,
                name: "beta".into(),
                last_played: 0,
            },
            Owned {
                app_id: 2,
                name: "Alpha".into(),
                last_played: 0,
            },
            Owned {
                app_id: 3,
                name: "Gamma".into(),
                last_played: 50,
            },
        ];
        let local = [installed(2, "Alpha", 100), installed(9, "Not owned", 999)];
        let sets = [
            Collection {
                name: "お気に入り".into(),
                apps: [].into(),
            },
            Collection {
                name: "outdate".into(),
                apps: [1, 3].into(),
            },
        ];
        let listing = listing(Ok(&owned), &local, &sets);
        let rows: Vec<_> = listing
            .items
            .iter()
            .map(|i| {
                (
                    i.id.as_str(),
                    i.name.as_str(),
                    i.installed,
                    i.labels.clone(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("2", "Alpha", true, vec![]),
                ("3", "Gamma", false, vec!["outdate".to_owned()]),
                ("1", "beta", false, vec!["outdate".to_owned()]),
            ]
        );
        // Labels without any listed game are left out.
        assert_eq!(listing.labels, ["outdate"]);
        assert_eq!(listing.partial, None);
    }

    #[test]
    fn the_picture_is_the_newest_cached_header_or_the_store_one() {
        let dir =
            std::env::temp_dir().join(format!("windows-link-pictures-{}", std::process::id()));
        let app = dir.join("1364780");
        std::fs::create_dir_all(app.join("aaa")).unwrap();
        std::fs::create_dir_all(app.join("bbb")).unwrap();
        let old = app.join(r"aaa\library_header.jpg");
        let new = app.join(r"bbb\library_header_japanese.jpg");
        std::fs::write(&old, "old").unwrap();
        std::fs::write(&new, "new").unwrap();
        std::fs::write(app.join(r"bbb\library_hero.jpg"), "hero").unwrap();
        let at = |path: &Path, secs| {
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
                .unwrap();
        };
        at(&old, 100);
        at(&new, 200);
        assert_eq!(picture(&dir, 1_364_780), Picture::File(new.clone()));
        at(&old, 300);
        assert_eq!(picture(&dir, 1_364_780), Picture::File(old));
        assert_eq!(
            picture(&dir, 105_600),
            Picture::Url(
                "https://cdn.cloudflare.steamstatic.com/steam/apps/105600/header.jpg".into()
            )
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_steam_folder_lists_its_installed_games_and_starts_them() {
        use crate::library::{GameLibrary, Start};

        let dir = std::env::temp_dir().join(format!("windows-link-steam-{}", std::process::id()));
        let apps = dir.join("steamapps");
        let cloud = dir.join(r"userdata\86265629\config\cloudstorage");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::create_dir_all(&cloud).unwrap();
        std::fs::write(
            dir.join(r"config\loginusers.vdf"),
            "users { 76561198046531357 { Timestamp 1 } }",
        )
        .unwrap();
        std::fs::write(
            apps.join("libraryfolders.vdf"),
            format!(
                "libraryfolders {{ 0 {{ path \"{}\" }} }}",
                dir.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        std::fs::write(
            apps.join("appmanifest_105600.acf"),
            "AppState { appid 105600 name Terraria StateFlags 4 installdir Terraria LastPlayed 5 }",
        )
        .unwrap();
        std::fs::write(
            cloud.join("cloud-storage-namespace-1.json"),
            r#"[["user-collections.uc-1",{"value":"{\"name\":\"outdate\",\"added\":[105600],\"removed\":[]}"}]]"#,
        )
        .unwrap();

        let library = super::SteamLibrary::new(dir.clone(), None);
        let listing = library.listing();
        assert_eq!(listing.items.len(), 1);
        assert_eq!(listing.items[0].name, "Terraria");
        assert_eq!(listing.items[0].labels, ["outdate"]);
        assert_eq!(
            listing.partial.as_deref(),
            Some("secrets.yaml has no steam api_key")
        );
        assert_eq!(
            library.start("105600"),
            Some(Start {
                open: "steam://rungameid/105600".into(),
                folder: Some(apps.join(r"common\Terraria")),
            })
        );
        assert_eq!(library.start("1364780"), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn without_the_api_the_listing_is_the_installed_games_with_the_reason() {
        let local = [installed(2, "Alpha", 100), installed(9, "Delta", 999)];
        let listing = listing(Err("no API key"), &local, &[]);
        let ids: Vec<_> = listing.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["9", "2"]);
        assert!(listing.items.iter().all(|i| i.installed));
        assert_eq!(listing.partial.as_deref(), Some("no API key"));
    }
}
