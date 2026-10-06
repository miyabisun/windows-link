//! Discord servers through the desktop app's local RPC: their icons, read while Discord
//! runs, for the `discord.server` buttons.

pub mod client;
pub mod proto;
pub mod service;
pub mod token;

use std::{collections::HashMap, fmt::Write as _};

use serde_json::Value;

/// Server ID → its icon picture on Discord's CDN.
pub type ServerIcons = HashMap<String, String>;

pub trait Discord: Send + Sync + 'static {
    /// The icons read from Discord so far (kept after Discord closes).
    fn icons(&self) -> ServerIcons;
}

/// Used when no button needs Discord, or Discord cannot be set up.
pub struct NoDiscord;

impl Discord for NoDiscord {
    fn icons(&self) -> ServerIcons {
        ServerIcons::new()
    }
}

fn guilds(data: &Value) -> impl Iterator<Item = &Value> {
    data["guilds"].as_array().into_iter().flatten()
}

/// Each server's icon from `GET_GUILDS`; servers without one are left out.
pub fn guild_icons(data: &Value) -> ServerIcons {
    guilds(data)
        .filter_map(|guild| {
            Some((
                guild["id"].as_str()?.to_owned(),
                guild["icon_url"].as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// `windows-link discord-servers` output: each server's ID and name.
pub fn server_listing(data: &Value) -> String {
    let mut out = String::new();
    for guild in guilds(data) {
        let _ = writeln!(
            out,
            "{}  {}",
            guild["id"].as_str().unwrap_or("?"),
            guild["name"].as_str().unwrap_or("?")
        );
    }
    out
}

#[cfg(test)]
pub mod fake {
    use super::{Discord, ServerIcons};

    pub struct FakeDiscord(pub ServerIcons);

    impl Discord for FakeDiscord {
        fn icons(&self) -> ServerIcons {
            self.0.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::{guild_icons, server_listing};

    fn guilds() -> serde_json::Value {
        json!({ "guilds": [
            { "id": "1533", "name": "UF4フレンド", "icon_url": "https://cdn.discordapp.com/icons/1533/d08.webp?size=128" },
            { "id": "1486", "name": "アイコンなし", "icon_url": null },
        ] })
    }

    #[test]
    fn lists_each_server_with_its_id() {
        assert_eq!(
            server_listing(&guilds()),
            "1533  UF4フレンド\n1486  アイコンなし\n"
        );
        assert_eq!(server_listing(&json!({})), "");
    }

    #[test]
    fn reads_each_servers_icon_and_skips_the_servers_without_one() {
        assert_eq!(
            guild_icons(&guilds()),
            HashMap::from([(
                "1533".to_owned(),
                "https://cdn.discordapp.com/icons/1533/d08.webp?size=128".to_owned()
            )])
        );
        assert!(guild_icons(&json!({})).is_empty());
    }
}
