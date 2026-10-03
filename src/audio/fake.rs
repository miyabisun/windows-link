use std::{collections::HashMap, sync::Mutex};

use super::{Audio, AudioError, AudioResult, Device};

/// In-memory audio state for tests.
#[derive(Default)]
pub struct FakeAudio {
    pub state: Mutex<FakeState>,
}

#[derive(Default)]
pub struct FakeState {
    pub devices: Vec<Device>,
    pub default_output: Option<String>,
    /// process file name (lowercase) -> volume
    pub volumes: HashMap<String, f32>,
}

impl FakeAudio {
    pub fn new(devices: Vec<Device>, default_output: Option<&str>) -> Self {
        Self {
            state: Mutex::new(FakeState {
                devices,
                default_output: default_output.map(str::to_owned),
                volumes: HashMap::new(),
            }),
        }
    }

    pub fn set_volume(&self, process: &str, level: f32) {
        self.state
            .lock()
            .unwrap()
            .volumes
            .insert(process.to_lowercase(), level);
    }

    pub fn set_connected(&self, id: &str, connected: bool) {
        let mut state = self.state.lock().unwrap();
        if let Some(device) = state.devices.iter_mut().find(|d| d.id == id) {
            device.connected = connected;
        }
    }
}

impl Audio for FakeAudio {
    fn devices(&self) -> AudioResult<Vec<Device>> {
        Ok(self.state.lock().unwrap().devices.clone())
    }

    fn default_output(&self) -> AudioResult<Option<String>> {
        Ok(self.state.lock().unwrap().default_output.clone())
    }

    fn set_default_output(&self, id: &str) -> AudioResult<()> {
        let mut state = self.state.lock().unwrap();
        match state.devices.iter().find(|d| d.id == id) {
            Some(device) if device.connected => {
                state.default_output = Some(id.to_owned());
                Ok(())
            }
            _ => Err(AudioError(format!("{id} is not connected"))),
        }
    }

    fn app_volume(&self, process: &str) -> AudioResult<Option<f32>> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .volumes
            .get(&process.to_lowercase())
            .copied())
    }

    fn set_app_volume(&self, process: &str, level: f32) -> AudioResult<usize> {
        let mut state = self.state.lock().unwrap();
        match state.volumes.get_mut(&process.to_lowercase()) {
            Some(volume) => {
                *volume = level;
                Ok(1)
            }
            None => Ok(0),
        }
    }
}
