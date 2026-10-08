use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

use super::{AppSound, Audio, AudioError, AudioResult, Device, Master};

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
    /// process file names (lowercase) muted by themselves
    pub muted: HashSet<String>,
    pub master: Master,
}

impl FakeAudio {
    pub fn new(devices: Vec<Device>, default_output: Option<&str>) -> Self {
        Self {
            state: Mutex::new(FakeState {
                devices,
                default_output: default_output.map(str::to_owned),
                volumes: HashMap::new(),
                muted: HashSet::new(),
                master: Master {
                    volume: 0.5,
                    muted: false,
                },
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

    fn master(&self) -> AudioResult<Master> {
        Ok(self.state.lock().unwrap().master)
    }

    fn set_master(&self, volume: Option<f32>, muted: Option<bool>) -> AudioResult<()> {
        let mut state = self.state.lock().unwrap();
        if let Some(volume) = volume {
            state.master.volume = volume;
        }
        if let Some(muted) = muted {
            state.master.muted = muted;
        }
        Ok(())
    }

    /// One app per volume, named by its file name without `.exe`.
    fn apps(&self) -> AudioResult<Vec<AppSound>> {
        let state = self.state.lock().unwrap();
        let mut apps: Vec<AppSound> = state
            .volumes
            .iter()
            .map(|(process, volume)| AppSound {
                process: process.clone(),
                name: process.trim_end_matches(".exe").to_owned(),
                volume: *volume,
                muted: state.muted.contains(process),
            })
            .collect();
        apps.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(apps)
    }

    fn app_muted(&self, process: &str) -> AudioResult<Option<bool>> {
        let state = self.state.lock().unwrap();
        let process = process.to_lowercase();
        Ok(state
            .volumes
            .contains_key(&process)
            .then(|| state.muted.contains(&process)))
    }

    fn set_app_mute(&self, process: &str, muted: bool) -> AudioResult<usize> {
        let mut state = self.state.lock().unwrap();
        let process = process.to_lowercase();
        if !state.volumes.contains_key(&process) {
            return Ok(0);
        }
        if muted {
            state.muted.insert(process);
        } else {
            state.muted.remove(&process);
        }
        Ok(1)
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
