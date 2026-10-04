//! Windows virtual desktops: list, switch, change notifications, and pinning a
//! window to every desktop. The undocumented API is used only in `winvd.rs`.

use serde::Serialize;

pub mod api;
pub mod winvd;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DesktopInfo {
    /// GUID, stable across renames and reordering.
    pub id: String,
    pub name: String,
    pub index: u32,
    pub current: bool,
}

#[derive(Debug, PartialEq)]
pub enum DesktopError {
    NotFound,
    Failed(String),
}

pub trait VirtualDesktops: Send + Sync + 'static {
    fn list(&self) -> Result<Vec<DesktopInfo>, String>;
    fn switch(&self, id: &str) -> Result<(), DesktopError>;
    fn pin_window(&self, hwnd: isize) -> Result<(), DesktopError>;
    /// Create a desktop with this name and switch to it.
    fn create(&self, name: &str) -> Result<(), DesktopError>;
}

/// Desktop files whose desktop does not exist (renamed or removed in Windows), so a
/// panel can offer to create them again. Names match without regard to case.
pub fn unmatched(defined: &[String], desktops: &[DesktopInfo]) -> Vec<String> {
    defined
        .iter()
        .filter(|name| {
            !desktops
                .iter()
                .any(|d| d.name.to_lowercase() == name.to_lowercase())
        })
        .cloned()
        .collect()
}

/// Windows shows unnamed desktops as "デスクトップ N"; do the same.
pub fn display_name(name: &str, index: u32) -> String {
    if name.trim().is_empty() {
        format!("デスクトップ {}", index + 1)
    } else {
        name.to_owned()
    }
}

/// Window handle from a path segment: decimal, or hexadecimal with `0x`.
pub fn parse_hwnd(text: &str) -> Option<isize> {
    let value = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => isize::from_str_radix(hex, 16).ok()?,
        None => text.parse().ok()?,
    };
    (value != 0).then_some(value)
}

/// Event name for `/events` from what changed, or `None` for changes that do not
/// affect the desktop list (wallpapers, windows moving between desktops).
pub fn reason(event: &::winvd::DesktopEvent) -> Option<&'static str> {
    use ::winvd::DesktopEvent::{
        DesktopChanged, DesktopCreated, DesktopDestroyed, DesktopMoved, DesktopNameChanged,
        DesktopWallpaperChanged, WindowChanged,
    };
    match event {
        DesktopChanged { .. } => Some("changed"),
        DesktopCreated(_) => Some("created"),
        DesktopDestroyed { .. } => Some("removed"),
        DesktopNameChanged(..) => Some("renamed"),
        DesktopMoved { .. } => Some("moved"),
        DesktopWallpaperChanged(..) | WindowChanged(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use ::winvd::{Desktop, DesktopEvent};
    use windows058::Win32::Foundation::HWND;

    use super::{DesktopInfo, display_name, parse_hwnd, reason, unmatched};

    #[test]
    fn lists_desktop_files_without_a_desktop() {
        let desktop = |name: &str| DesktopInfo {
            id: name.to_owned(),
            name: name.to_owned(),
            index: 0,
            current: false,
        };
        let defined = ["dev", "SF6", "Old"].map(str::to_owned);
        assert_eq!(
            unmatched(&defined, &[desktop("DEV"), desktop("sf6")]),
            ["Old"]
        );
        assert!(unmatched(&[], &[desktop("dev")]).is_empty());
    }

    #[test]
    fn desktop_list_events_are_reported_and_window_or_wallpaper_events_are_not() {
        let d = |i: u32| Desktop::from(i);
        let cases = [
            (
                DesktopEvent::DesktopChanged {
                    new: d(1),
                    old: d(0),
                },
                Some("changed"),
            ),
            (DesktopEvent::DesktopCreated(d(4)), Some("created")),
            (
                DesktopEvent::DesktopDestroyed {
                    destroyed: d(4),
                    fallback: d(0),
                },
                Some("removed"),
            ),
            (
                DesktopEvent::DesktopNameChanged(d(0), "dev".into()),
                Some("renamed"),
            ),
            (
                DesktopEvent::DesktopMoved {
                    desktop: d(0),
                    old_index: 0,
                    new_index: 2,
                },
                Some("moved"),
            ),
            (
                DesktopEvent::DesktopWallpaperChanged(d(0), "a.jpg".into()),
                None,
            ),
            (DesktopEvent::WindowChanged(HWND::default()), None),
        ];
        for (event, expected) in &cases {
            assert_eq!(reason(event), *expected);
        }
    }

    #[test]
    fn unnamed_desktops_get_the_windows_style_name() {
        assert_eq!(display_name("", 0), "デスクトップ 1");
        assert_eq!(display_name("  ", 2), "デスクトップ 3");
        assert_eq!(display_name("ゲーム", 1), "ゲーム");
    }

    #[test]
    fn parses_decimal_and_hex_handles_and_rejects_zero_or_junk() {
        assert_eq!(parse_hwnd("4723016"), Some(4_723_016));
        assert_eq!(parse_hwnd("0x481148"), Some(0x0048_1148));
        assert_eq!(parse_hwnd("0XAbC"), Some(0xabc));
        assert_eq!(parse_hwnd("0"), None);
        assert_eq!(parse_hwnd("0x"), None);
        assert_eq!(parse_hwnd("window"), None);
    }
}
