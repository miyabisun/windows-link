//! Windows Core Audio implementation. COM objects stay on one MTA worker thread;
//! callers talk to it through a command channel.

// COM method names (IPolicyConfig) keep their Windows spelling.
#![allow(
    non_snake_case,
    clippy::inline_always,
    clippy::ref_as_ptr,
    clippy::transmute_ptr_to_ptr
)]

use std::{
    cell::RefCell,
    ffi::c_void,
    path::Path,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use tokio::sync::Notify;
use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Foundation::{CloseHandle, PROPERTYKEY},
        Media::Audio::{
            AUDIO_VOLUME_NOTIFICATION_DATA, AudioSessionDisconnectReason, AudioSessionState,
            AudioSessionStateExpired, DEVICE_STATE, DEVICE_STATE_ACTIVE, DEVICE_STATEMASK_ALL,
            EDataFlow, ERole,
            Endpoints::{
                IAudioEndpointVolume, IAudioEndpointVolumeCallback,
                IAudioEndpointVolumeCallback_Impl,
            },
            IAudioSessionControl, IAudioSessionControl2, IAudioSessionEvents,
            IAudioSessionEvents_Impl, IAudioSessionManager2, IAudioSessionNotification,
            IAudioSessionNotification_Impl, IMMDevice, IMMDeviceEnumerator, IMMNotificationClient,
            IMMNotificationClient_Impl, ISimpleAudioVolume, MMDeviceEnumerator, eCommunications,
            eConsole, eMultimedia, eRender,
        },
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
                STGM_READ,
            },
            Threading::{
                OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            },
        },
    },
    core::{
        BOOL, GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, PCWSTR, PWSTR, Ref, implement,
        interface,
    },
};

use super::{AppSound, Audio, AudioError, AudioResult, Device, Master, Outputs};

/// Undocumented but long-stable interface used to change the default endpoint
/// (the same one `SoundSwitch` and `AudioDeviceCmdlets` use). Only `SetDefaultEndpoint`
/// is called; the earlier slots keep the vtable layout.
#[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: IUnknown {
    fn GetMixFormat(&self, device: PCWSTR, format: *mut *mut c_void) -> HRESULT;
    fn GetDeviceFormat(&self, device: PCWSTR, default: i32, format: *mut *mut c_void) -> HRESULT;
    fn ResetDeviceFormat(&self, device: PCWSTR) -> HRESULT;
    fn SetDeviceFormat(&self, device: PCWSTR, endpoint: *mut c_void, mix: *mut c_void) -> HRESULT;
    fn GetProcessingPeriod(
        &self,
        device: PCWSTR,
        default: i32,
        default_period: *mut i64,
        min_period: *mut i64,
    ) -> HRESULT;
    fn SetProcessingPeriod(&self, device: PCWSTR, period: *mut i64) -> HRESULT;
    fn GetShareMode(&self, device: PCWSTR, mode: *mut c_void) -> HRESULT;
    fn SetShareMode(&self, device: PCWSTR, mode: *mut c_void) -> HRESULT;
    fn GetPropertyValue(
        &self,
        device: PCWSTR,
        store: i32,
        key: *const PROPERTYKEY,
        value: *mut c_void,
    ) -> HRESULT;
    fn SetPropertyValue(
        &self,
        device: PCWSTR,
        store: i32,
        key: *const PROPERTYKEY,
        value: *mut c_void,
    ) -> HRESULT;
    fn SetDefaultEndpoint(&self, device: PCWSTR, role: ERole) -> HRESULT;
    fn SetEndpointVisibility(&self, device: PCWSTR, visible: i32) -> HRESULT;
}

const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870a_f99c_171d_4f9e_af0d_e63d_f40c_2bc9);

type Reply<T> = mpsc::Sender<AudioResult<T>>;

enum Command {
    Devices(Reply<Vec<Device>>),
    DefaultOutput(Reply<Option<String>>),
    SetDefaultOutput(String, Reply<()>),
    AppVolume(String, Reply<Option<f32>>),
    SetAppVolume(String, f32, Reply<usize>),
    AppMuted(String, Reply<Option<bool>>),
    SetAppMute(String, bool, Reply<usize>),
    Master(Reply<Master>),
    SetMaster(Option<f32>, Option<bool>, Reply<()>),
    Apps(Outputs, Reply<Vec<AppSound>>),
}

pub struct WindowsAudio {
    commands: mpsc::Sender<Command>,
}

impl WindowsAudio {
    /// Start the COM worker. `changes` is notified when Windows reports a device
    /// or default-endpoint change.
    pub fn start(changes: Option<Arc<Notify>>) -> AudioResult<Self> {
        let (commands, receiver) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        thread::Builder::new()
            .name("audio-com".into())
            .spawn(move || worker(&receiver, changes, &ready_tx))
            .map_err(|err| AudioError(format!("cannot start audio thread: {err}")))?;
        ready_rx
            .recv()
            .map_err(|_| AudioError("audio thread exited during start".into()))??;
        Ok(Self { commands })
    }

    fn call<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> AudioResult<T> {
        let (tx, rx) = mpsc::channel();
        self.commands
            .send(make(tx))
            .map_err(|_| AudioError("audio thread stopped".into()))?;
        rx.recv()
            .map_err(|_| AudioError("audio thread stopped".into()))?
    }
}

impl Audio for WindowsAudio {
    fn devices(&self) -> AudioResult<Vec<Device>> {
        self.call(Command::Devices)
    }

    fn default_output(&self) -> AudioResult<Option<String>> {
        self.call(Command::DefaultOutput)
    }

    fn set_default_output(&self, id: &str) -> AudioResult<()> {
        self.call(|reply| Command::SetDefaultOutput(id.to_owned(), reply))
    }

    fn app_volume(&self, process: &str) -> AudioResult<Option<f32>> {
        self.call(|reply| Command::AppVolume(process.to_owned(), reply))
    }

    fn set_app_volume(&self, process: &str, level: f32) -> AudioResult<usize> {
        self.call(|reply| Command::SetAppVolume(process.to_owned(), level, reply))
    }

    fn app_muted(&self, process: &str) -> AudioResult<Option<bool>> {
        self.call(|reply| Command::AppMuted(process.to_owned(), reply))
    }

    fn set_app_mute(&self, process: &str, muted: bool) -> AudioResult<usize> {
        self.call(|reply| Command::SetAppMute(process.to_owned(), muted, reply))
    }

    fn master(&self) -> AudioResult<Master> {
        self.call(Command::Master)
    }

    fn set_master(&self, volume: Option<f32>, muted: Option<bool>) -> AudioResult<()> {
        self.call(|reply| Command::SetMaster(volume, muted, reply))
    }

    fn apps(&self, outputs: Outputs) -> AudioResult<Vec<AppSound>> {
        self.call(|reply| Command::Apps(outputs, reply))
    }
}

/// What Windows' notices tell: whoever reads the buttons, and the worker.
struct Signals {
    changes: Option<Arc<Notify>>,
    /// Set so the worker reads the endpoints again (`ComState::endpoints`).
    stale: Arc<AtomicBool>,
}

impl Signals {
    /// A volume or a mute changed: the buttons are read again.
    fn changed(&self) {
        if let Some(changes) = &self.changes {
            changes.notify_one();
        }
    }

    /// The endpoints or their sessions changed: they are read again too.
    fn rearranged(&self) {
        self.stale.store(true, Ordering::Release);
        self.changed();
    }
}

/// The endpoints coming, going and changing.
#[implement(IMMNotificationClient)]
struct ChangeNotifier(Arc<Signals>);

impl IMMNotificationClient_Impl for ChangeNotifier_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        self.0.rearranged();
        Ok(())
    }

    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        self.0.rearranged();
        Ok(())
    }

    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        self.0.rearranged();
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        _: ERole,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        if flow == eRender {
            self.0.rearranged();
        }
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

/// The default output's volume and mute changing (the volume keys, Windows' mixer).
#[implement(IAudioEndpointVolumeCallback)]
struct VolumeListener(Arc<Signals>);

impl IAudioEndpointVolumeCallback_Impl for VolumeListener_Impl {
    fn OnNotify(&self, _: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows::core::Result<()> {
        self.0.changed();
        Ok(())
    }
}

/// An output's sessions: one starting, and each one's volume, mute and end.
#[implement(IAudioSessionNotification, IAudioSessionEvents)]
struct SessionListener(Arc<Signals>);

impl IAudioSessionNotification_Impl for SessionListener_Impl {
    fn OnSessionCreated(&self, _: Ref<IAudioSessionControl>) -> windows::core::Result<()> {
        // Read again, which also listens to the new session.
        self.0.rearranged();
        Ok(())
    }
}

impl IAudioSessionEvents_Impl for SessionListener_Impl {
    fn OnDisplayNameChanged(&self, _: &PCWSTR, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnIconPathChanged(&self, _: &PCWSTR, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnSimpleVolumeChanged(&self, _: f32, _: BOOL, _: *const GUID) -> windows::core::Result<()> {
        self.0.changed();
        Ok(())
    }

    fn OnChannelVolumeChanged(
        &self,
        _: u32,
        _: *const f32,
        _: u32,
        _: *const GUID,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnGroupingParamChanged(&self, _: *const GUID, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnStateChanged(&self, state: AudioSessionState) -> windows::core::Result<()> {
        // Sound starting and pausing changes no button; a session that ended does.
        if state == AudioSessionStateExpired {
            self.0.rearranged();
        }
        Ok(())
    }

    fn OnSessionDisconnected(&self, _: AudioSessionDisconnectReason) -> windows::core::Result<()> {
        self.0.rearranged();
        Ok(())
    }
}

/// What is listened to while a reading of the endpoints is kept: the default output's
/// volume, and every connected output's sessions. Dropped with the reading, which stops
/// the listening.
#[derive(Default)]
struct Heard {
    volume: Option<(IAudioEndpointVolume, IAudioEndpointVolumeCallback)>,
    managers: Vec<(IAudioSessionManager2, IAudioSessionNotification)>,
    sessions: Vec<(IAudioSessionControl, IAudioSessionEvents)>,
}

impl Drop for Heard {
    fn drop(&mut self) {
        // SAFETY: each listener is unregistered from the live object it was registered with,
        // on the worker thread that registered it.
        unsafe {
            if let Some((volume, listener)) = &self.volume {
                let _ = volume.UnregisterControlChangeNotify(listener);
            }
            for (session, listener) in &self.sessions {
                let _ = session.UnregisterAudioSessionNotification(listener);
            }
            for (manager, listener) in &self.managers {
                let _ = manager.UnregisterSessionNotification(listener);
            }
        }
    }
}

fn err(context: &str, error: &windows::core::Error) -> AudioError {
    AudioError(format!("{context}: {error}"))
}

fn worker(
    commands: &mpsc::Receiver<Command>,
    changes: Option<Arc<Notify>>,
    ready: &mpsc::Sender<AudioResult<()>>,
) {
    let signals = Arc::new(Signals {
        changes,
        stale: Arc::new(AtomicBool::new(true)),
    });
    let setup = || -> AudioResult<(IMMDeviceEnumerator, IMMNotificationClient)> {
        // SAFETY: COM is initialised on this worker thread before any other call, every call goes
        // through interfaces COM just returned, and the notification client stays registered only
        // while `_client` keeps it alive for the rest of the worker.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(|e| err("CoInitializeEx", &e))?;
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| err("MMDeviceEnumerator", &e))?;
            let client: IMMNotificationClient = ChangeNotifier(signals.clone()).into();
            enumerator
                .RegisterEndpointNotificationCallback(&client)
                .map_err(|e| err("RegisterEndpointNotificationCallback", &e))?;
            Ok((enumerator, client))
        }
    };
    let (enumerator, _client) = match setup() {
        Ok(objects) => {
            let _ = ready.send(Ok(()));
            objects
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let com = ComState {
        enumerator,
        signals,
        endpoints: RefCell::new(None),
    };
    while let Ok(command) = commands.recv() {
        match command {
            Command::Devices(reply) => drop(reply.send(com.devices())),
            Command::DefaultOutput(reply) => drop(reply.send(com.default_output())),
            Command::SetDefaultOutput(id, reply) => drop(reply.send(com.set_default_output(&id))),
            Command::AppVolume(process, reply) => drop(reply.send(com.app_volume(&process))),
            Command::SetAppVolume(process, level, reply) => {
                drop(reply.send(com.set_app_volume(&process, level)));
            }
            Command::AppMuted(process, reply) => drop(reply.send(com.app_muted(&process))),
            Command::SetAppMute(process, muted, reply) => {
                drop(reply.send(com.set_app_mute(&process, muted)));
            }
            Command::Master(reply) => drop(reply.send(com.master())),
            Command::SetMaster(volume, muted, reply) => {
                drop(reply.send(com.set_master(volume, muted)));
            }
            Command::Apps(outputs, reply) => drop(reply.send(com.apps(outputs))),
        }
    }
}

/// The endpoints are read again at least this often, should a change go unreported.
const ENDPOINTS_MAX_AGE: Duration = Duration::from_secs(30);

/// What the device enumerator says. Asking it costs milliseconds (it reads each
/// endpoint's properties), so this is kept until Windows reports a change.
struct Endpoints {
    read: Instant,
    devices: Vec<Device>,
    /// The connected ones, whose sessions `sessions` looks through.
    active: Vec<IMMDevice>,
    /// The default output with its ID, else why there is none.
    default: Result<(String, IMMDevice), AudioError>,
    _heard: Heard,
}

struct ComState {
    enumerator: IMMDeviceEnumerator,
    /// Told by Windows' notices; its `stale` has the endpoints read again.
    signals: Arc<Signals>,
    endpoints: RefCell<Option<Rc<Endpoints>>>,
}

impl ComState {
    /// The endpoints, read again after a change (or `ENDPOINTS_MAX_AGE`).
    fn endpoints(&self) -> AudioResult<Rc<Endpoints>> {
        // Cleared before reading, so a change reported while reading is read next time.
        let changed = self.signals.stale.swap(false, Ordering::AcqRel);
        if !changed
            && let Some(kept) = self.endpoints.borrow().as_ref()
            && kept.read.elapsed() < ENDPOINTS_MAX_AGE
        {
            return Ok(kept.clone());
        }
        self.endpoints.borrow_mut().take();
        let read = Rc::new(self.read_endpoints()?);
        *self.endpoints.borrow_mut() = Some(read.clone());
        Ok(read)
    }

    /// Have the endpoints read again: one kept did not answer (its device went away, or
    /// the audio service restarted).
    fn forget_endpoints(&self) {
        self.signals.stale.store(true, Ordering::Release);
    }

    /// Listen to the default output's volume and to the connected outputs' sessions,
    /// when someone reads the buttons on their notice. What cannot be listened to is
    /// left out: its changes then show when something else has the buttons read.
    fn hear(&self, active: &[IMMDevice], default: Option<&IMMDevice>) -> Heard {
        let mut heard = Heard::default();
        if self.signals.changes.is_none() {
            return heard;
        }
        // SAFETY: the devices are live COM objects of this thread, every call goes through
        // interfaces they returned, and each listener registered is kept in `heard` with the
        // object it is registered with until `Heard::drop` unregisters it.
        unsafe {
            if let Some(volume) = default.and_then(|device| {
                device
                    .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
                    .ok()
            }) {
                let listener: IAudioEndpointVolumeCallback =
                    VolumeListener(self.signals.clone()).into();
                if volume.RegisterControlChangeNotify(&listener).is_ok() {
                    heard.volume = Some((volume, listener));
                }
            }
            for device in active {
                let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                    continue;
                };
                let listener = SessionListener(self.signals.clone());
                let created: IAudioSessionNotification = listener.into();
                if manager.RegisterSessionNotification(&created).is_err() {
                    continue;
                }
                // Windows tells of new sessions only once the sessions were listed.
                let sessions = manager.GetSessionEnumerator();
                let events: Option<IAudioSessionEvents> = created.cast().ok();
                heard.managers.push((manager, created));
                let (Ok(sessions), Some(events)) = (sessions, events) else {
                    continue;
                };
                for i in 0..sessions.GetCount().unwrap_or(0) {
                    if let Ok(session) = sessions.GetSession(i)
                        && session.RegisterAudioSessionNotification(&events).is_ok()
                    {
                        heard.sessions.push((session, events.clone()));
                    }
                }
            }
        }
        heard
    }

    fn read_endpoints(&self) -> AudioResult<Endpoints> {
        // SAFETY: `enumerator` is a live COM object of this thread, and every call goes through
        // interfaces it returned; `device_id` and `friendly_name` only read the device they are
        // given.
        unsafe {
            let collection = self
                .enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE(DEVICE_STATEMASK_ALL))
                .map_err(|e| err("EnumAudioEndpoints", &e))?;
            let count = collection.GetCount().map_err(|e| err("GetCount", &e))?;
            let (mut devices, mut active) = (Vec::new(), Vec::new());
            for index in 0..count {
                let Ok(device) = collection.Item(index) else {
                    continue;
                };
                let Ok(id) = device_id(&device) else {
                    continue;
                };
                let connected = device
                    .GetState()
                    .is_ok_and(|state| state == DEVICE_STATE_ACTIVE);
                let name = friendly_name(&device).unwrap_or_else(|_| id.clone());
                devices.push(Device {
                    id,
                    name,
                    connected,
                });
                if connected {
                    active.push(device);
                }
            }
            let default = match self.enumerator.GetDefaultAudioEndpoint(eRender, eConsole) {
                Ok(device) => Ok((device_id(&device)?, device)),
                Err(e) => Err(err("GetDefaultAudioEndpoint", &e)),
            };
            let heard = self.hear(&active, default.as_ref().ok().map(|(_, device)| device));
            Ok(Endpoints {
                read: Instant::now(),
                devices,
                active,
                default,
                _heard: heard,
            })
        }
    }

    fn devices(&self) -> AudioResult<Vec<Device>> {
        Ok(self.endpoints()?.devices.clone())
    }

    fn default_output(&self) -> AudioResult<Option<String>> {
        Ok(self
            .endpoints()?
            .default
            .as_ref()
            .ok()
            .map(|(id, _)| id.clone()))
    }

    /// The default output, from the endpoints kept.
    fn default_device(&self) -> AudioResult<IMMDevice> {
        let endpoints = self.endpoints()?;
        let (_, device) = endpoints.default.as_ref().map_err(Clone::clone)?;
        Ok(device.clone())
    }

    fn set_default_output(&self, id: &str) -> AudioResult<()> {
        let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives both calls reading it as a
        // PCWSTR, and `policy` is a live COM object created just before its calls.
        unsafe {
            let device = self
                .enumerator
                .GetDevice(PCWSTR(wide.as_ptr()))
                .map_err(|e| err("GetDevice", &e))?;
            if !device
                .GetState()
                .is_ok_and(|state| state == DEVICE_STATE_ACTIVE)
            {
                return Err(AudioError(
                    "device is not connected, so Windows cannot make it the default".into(),
                ));
            }
            let policy: IPolicyConfig =
                CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)
                    .map_err(|e| err("PolicyConfigClient", &e))?;
            for role in [eConsole, eMultimedia, eCommunications] {
                policy
                    .SetDefaultEndpoint(PCWSTR(wide.as_ptr()), role)
                    .ok()
                    .map_err(|e| err("SetDefaultEndpoint", &e))?;
            }
        }
        // Read at once by the press that asked, before Windows reports the change.
        self.forget_endpoints();
        Ok(())
    }

    /// The default output's volume control.
    fn endpoint_volume(&self) -> AudioResult<IAudioEndpointVolume> {
        let device = self.default_device()?;
        // SAFETY: `device` is a live COM object of this thread; `Activate` takes no activation
        // parameters.
        unsafe { device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) }.map_err(|e| {
            self.forget_endpoints();
            err("IAudioEndpointVolume", &e)
        })
    }

    fn master(&self) -> AudioResult<Master> {
        let endpoint = self.endpoint_volume()?;
        // SAFETY: `endpoint` is a live IAudioEndpointVolume returned by `endpoint_volume`.
        unsafe {
            Ok(Master {
                volume: endpoint
                    .GetMasterVolumeLevelScalar()
                    .map_err(|e| err("GetMasterVolumeLevelScalar", &e))?,
                muted: endpoint
                    .GetMute()
                    .map_err(|e| err("GetMute", &e))?
                    .as_bool(),
            })
        }
    }

    fn set_master(&self, volume: Option<f32>, muted: Option<bool>) -> AudioResult<()> {
        let endpoint = self.endpoint_volume()?;
        // SAFETY: `endpoint` is a live IAudioEndpointVolume; a null event context is allowed and
        // means no caller-supplied context GUID.
        unsafe {
            if let Some(volume) = volume {
                endpoint
                    .SetMasterVolumeLevelScalar(volume, std::ptr::null())
                    .map_err(|e| err("SetMasterVolumeLevelScalar", &e))?;
            }
            if let Some(muted) = muted {
                endpoint
                    .SetMute(muted, std::ptr::null())
                    .map_err(|e| err("SetMute", &e))?;
            }
        }
        Ok(())
    }

    /// The apps with a live session on `outputs`, one per program (`app_sounds`).
    fn apps(&self, outputs: Outputs) -> AudioResult<Vec<AppSound>> {
        let devices = match outputs {
            Outputs::Default => vec![self.default_device()?],
            Outputs::Connected => self.endpoints()?.active.clone(),
        };
        let sessions = self.live_sessions(&devices);
        Ok(super::app_sounds(sessions.into_iter().map(
            |(process, volume)| {
                // SAFETY: `volume` is a live ISimpleAudioVolume taken from `live_sessions`.
                let (level, muted) = unsafe {
                    (
                        volume.GetMasterVolume().ok(),
                        volume.GetMute().is_ok_and(windows::core::BOOL::as_bool),
                    )
                };
                (process, level, muted)
            },
        )))
    }

    /// Every live session on `devices`, with its program's file name.
    fn live_sessions(&self, devices: &[IMMDevice]) -> Vec<(String, ISimpleAudioVolume)> {
        let mut found = Vec::new();
        // SAFETY: the devices are live COM objects of this thread and every call goes through
        // interfaces they returned; `process_file_name` only queries the process with the
        // reported id.
        unsafe {
            for device in devices {
                let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                    self.forget_endpoints();
                    continue;
                };
                let Ok(sessions) = manager.GetSessionEnumerator() else {
                    continue;
                };
                for i in 0..sessions.GetCount().unwrap_or(0) {
                    let Ok(control) = sessions.GetSession(i) else {
                        continue;
                    };
                    if control
                        .GetState()
                        .is_ok_and(|state| state == AudioSessionStateExpired)
                    {
                        continue;
                    }
                    let Ok(pid) = control
                        .cast::<IAudioSessionControl2>()
                        .and_then(|c| c.GetProcessId())
                    else {
                        continue;
                    };
                    let Some(process) = (pid != 0).then(|| process_file_name(pid)).flatten() else {
                        continue;
                    };
                    if let Ok(volume) = control.cast::<ISimpleAudioVolume>() {
                        found.push((process, volume));
                    }
                }
            }
        }
        found
    }

    /// Live sessions of `process` across all active render endpoints.
    fn sessions(&self, process: &str) -> AudioResult<Vec<ISimpleAudioVolume>> {
        let endpoints = self.endpoints()?;
        Ok(self
            .live_sessions(&endpoints.active)
            .into_iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case(process))
            .map(|(_, volume)| volume)
            .collect())
    }

    fn app_volume(&self, process: &str) -> AudioResult<Option<f32>> {
        let sessions = self.sessions(process)?;
        Ok(sessions
            .first()
            // SAFETY: `volume` is a live ISimpleAudioVolume taken from `sessions`.
            .and_then(|volume| unsafe { volume.GetMasterVolume().ok() }))
    }

    fn set_app_volume(&self, process: &str, level: f32) -> AudioResult<usize> {
        let sessions = self.sessions(process)?;
        for volume in &sessions {
            // SAFETY: `volume` is a live ISimpleAudioVolume taken from `sessions`; a null event
            // context is allowed.
            unsafe {
                volume
                    .SetMasterVolume(level, std::ptr::null())
                    .map_err(|e| err("SetMasterVolume", &e))?;
            }
        }
        Ok(sessions.len())
    }

    /// Muted while every live session of `process` is, so a session it started later
    /// unmuted (such as on another output) shows as sound.
    fn app_muted(&self, process: &str) -> AudioResult<Option<bool>> {
        let sessions = self.sessions(process)?;
        if sessions.is_empty() {
            return Ok(None);
        }
        // SAFETY: `volume` is a live ISimpleAudioVolume taken from `sessions`.
        Ok(Some(sessions.iter().all(|volume| unsafe {
            volume.GetMute().is_ok_and(windows::core::BOOL::as_bool)
        })))
    }

    fn set_app_mute(&self, process: &str, muted: bool) -> AudioResult<usize> {
        let sessions = self.sessions(process)?;
        for volume in &sessions {
            // SAFETY: `volume` is a live ISimpleAudioVolume taken from `sessions`; a null event
            // context is allowed.
            unsafe {
                volume
                    .SetMute(muted, std::ptr::null())
                    .map_err(|e| err("SetMute", &e))?;
            }
        }
        Ok(sessions.len())
    }
}

fn device_id(device: &IMMDevice) -> AudioResult<String> {
    // SAFETY: `device` is a live IMMDevice. `GetId` returns a CoTaskMemAlloc'd string, which is
    // read once and then freed exactly once.
    unsafe {
        let raw: PWSTR = device.GetId().map_err(|e| err("GetId", &e))?;
        let id = raw
            .to_string()
            .map_err(|e| AudioError(format!("endpoint id: {e}")));
        CoTaskMemFree(Some(raw.0.cast_const().cast()));
        id
    }
}

fn friendly_name(device: &IMMDevice) -> AudioResult<String> {
    // SAFETY: `device` is a live IMMDevice, and its property store is only read.
    unsafe {
        let store = device
            .OpenPropertyStore(STGM_READ)
            .map_err(|e| err("OpenPropertyStore", &e))?;
        let value = store
            .GetValue(&PKEY_Device_FriendlyName)
            .map_err(|e| err("GetValue", &e))?;
        Ok(value.to_string())
    }
}

fn process_file_name(pid: u32) -> Option<String> {
    // SAFETY: the process handle is used only after `OpenProcess` succeeded and is closed exactly
    // once; `buffer` and `len` describe the same writable buffer, and only the `len` characters
    // written are read back.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 1024];
        let mut len = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &raw mut len,
        );
        let _ = CloseHandle(handle);
        ok.ok()?;
        let path = String::from_utf16_lossy(&buffer[..len as usize]);
        Path::new(&path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    }
}
