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
}
