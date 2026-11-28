//! Audio control surface used by the buttons. The Windows implementation lives in
//! `windows.rs`; tests use `fake::FakeAudio`.

use std::fmt;

use serde::Serialize;

#[cfg(test)]
pub mod fake;
pub mod windows;

/// An audio render endpoint.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// `true` only when Windows reports the endpoint as active.
    pub connected: bool,
}

/// The endpoint to use for the configured `id`: itself while it is connected, otherwise
/// a connected endpoint that is the same device. A USB device back on another port gets
/// a new endpoint from Windows, named with a count (`Speakers (2- D3V)`), while the old
/// one stays, not connected. `None` when Windows does not know `id`.
pub fn resolve<'a>(devices: &'a [Device], id: &str) -> Option<&'a Device> {
    let own = devices.iter().find(|d| d.id == id)?;
    if own.connected {
        return Some(own);
    }
    let name = without_count(&own.name);
    let twin = devices
        .iter()
        .find(|d| d.connected && without_count(&d.name) == name);
    Some(twin.unwrap_or(own))
}

/// An endpoint's name, `what it is called (device)`, in those two parts, without the
/// count Windows puts before a device it has met again.
// ponytail: by name, so an output the user renamed is not found under its new ID;
// compare the devices' hardware IDs if that comes up.
fn without_count(name: &str) -> (&str, &str) {
    let (called, device) = name.split_once('(').unwrap_or((name, ""));
    let rest = device.trim_start_matches(|c: char| c.is_ascii_digit());
    match rest.strip_prefix("- ") {
        Some(counted) if rest.len() < device.len() => (called, counted),
        _ => (called, device),
    }
}

/// The default output's volume (0.0-1.0) and whether it is muted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Master {
    pub volume: f32,
    pub muted: bool,
}

/// An app with sound on the default output: its program's file name, a name to show,
/// its volume (0.0-1.0) and whether it is muted by itself.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AppSound {
    pub process: String,
    pub name: String,
    pub volume: f32,
    pub muted: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioError(pub String);

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AudioError {}

pub type AudioResult<T> = Result<T, AudioError>;

/// Stand-in used when Windows audio cannot be opened: the server keeps running and
/// every audio button reports this error as its state.
pub struct UnavailableAudio(pub String);

impl Audio for UnavailableAudio {
    fn devices(&self) -> AudioResult<Vec<Device>> {
        Err(AudioError(self.0.clone()))
    }

    fn default_output(&self) -> AudioResult<Option<String>> {
        Err(AudioError(self.0.clone()))
    }

    fn set_default_output(&self, _: &str) -> AudioResult<()> {
        Err(AudioError(self.0.clone()))
    }

    fn app_volume(&self, _: &str) -> AudioResult<Option<f32>> {
        Err(AudioError(self.0.clone()))
    }

    fn set_app_volume(&self, _: &str, _: f32) -> AudioResult<usize> {
        Err(AudioError(self.0.clone()))
    }

    fn app_muted(&self, _: &str) -> AudioResult<Option<bool>> {
        Err(AudioError(self.0.clone()))
    }

    fn set_app_mute(&self, _: &str, _: bool) -> AudioResult<usize> {
        Err(AudioError(self.0.clone()))
    }

    fn master(&self) -> AudioResult<Master> {
        Err(AudioError(self.0.clone()))
    }

    fn set_master(&self, _: Option<f32>, _: Option<bool>) -> AudioResult<()> {
        Err(AudioError(self.0.clone()))
    }

    fn apps(&self, _: Outputs) -> AudioResult<Vec<AppSound>> {
        Err(AudioError(self.0.clone()))
    }
}

/// Blocking calls; the server runs them on the blocking pool.
pub trait Audio: Send + Sync + 'static {
    /// All render endpoints Windows knows about, connected or not.
    fn devices(&self) -> AudioResult<Vec<Device>>;
    /// Endpoint ID of the current default output, if any.
    fn default_output(&self) -> AudioResult<Option<String>>;
    /// Make `id` the default output for every role.
    fn set_default_output(&self, id: &str) -> AudioResult<()>;
    /// Volume of the first live session owned by `process` (file name), if any.
    fn app_volume(&self, process: &str) -> AudioResult<Option<f32>>;
    /// Set the volume of every live session owned by `process`; returns the count.
    fn set_app_volume(&self, process: &str, level: f32) -> AudioResult<usize>;
    /// Whether every live session owned by `process` is muted, if it has one.
    fn app_muted(&self, process: &str) -> AudioResult<Option<bool>>;
    /// Mute or unmute every live session owned by `process`, as the speaker in
    /// Windows' volume mixer does; returns the count.
    fn set_app_mute(&self, process: &str, muted: bool) -> AudioResult<usize>;
    /// The default output's volume and mute.
    fn master(&self) -> AudioResult<Master>;
    /// Change the default output's volume, its mute, or both.
    fn set_master(&self, volume: Option<f32>, muted: Option<bool>) -> AudioResult<()>;
    /// The apps with sound on `outputs`, one per program, by name.
    fn apps(&self, outputs: Outputs) -> AudioResult<Vec<AppSound>>;
}

/// Which outputs' apps to read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outputs {
    /// The default output, as Windows' volume mixer shows.
    Default,
    /// Every connected output, as the app buttons follow a program.
    Connected,
}

/// One app per program from its live sessions (program file name, volume when it can
/// be read, muted), by name: the first session's volume, muted while every session is.
/// A program's name is its file name without `.exe`.
pub fn app_sounds(
    sessions: impl IntoIterator<Item = (String, Option<f32>, bool)>,
) -> Vec<AppSound> {
    let mut apps: Vec<AppSound> = Vec::new();
    for (process, volume, muted) in sessions {
        if let Some(app) = apps
            .iter_mut()
            .find(|a| a.process.eq_ignore_ascii_case(&process))
        {
            app.muted &= muted;
        } else if let Some(volume) = volume {
            let name = std::path::Path::new(&process)
                .file_stem()
                .map_or_else(|| process.clone(), |s| s.to_string_lossy().into_owned());
            apps.push(AppSound {
                process,
                name,
                volume,
                muted,
            });
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

#[cfg(test)]
mod tests {
    use super::{AppSound, Device, app_sounds, resolve};

    fn device(id: &str, name: &str, connected: bool) -> Device {
        Device {
            id: id.into(),
            name: name.into(),
            connected,
        }
    }

    #[test]
    fn a_device_back_under_another_id_stands_for_the_one_configured() {
        let known = [
            device("old", "スピーカー (ADAM Audio D3V  )", false),
            device("new", "スピーカー (2- ADAM Audio D3V  )", true),
            device("third", "スピーカー (3- ADAM Audio D3V  )", false),
            device("jbl", "BTイヤホン (JBL Tour Pro 3)", true),
        ];
        let id = |id: &str| resolve(&known, id).map(|d| d.id.as_str());
        assert_eq!(id("old"), Some("new"), "the count Windows adds");
        assert_eq!(id("third"), Some("new"), "between two counts");
        assert_eq!(id("new"), Some("new"), "connected: itself");
        assert_eq!(id("jbl"), Some("jbl"));
        assert_eq!(id("gone"), None, "Windows does not know it");
    }

    #[test]
    fn a_disconnected_device_stays_itself_without_a_connected_twin() {
        let known = [
            device("main", "MOTU (MOTU M Series)", false),
            // Another output of the same device is not the one configured.
            device("out34", "Out 3-4 (MOTU M Series)", true),
            device("back", "MOTU (2- MOTU M Series)", false),
            // The count is before the device, not before what the output is called.
            device(
                "hdmi",
                "2 - T10FA (AMD High Definition Audio Device)",
                false,
            ),
            device("hdmi-1", "T10FA (AMD High Definition Audio Device)", true),
            device("r", "Speakers (2- Realtek(R) Audio)", false),
            device("r-2", "Speakers (Realtek(R) Audio)", true),
        ];
        let id = |id: &str| resolve(&known, id).map(|d| d.id.as_str());
        assert_eq!(id("main"), Some("main"));
        assert_eq!(id("hdmi"), Some("hdmi"));
        assert_eq!(id("r"), Some("r-2"), "a device name with brackets");
    }

    #[test]
    fn a_program_is_one_app_with_its_first_volume_muted_while_all_its_sessions_are() {
        let app = |process: &str, name: &str, volume: f32, muted: bool| AppSound {
            process: process.into(),
            name: name.into(),
            volume,
            muted,
        };
        let apps = app_sounds([
            ("Steam.exe".to_owned(), Some(0.4), true),
            ("BlueArchive.exe".to_owned(), Some(0.7), true),
            // A session whose volume cannot be read does not make the app.
            ("zen.exe".to_owned(), None, false),
            ("bluearchive.exe".to_owned(), Some(0.2), false),
            ("steam.exe".to_owned(), Some(1.0), true),
            ("zen.exe".to_owned(), Some(0.5), false),
        ]);
        assert_eq!(
            apps,
            [
                app("BlueArchive.exe", "BlueArchive", 0.7, false),
                app("Steam.exe", "Steam", 0.4, true),
                app("zen.exe", "zen", 0.5, false),
            ]
        );
    }
}
