//! FANZA, DMM's adult PC game shop, as a shop of games for `crate::shop`: the works
//! bought are read from FANZA's library and downloaded into `<root>\<brand>\<title>`
//! (`D:\fanza` unless configured). Signing in is the user's, in the panel's login
//! window; its cookies come through `PUT /fanza/session`.

pub mod api;
pub mod client;
pub mod session;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use crate::{
    library::{KeyError, LicenseKey},
    shop::{Known, Shop, ShopLibrary, Title, Work, download::folder_name},
};

pub use client::{Client, FanzaError};

/// Where FANZA's games go.
pub const DEFAULT_ROOT: &str = r"D:\fanza";

/// Which folder is which work bought: the one named after its brand and title, as
/// downloads name them (with the work's ID after the title when another game had the
/// name). Downloads record the folder anyway; this finds them again without the
/// record.
pub fn identify(titles: &[Title], purchases: &[Work]) -> HashMap<String, Known> {
    titles
        .iter()
        .filter_map(|title| {
            let work = purchases.iter().find(|work| {
                let name = folder_name(&work.name);
                title.maker == folder_name(&work.maker)
                    && (title.name == name || title.name == format!("{name} ({})", work.id))
            })?;
            let known = Known {
                work: work.id.clone(),
                image: work.image.clone(),
            };
            Some((title.id.clone(), known))
        })
        .collect()
}

/// The library pages read at most (FANZA gives 120 works a page).
const PAGES: usize = 100;

/// A work's ID as FANZA writes them (`alice_0024`), safe in a URL as it is.
fn plain_id(id: &str) -> Result<&str, String> {
    let plain = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if plain {
        Ok(id)
    } else {
        Err(format!("FANZA gave an unexpected work ID: {id:?}"))
    }
}

/// FANZA, through the cookies the panel's login window got.
pub struct FanzaShop {
    client: Arc<Client>,
    /// For the files themselves, on DMM's servers: no cookies.
    files: ureq::Agent,
}

impl FanzaShop {
    pub fn new(client: Arc<Client>) -> Self {
        Self {
            client,
            files: ureq::Agent::config_builder()
                .https_only(true)
                .user_agent(concat!("windows-link/", env!("CARGO_PKG_VERSION")))
                .build()
                .into(),
        }
    }

    /// Every work bought, page by page.
    pub fn purchases(&self) -> Result<Vec<Work>, FanzaError> {
        let mut works = Vec::new();
        for page in 1..=PAGES {
            let json = self.client.api(&format!(
                "/ajax/v1/library?service=all&sort=order_desc&page={page}&browserOnly=0"
            ))?;
            let (total, more) = api::library_page(&json).map_err(FanzaError::Failed)?;
            if more.is_empty() {
                break;
            }
            works.extend(more);
            if works.len() >= total {
                break;
            }
        }
        Ok(works)
    }
}

impl Shop for FanzaShop {
    fn name(&self) -> &'static str {
        "FANZA"
    }

    /// A work sold on its own; a set's games come through its own page, not read yet.
    fn wants(&self, work: &Work) -> bool {
        work.kind == "single"
    }

    fn ready(&self) -> bool {
        self.client.signed_in()
    }

    /// Every FANZA game here is downloaded by windows-link, which records its version.
    fn released(&self, _version: &str) -> Option<SystemTime> {
        None
    }

    fn fetch(
        &self,
        work: &Work,
        staging: &Path,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Vec<PathBuf>, String> {
        let id = plain_id(&work.id)?;
        let detail = self
            .client
            .api(&format!("/ajax/v1/library/detail/single/?productId={id}"))
            .map_err(|err| err.to_string())?;
        let mut files = Vec::new();
        for file in api::files(&detail)? {
            // DMM's link to each file lasts a while only: ask for it right before.
            let url = self
                .client
                .download_url(&file.path)
                .map_err(|err| err.to_string())?;
            let path = staging.join(&file.name);
            crate::shop::download::fetch(&self.files, &url, &file.name, &path, progress)?;
            files.push(path);
        }
        Ok(files)
    }

    /// FANZA sends a game's serial code by e-mail; its pages do not show it.
    fn license_keys(&self, _work: &str) -> Result<Vec<LicenseKey>, KeyError> {
        Err(KeyError::Unavailable(
            "FANZA sends serial codes by e-mail".into(),
        ))
    }

    fn sign_in(&self) -> Option<&'static str> {
        (!self.client.signed_in()).then_some("fanza")
    }
}

/// The FANZA games under one folder (`DEFAULT_ROOT` unless configured).
pub type FanzaLibrary = ShopLibrary<FanzaShop>;

impl ShopLibrary<FanzaShop> {
    /// Read the works bought and find out which folder is which. Failures are logged
    /// and leave what was known; a needed sign-in shows on the panel.
    pub fn refresh(&self) {
        match self.shop().purchases() {
            Ok(works) => {
                tracing::info!(works = works.len(), "FANZA purchases read");
                self.set_purchases(&works);
                let Ok(games) = crate::shop::scan(&self.root) else {
                    return;
                };
                let titles: Vec<Title> = games.into_iter().map(|(title, _)| title).collect();
                let known = identify(&titles, &works);
                if let Err(err) = self.store.remember(&titles, &known) {
                    tracing::warn!(%err, "cannot keep which FANZA work each game is");
                }
            }
            Err(FanzaError::SignIn) => {
                tracing::warn!("FANZA needs signing in again through the panel");
            }
            Err(FanzaError::Failed(reason)) => {
                tracing::warn!(%reason, "cannot read the FANZA purchases");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        time::{Duration, UNIX_EPOCH},
    };

    use super::identify;
    use crate::shop::{Known, Title, Work, title_id};

    fn title(maker: &str, name: &str) -> Title {
        Title {
            id: title_id(maker, name),
            maker: maker.into(),
            name: name.into(),
            folder: PathBuf::from(format!(r"D:\fanza\{maker}\{name}")),
            modified: UNIX_EPOCH + Duration::from_secs(1),
        }
    }

    fn bought(id: &str, maker: &str, name: &str) -> Work {
        Work {
            id: id.into(),
            name: name.into(),
            maker: maker.into(),
            image: Some(format!("https://pics.dmm.co.jp/{id}pl.jpg")),
            kind: "single".into(),
            windows: true,
            version: "2014-04-25 10:00".into(),
        }
    }

    #[test]
    fn work_ids_go_into_urls_only_when_plain() {
        use super::plain_id;

        assert_eq!(plain_id("alice_0024"), Ok("alice_0024"));
        assert_eq!(plain_id("d_123-a"), Ok("d_123-a"));
        for bad in ["", "a&b=c", "../x", "a b"] {
            assert!(plain_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn updating_needs_signing_in_first() {
        use std::sync::Arc;

        use crate::library::{GameLibrary, UpdateError};
        use crate::shop::ShopStore;

        let session = std::env::temp_dir().join(format!(
            "windows-link-fanza-none-{}.json",
            std::process::id()
        ));
        let library = super::FanzaLibrary::new(
            std::env::temp_dir(),
            Arc::new(ShopStore::in_memory("fanza").unwrap()),
            super::FanzaShop::new(Arc::new(super::Client::open(session))),
        );
        assert_eq!(library.update(&[]), Err(UpdateError::SignIn));
    }

    #[test]
    fn folders_are_found_by_brand_and_title_as_downloads_name_them() {
        let titles = [
            title("アリスソフト", "ランス9 ヘルマン革命"),
            // A title Windows does not allow as a folder name, and one that had to take
            // its ID.
            title("セレン", "DEEP_ZERO"),
            title("アリスソフト", "大悪司 (alice_0004)"),
            title("Other", "Not bought"),
        ];
        let purchases = [
            bought("alice_0024", "アリスソフト", "ランス9 ヘルマン革命"),
            bought("selen_0004", "セレン", "DEEP/ZERO"),
            bought("alice_0004", "アリスソフト", "大悪司"),
            bought("alice_0099", "アリスソフト", "Not here"),
        ];
        let known = identify(&titles, &purchases);
        let work = |t: &Title| known.get(&t.id).map(|k| k.work.as_str());
        assert_eq!(work(&titles[0]), Some("alice_0024"));
        assert_eq!(work(&titles[1]), Some("selen_0004"));
        assert_eq!(work(&titles[2]), Some("alice_0004"));
        assert_eq!(work(&titles[3]), None);
        assert_eq!(
            known[&titles[0].id],
            Known {
                work: "alice_0024".into(),
                image: Some("https://pics.dmm.co.jp/alice_0024pl.jpg".into())
            }
        );
        assert_eq!(known.len(), 3);
    }
}
