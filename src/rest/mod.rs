//! Resting the PC instead of putting it to sleep: every monitor but the panel's is
//! darkened (its brightness set to 0 over DDC/CI and a black window laid over it) until
//! the panel wakes them. Nothing is unplugged, unlike sleep: waking from sleep re-plugs
//! the USB hubs, the panel's monitor and their devices, and turning a monitor's power
//! off over DDC/CI re-plugs its built-in hub (both observed on 2026-10-26). The PC is
//! muted while resting, unless it was muted already. Each rest, wake and restart while
//! resting is logged, what went through as well as what did not.

use std::sync::{Arc, Mutex, PoisonError};

use tracing::{info, warn};

use crate::{
    audio::Audio,
    touch::{Displays, Monitor},
};

pub mod api;
pub mod store;
pub mod windows;

use store::RestStore;

/// What darkens and lights the monitors; the Windows one lives in `windows.rs`.
pub trait Screens: Send + Sync + 'static {
    /// The monitor's brightness over DDC/CI, or `None` when it does not answer.
    fn brightness(&self, monitor: &Monitor) -> Option<u32>;
    fn set_brightness(&self, monitor: &Monitor, value: u32) -> Result<(), String>;
    /// Lay black windows over exactly these monitors; none lifts them all.
    fn cover(&self, monitors: &[Monitor]);
}

#[derive(Debug, PartialEq, Eq)]
pub enum RestError {
    /// The monitor to keep lit is not connected.
    UnknownMonitor,
    /// The saved brightness could not be put back on these monitors; it stays saved
    /// for the next wake.
    Unrestored(Vec<String>),
    Failed(String),
}

impl From<rusqlite::Error> for RestError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Failed(err.to_string())
    }
}

/// The monitors to darken: every one but `keep`, or `None` when `keep` is not connected.
pub fn darkened(monitors: &[Monitor], keep: &str) -> Option<Vec<Monitor>> {
    monitors
        .iter()
        .any(|m| m.id == keep)
        .then(|| monitors.iter().filter(|m| m.id != keep).cloned().collect())
}

fn names<'a>(monitors: impl IntoIterator<Item = &'a Monitor>) -> Vec<&'a str> {
    monitors.into_iter().map(|m| m.name.trim()).collect()
}

pub struct Rest {
    displays: Arc<dyn Displays>,
    screens: Arc<dyn Screens>,
    audio: Arc<dyn Audio>,
    store: RestStore,
    /// Resting and waking one at a time, so neither sees the other half done.
    busy: Mutex<()>,
}

impl Rest {
    pub fn new(
        displays: Arc<dyn Displays>,
        screens: Arc<dyn Screens>,
        audio: Arc<dyn Audio>,
        store: RestStore,
    ) -> Self {
        Self {
            displays,
            screens,
            audio,
            store,
            busy: Mutex::new(()),
        }
    }

    fn monitors(&self) -> Result<Vec<Monitor>, RestError> {
        self.displays.monitors().map_err(RestError::Failed)
    }

    pub fn resting(&self) -> Result<bool, RestError> {
        Ok(self.store.keep()?.is_some())
    }

    /// Darken every monitor but `keep`. Resting already changes nothing.
    pub fn rest(&self, keep: &str) -> Result<(), RestError> {
        let _busy = self.busy.lock().unwrap_or_else(PoisonError::into_inner);
        if self.resting()? {
            return Ok(());
        }
        let monitors = self.monitors()?;
        let others = darkened(&monitors, keep).ok_or(RestError::UnknownMonitor)?;
        // A value still saved from a wake that could not put it back is the monitor's
        // own; reading now would save the 0 it was left at.
        let mut saved = self.store.brightness()?;
        let read: Vec<(String, u32)> = others
            .iter()
            .filter(|m| !saved.contains_key(&m.id))
            .filter_map(|m| Some((m.id.clone(), self.screens.brightness(m)?)))
            .collect();
        // Saved before anything is dimmed, so no monitor is left at 0 without its value.
        self.store.start(keep, &read)?;
        saved.extend(read);
        for monitor in others.iter().filter(|m| saved.contains_key(&m.id)) {
            if let Err(message) = self.screens.set_brightness(monitor, 0) {
                warn!(monitor = %monitor.name, %message, "cannot darken the monitor");
            }
        }
        self.screens.cover(&others);
        self.mute()?;
        let darkened: Vec<String> = others
            .iter()
            .filter_map(|m| Some(format!("{}={}", m.name.trim(), saved.get(&m.id)?)))
            .collect();
        info!(
            keep = monitors.iter().find(|m| m.id == keep).map_or(keep, |m| m.name.trim()),
            ?darkened,
            covered = ?names(&others),
            muted = self.store.muted()?,
            "resting"
        );
        Ok(())
    }

    /// Mute the PC unless it is muted already, remembering to unmute it on waking.
    fn mute(&self) -> Result<(), RestError> {
        match self.audio.master() {
            Ok(master) if !master.muted => {
                self.store.set_muted(true)?;
                if let Err(err) = self.audio.set_master(None, Some(true)) {
                    warn!(%err, "cannot mute the PC");
                    self.store.set_muted(false)?;
                }
            }
            Ok(_) => {}
            Err(err) => warn!(%err, "cannot read whether the PC is muted"),
        }
        Ok(())
    }

    /// Put each saved brightness back and lift the black windows.
    pub fn wake(&self) -> Result<(), RestError> {
        let _busy = self.busy.lock().unwrap_or_else(PoisonError::into_inner);
        self.wake_now()
    }

    fn wake_now(&self) -> Result<(), RestError> {
        let saved = self.store.brightness()?;
        let monitors = if saved.is_empty() {
            Vec::new()
        } else {
            self.monitors()?
        };
        let mut restored = Vec::new();
        let mut unrestored = Vec::new();
        for (id, value) in &saved {
            let monitor = monitors.iter().find(|m| &m.id == id);
            match monitor.map(|m| self.put_back(m, *value)) {
                Some(Ok(())) => {
                    self.store.forget(id)?;
                    restored.extend(monitor.map(|m| format!("{}={value}", m.name.trim())));
                }
                Some(Err(message)) => {
                    warn!(monitor = %id, %message, "cannot put the brightness back");
                    unrestored.push(monitor.map_or(id.as_str(), |m| m.name.trim()).to_owned());
                }
                None => unrestored.push(id.clone()),
            }
        }
        self.screens.cover(&[]);
        if self.store.muted()? {
            match self.audio.set_master(None, Some(false)) {
                Ok(()) => self.store.set_muted(false)?,
                Err(err) => warn!(%err, "cannot unmute the PC; the next wake tries again"),
            }
        }
        self.store.stop()?;
        restored.sort();
        unrestored.sort();
        info!(?restored, ?unrestored, muted = self.store.muted()?, "woke");
        if unrestored.is_empty() {
            Ok(())
        } else {
            Err(RestError::Unrestored(unrestored))
        }
    }

    /// Set `value` and read it back: a monitor may take the command over DDC/CI and keep
    /// its own brightness. A monitor that answers with the value may still stay dark.
    fn put_back(&self, monitor: &Monitor, value: u32) -> Result<(), String> {
        self.screens.set_brightness(monitor, value)?;
        let name = monitor.name.trim();
        match self.screens.brightness(monitor) {
            Some(read) if read == value => Ok(()),
            Some(read) => Err(format!("{name} reads {read} after being set to {value}")),
            None => Err(format!("{name} does not answer after being set to {value}")),
        }
    }

    /// After a restart while resting (an update replaces the process): lay the black
    /// windows again, the brightness being still down. Without the kept monitor there
    /// is no panel to wake from, so wake instead.
    pub fn resume(&self) -> Result<(), RestError> {
        let _busy = self.busy.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(keep) = self.store.keep()? else {
            return Ok(());
        };
        match darkened(&self.monitors()?, &keep) {
            Some(others) => {
                self.screens.cover(&others);
                info!(covered = ?names(&others), "covered the monitors again after a restart");
                Ok(())
            }
            None => self.wake_now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use super::{Rest, RestError, Screens, darkened, store::RestStore};
    use crate::{
        audio::{Audio, fake::FakeAudio},
        touch::{Displays, Monitor},
    };

    fn monitor(id: &str) -> Monitor {
        Monitor {
            id: id.into(),
            name: format!("{id} name"),
            device_path: format!("path-{id}"),
            gdi_name: format!(r"\\.\{id}"),
            primary: id == "main",
            touch: id == "panel",
        }
    }

    struct FakeDisplays(Mutex<Vec<Monitor>>);

    impl Displays for FakeDisplays {
        fn monitors(&self) -> Result<Vec<Monitor>, String> {
            Ok(self.0.lock().unwrap().clone())
        }
    }

    /// Monitors' brightness by id (`None`: no DDC/CI), the ids that refuse a new value,
    /// the ids that accept one and keep their own, and the ids covered.
    #[derive(Default)]
    struct FakeScreens {
        brightness: Mutex<HashMap<String, Option<u32>>>,
        refuse: Mutex<Vec<String>>,
        ignore: Mutex<Vec<String>>,
        covered: Mutex<Vec<String>>,
    }

    impl Screens for FakeScreens {
        fn brightness(&self, monitor: &Monitor) -> Option<u32> {
            self.brightness
                .lock()
                .unwrap()
                .get(&monitor.id)
                .copied()
                .flatten()
        }

        fn set_brightness(&self, monitor: &Monitor, value: u32) -> Result<(), String> {
            if self.refuse.lock().unwrap().contains(&monitor.id) {
                return Err(format!("{} refused", monitor.id));
            }
            if self.ignore.lock().unwrap().contains(&monitor.id) {
                return Ok(());
            }
            let mut all = self.brightness.lock().unwrap();
            match all.get_mut(&monitor.id) {
                Some(Some(current)) => {
                    *current = value;
                    Ok(())
                }
                _ => Err(format!("{} has no DDC/CI", monitor.id)),
            }
        }

        fn cover(&self, monitors: &[Monitor]) {
            *self.covered.lock().unwrap() = monitors.iter().map(|m| m.id.clone()).collect();
        }
    }

    impl FakeScreens {
        fn level(&self, id: &str) -> Option<u32> {
            self.brightness.lock().unwrap()[id]
        }

        fn covered(&self) -> Vec<String> {
            self.covered.lock().unwrap().clone()
        }
    }

    /// The panel (no DDC/CI, as the T10FA), a main monitor at 40, a side one at 70 and
    /// one without DDC/CI.
    fn setup() -> (Rest, Arc<FakeDisplays>, Arc<FakeScreens>, Arc<FakeAudio>) {
        let ids = ["panel", "main", "side", "plain"];
        let displays = Arc::new(FakeDisplays(Mutex::new(
            ids.iter().map(|id| monitor(id)).collect(),
        )));
        let screens = Arc::new(FakeScreens::default());
        *screens.brightness.lock().unwrap() = HashMap::from([
            ("panel".into(), None),
            ("main".into(), Some(40)),
            ("side".into(), Some(70)),
            ("plain".into(), None),
        ]);
        let audio = Arc::new(FakeAudio::new(Vec::new(), None));
        let rest = Rest::new(
            displays.clone(),
            screens.clone(),
            audio.clone(),
            RestStore::in_memory().unwrap(),
        );
        (rest, displays, screens, audio)
    }

    fn muted(audio: &FakeAudio) -> bool {
        audio.master().unwrap().muted
    }

    #[test]
    fn darkens_every_monitor_but_the_kept_one() {
        let all = vec![monitor("panel"), monitor("main"), monitor("side")];
        let ids = |list: Vec<Monitor>| list.into_iter().map(|m| m.id).collect::<Vec<_>>();
        assert_eq!(ids(darkened(&all, "panel").unwrap()), ["main", "side"]);
        assert_eq!(ids(darkened(&all, "main").unwrap()), ["panel", "side"]);
        assert!(darkened(&all, "gone").is_none());
        assert_eq!(
            ids(darkened(&[monitor("panel")], "panel").unwrap()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn resting_saves_and_zeroes_each_brightness_and_covers_the_others() {
        let (rest, _, screens, _) = setup();
        assert!(!rest.resting().unwrap());
        rest.rest("panel").unwrap();
        assert!(rest.resting().unwrap());
        assert_eq!(screens.level("main"), Some(0));
        assert_eq!(screens.level("side"), Some(0));
        assert_eq!(screens.covered(), ["main", "side", "plain"]);
        assert_eq!(
            rest.store.brightness().unwrap(),
            HashMap::from([("main".into(), 40), ("side".into(), 70)])
        );
    }

    #[test]
    fn a_monitor_whose_brightness_cannot_be_read_is_left_as_it_is() {
        let (rest, _, screens, _) = setup();
        screens
            .brightness
            .lock()
            .unwrap()
            .insert("plain".into(), None);
        rest.rest("panel").unwrap();
        assert!(!rest.store.brightness().unwrap().contains_key("plain"));
        assert_eq!(screens.level("plain"), None);
    }

    #[test]
    fn resting_again_keeps_the_saved_brightness() {
        let (rest, _, screens, _) = setup();
        rest.rest("panel").unwrap();
        rest.rest("panel").unwrap();
        assert_eq!(rest.store.brightness().unwrap()["main"], 40);
        rest.wake().unwrap();
        assert_eq!(screens.level("main"), Some(40));
    }

    #[test]
    fn waking_puts_the_brightness_back_and_lifts_the_cover() {
        let (rest, _, screens, _) = setup();
        rest.rest("panel").unwrap();
        rest.wake().unwrap();
        assert!(!rest.resting().unwrap());
        assert_eq!(screens.level("main"), Some(40));
        assert_eq!(screens.level("side"), Some(70));
        assert!(screens.covered().is_empty());
        assert!(rest.store.brightness().unwrap().is_empty());
    }

    #[test]
    fn waking_when_not_resting_changes_nothing() {
        let (rest, _, screens, _) = setup();
        rest.wake().unwrap();
        assert_eq!(screens.level("main"), Some(40));
        assert!(!rest.resting().unwrap());
    }

    #[test]
    fn a_brightness_that_cannot_be_put_back_stays_saved_for_the_next_wake() {
        let (rest, displays, screens, _) = setup();
        rest.rest("panel").unwrap();
        screens.refuse.lock().unwrap().push("main".into());
        displays.0.lock().unwrap().retain(|m| m.id != "side");
        assert_eq!(
            rest.wake(),
            Err(RestError::Unrestored(vec![
                "main name".into(),
                "side".into()
            ]))
        );
        assert!(!rest.resting().unwrap());
        assert!(screens.covered().is_empty());
        assert_eq!(
            rest.store.brightness().unwrap(),
            HashMap::from([("main".into(), 40), ("side".into(), 70)])
        );
        // Resting again keeps the values saved; the next wake puts them back.
        rest.rest("panel").unwrap();
        assert_eq!(rest.store.brightness().unwrap()["main"], 40);
        screens.refuse.lock().unwrap().clear();
        displays.0.lock().unwrap().push(monitor("side"));
        rest.wake().unwrap();
        assert_eq!(screens.level("main"), Some(40));
        assert_eq!(screens.level("side"), Some(70));
        assert!(rest.store.brightness().unwrap().is_empty());
    }

    #[test]
    fn a_brightness_the_monitor_does_not_take_stays_saved_for_the_next_wake() {
        let (rest, _, screens, _) = setup();
        rest.rest("panel").unwrap();
        // DDC/CI accepts the value, and the monitor reads 0 still.
        screens.ignore.lock().unwrap().push("main".into());
        assert_eq!(
            rest.wake(),
            Err(RestError::Unrestored(vec!["main name".into()]))
        );
        assert_eq!(screens.level("main"), Some(0));
        assert_eq!(screens.level("side"), Some(70));
        assert!(screens.covered().is_empty());
        assert_eq!(
            rest.store.brightness().unwrap(),
            HashMap::from([("main".into(), 40)])
        );
        screens.ignore.lock().unwrap().clear();
        rest.wake().unwrap();
        assert_eq!(screens.level("main"), Some(40));
        assert!(rest.store.brightness().unwrap().is_empty());
    }

    #[test]
    fn an_unknown_monitor_to_keep_changes_nothing() {
        let (rest, _, screens, audio) = setup();
        assert_eq!(rest.rest("gone"), Err(RestError::UnknownMonitor));
        assert!(!muted(&audio));
        assert!(!rest.resting().unwrap());
        assert_eq!(screens.level("main"), Some(40));
        assert!(screens.covered().is_empty());
    }

    #[test]
    fn resting_mutes_the_pc_and_waking_unmutes_it() {
        let (rest, _, _, audio) = setup();
        rest.rest("panel").unwrap();
        assert!(muted(&audio));
        rest.wake().unwrap();
        assert!(!muted(&audio));
    }

    #[test]
    fn a_pc_muted_before_resting_stays_muted_after_waking() {
        let (rest, _, _, audio) = setup();
        audio.set_master(None, Some(true)).unwrap();
        rest.rest("panel").unwrap();
        rest.wake().unwrap();
        assert!(muted(&audio));
    }

    #[test]
    fn a_restart_while_resting_covers_again_and_can_still_wake() {
        let (rest, displays, screens, audio) = setup();
        rest.rest("panel").unwrap();
        // A new process: the same store, nothing covered yet.
        let restarted = Rest::new(displays, screens.clone(), audio.clone(), rest.store);
        screens.cover(&[]);
        restarted.resume().unwrap();
        assert_eq!(screens.covered(), ["main", "side", "plain"]);
        restarted.wake().unwrap();
        assert_eq!(screens.level("main"), Some(40));
        assert!(screens.covered().is_empty());
        assert!(!muted(&audio));
    }

    #[test]
    fn a_restart_without_the_kept_monitor_wakes() {
        let (rest, displays, screens, audio) = setup();
        rest.rest("panel").unwrap();
        displays.0.lock().unwrap().retain(|m| m.id != "panel");
        let restarted = Rest::new(displays, screens.clone(), audio.clone(), rest.store);
        restarted.resume().unwrap();
        assert!(!restarted.resting().unwrap());
        assert_eq!(screens.level("main"), Some(40));
        assert!(screens.covered().is_empty());
        assert!(!muted(&audio));
    }

    #[test]
    fn resuming_when_not_resting_does_nothing() {
        let (rest, _, screens, _) = setup();
        rest.resume().unwrap();
        assert!(screens.covered().is_empty());
        assert!(!rest.resting().unwrap());
    }
}
