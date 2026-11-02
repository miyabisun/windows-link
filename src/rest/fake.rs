//! A resting PC for tests: a panel and a main monitor at brightness 40.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use super::{Rest, Screens, store::RestStore};
use crate::{
    audio::fake::FakeAudio,
    touch::{Displays, Monitor},
};

struct FakeDisplays(Vec<Monitor>);

impl Displays for FakeDisplays {
    fn monitors(&self) -> Result<Vec<Monitor>, String> {
        Ok(self.0.clone())
    }
}

/// Brightness by monitor id; `refuse` makes every write fail.
#[derive(Default)]
pub struct FakeScreens {
    pub levels: Mutex<HashMap<String, u32>>,
    pub refuse: Mutex<bool>,
}

impl Screens for FakeScreens {
    fn brightness(&self, monitor: &Monitor) -> Option<u32> {
        self.levels.lock().unwrap().get(&monitor.id).copied()
    }

    fn set_brightness(&self, monitor: &Monitor, value: u32) -> Result<(), String> {
        if *self.refuse.lock().unwrap() {
            return Err("refused".into());
        }
        self.levels
            .lock()
            .unwrap()
            .insert(monitor.id.clone(), value);
        Ok(())
    }

    fn cover(&self, _: &[Monitor]) {}
}

fn monitor(id: &str) -> Monitor {
    Monitor {
        id: id.into(),
        name: id.to_uppercase(),
        device_path: format!("path-{id}"),
        gdi_name: format!(r"\\.\{id}"),
        primary: false,
        touch: false,
    }
}

/// Monitors `panel` and `main`, waking without waits between tries.
pub fn rest() -> (Arc<Rest>, Arc<FakeScreens>) {
    let screens = Arc::new(FakeScreens::default());
    screens.levels.lock().unwrap().insert("main".into(), 40);
    let mut rest = Rest::new(
        Arc::new(FakeDisplays(vec![monitor("panel"), monitor("main")])),
        screens.clone(),
        Arc::new(FakeAudio::new(Vec::new(), None)),
        RestStore::in_memory().unwrap(),
    );
    rest.retry_waits.clear();
    (Arc::new(rest), screens)
}
