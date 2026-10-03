//! Keep the mouse cursor where it was after touch input.
//!
//! Windows promotes touch to mouse input and moves the single system cursor to the
//! touched point. For monitors where "keep cursor" is on (the default), the cursor is
//! moved back to the last real mouse position shortly after the finger is lifted; the
//! tap itself still reaches the application.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use serde::Serialize;

pub mod api;
pub mod store;
pub mod windows;

/// `dwExtraInfo` signature Windows puts on mouse input synthesized from pen or touch.
const MI_WP_SIGNATURE: usize = 0xFF51_5700;
const SIGNATURE_MASK: usize = 0xFFFF_FF00;
const TOUCH_BIT: usize = 0x80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Mouse,
    Pen,
    Touch,
}

/// Classify a low-level mouse event by its extra info. Touch-promoted input is also
/// flagged as injected, so that flag cannot tell it apart from a real mouse.
pub fn classify(extra_info: usize) -> Source {
    if extra_info & SIGNATURE_MASK != MI_WP_SIGNATURE {
        Source::Mouse
    } else if extra_info & TOUCH_BIT != 0 {
        Source::Touch
    } else {
        Source::Pen
    }
}

/// Stable short ID for a monitor from its device path (which survives reconnection to
/// the same port, unlike `\\.\DISPLAYn`).
pub fn monitor_id(device_path: &str) -> String {
    // FNV-1a 64-bit over the lowercase path: stable across runs and Rust versions.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in device_path.to_lowercase().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("mon-{hash:016x}")
}

/// How long the real mouse must have been still before a cursor jump is attributed to touch.
pub const QUIET_MS: u64 = 150;

/// One poll of the cursor. Pointer-native applications (Chrome, `WebView2`) receive touch
/// without any mouse promotion, so no hook event marks the end of a tap; the only trace
/// is that Windows moved the cursor to the touched point.
#[derive(Clone, Copy, Debug)]
pub struct Observation {
    pub cursor: (i32, i32),
    pub previous: Option<(i32, i32)>,
    /// Last position reported by a real mouse.
    pub real: (i32, i32),
    pub since_real_ms: u64,
    /// A touch contact is down in an application that gets promoted mouse input;
    /// moving the cursor now would break its drag.
    pub touch_down: bool,
}

/// The cursor sits where the real mouse did not put it and has stopped moving, so the
/// touch that moved it has ended (or paused). The monitor check comes after this.
pub fn is_stray(o: &Observation) -> bool {
    let away = (o.cursor.0 - o.real.0)
        .abs()
        .max((o.cursor.1 - o.real.1).abs())
        > 1;
    !o.touch_down && o.since_real_ms >= QUIET_MS && o.previous == Some(o.cursor) && away
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Monitor {
    pub id: String,
    pub name: String,
    pub device_path: String,
    /// Windows GDI name such as `\\.\DISPLAY2` (changes with connection order).
    pub gdi_name: String,
    pub primary: bool,
    /// A touch digitizer is mapped to this monitor.
    pub touch: bool,
}

/// Enumerates connected monitors; the Windows implementation lives in `windows.rs`.
pub trait Displays: Send + Sync + 'static {
    fn monitors(&self) -> Result<Vec<Monitor>, String>;
}

/// "Keep cursor" settings shared by the API and the hook thread. Monitors without a
/// stored value keep the cursor (the default is on).
#[derive(Clone, Default)]
pub struct KeepCursor {
    overrides: Arc<RwLock<HashMap<String, bool>>>,
}

impl KeepCursor {
    pub fn new(overrides: HashMap<String, bool>) -> Self {
        Self {
            overrides: Arc::new(RwLock::new(overrides)),
        }
    }

    pub fn get(&self, monitor_id: &str) -> bool {
        self.overrides
            .read()
            .map_or(true, |map| map.get(monitor_id).copied().unwrap_or(true))
    }

    pub fn set(&self, monitor_id: &str, keep: bool) {
        if let Ok(mut map) = self.overrides.write() {
            map.insert(monitor_id.to_owned(), keep);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MonitorView {
    #[serde(flatten)]
    pub monitor: Monitor,
    pub keep_cursor: bool,
}

#[cfg(test)]
mod tests {
    use super::{KeepCursor, Observation, QUIET_MS, Source, classify, is_stray, monitor_id};

    #[test]
    fn classifies_touch_pen_and_mouse_by_signature() {
        assert_eq!(classify(0), Source::Mouse);
        assert_eq!(classify(0x1234), Source::Mouse);
        // Observed on this machine: real finger taps and InjectTouchInput carry 0xff5157xx
        // with the touch bit set.
        assert_eq!(classify(0xff51_57b4), Source::Touch);
        assert_eq!(classify(0xff51_57aa), Source::Touch);
        assert_eq!(classify(0xff51_5700), Source::Pen);
        assert_eq!(classify(0xff51_5701), Source::Pen);
        // Same low byte, different signature.
        assert_eq!(classify(0xff52_57b4), Source::Mouse);
    }

    #[test]
    fn monitor_id_is_stable_and_case_insensitive() {
        let path =
            r"\\?\DISPLAY#RTK0101#5&384b0f7a&0&UID4352#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}";
        let id = monitor_id(path);
        assert!(id.starts_with("mon-"));
        assert_eq!(id.len(), 4 + 16);
        assert_eq!(id, monitor_id(&path.to_uppercase()));
        assert_ne!(id, monitor_id(r"\\?\DISPLAY#IOC90CF#5&384b0f7a&0&UID4357"));
    }

    fn observation() -> Observation {
        Observation {
            cursor: (1378, 1950),
            previous: Some((1378, 1950)),
            real: (1280, 720),
            since_real_ms: 1000,
            touch_down: false,
        }
    }

    #[test]
    fn a_still_cursor_away_from_the_quiet_mouse_is_stray() {
        assert!(is_stray(&observation()));
    }

    #[test]
    fn not_stray_while_the_mouse_is_active_or_the_cursor_moves() {
        let recent = Observation {
            since_real_ms: QUIET_MS - 1,
            ..observation()
        };
        assert!(!is_stray(&recent));
        let moving = Observation {
            previous: Some((1300, 1900)),
            ..observation()
        };
        assert!(!is_stray(&moving));
        let first_tick = Observation {
            previous: None,
            ..observation()
        };
        assert!(!is_stray(&first_tick));
    }

    #[test]
    fn not_stray_during_a_promoted_touch_or_at_the_real_position() {
        let down = Observation {
            touch_down: true,
            ..observation()
        };
        assert!(!is_stray(&down));
        let home = Observation {
            cursor: (1281, 720),
            previous: Some((1281, 720)),
            ..observation()
        };
        assert!(!is_stray(&home));
    }

    #[test]
    fn keep_cursor_defaults_to_on_and_remembers_overrides() {
        let keep = KeepCursor::default();
        assert!(keep.get("mon-a"));
        keep.set("mon-a", false);
        assert!(!keep.get("mon-a"));
        assert!(keep.get("mon-b"));
        let clone = keep.clone();
        clone.set("mon-a", true);
        assert!(keep.get("mon-a"));
    }
}
