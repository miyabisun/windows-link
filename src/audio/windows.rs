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
    ffi::c_void,
    path::Path,
    sync::{Arc, mpsc},
    thread,
};

use tokio::sync::Notify;
use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Foundation::{CloseHandle, PROPERTYKEY},
        Media::Audio::{
            AudioSessionStateExpired, DEVICE_STATE, DEVICE_STATE_ACTIVE, DEVICE_STATEMASK_ALL,
            EDataFlow, ERole, IAudioSessionControl2, IAudioSessionManager2, IMMDevice,
            IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl,
            ISimpleAudioVolume, MMDeviceEnumerator, eCommunications, eConsole, eMultimedia,
            eRender,
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
        GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, PCWSTR, PWSTR, implement, interface,
    },
};

use super::{Audio, AudioError, AudioResult, Device};

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
}

#[implement(IMMNotificationClient)]
struct ChangeNotifier {
    changes: Arc<Notify>,
}

impl IMMNotificationClient_Impl for ChangeNotifier_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        self.changes.notify_one();
        Ok(())
    }

    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        self.changes.notify_one();
        Ok(())
    }

    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        self.changes.notify_one();
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        _: ERole,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        if flow == eRender {
            self.changes.notify_one();
        }
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
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
    let setup = || -> AudioResult<(IMMDeviceEnumerator, Option<IMMNotificationClient>)> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(|e| err("CoInitializeEx", &e))?;
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| err("MMDeviceEnumerator", &e))?;
            let client = match changes {
                Some(changes) => {
                    let client: IMMNotificationClient = ChangeNotifier { changes }.into();
                    enumerator
                        .RegisterEndpointNotificationCallback(&client)
                        .map_err(|e| err("RegisterEndpointNotificationCallback", &e))?;
                    Some(client)
                }
                None => None,
            };
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
    let state = ComState { enumerator };
    while let Ok(command) = commands.recv() {
        match command {
            Command::Devices(reply) => drop(reply.send(state.devices())),
            Command::DefaultOutput(reply) => drop(reply.send(state.default_output())),
            Command::SetDefaultOutput(id, reply) => drop(reply.send(state.set_default_output(&id))),
            Command::AppVolume(process, reply) => drop(reply.send(state.app_volume(&process))),
            Command::SetAppVolume(process, level, reply) => {
                drop(reply.send(state.set_app_volume(&process, level)));
            }
        }
    }
}

struct ComState {
    enumerator: IMMDeviceEnumerator,
}

impl ComState {
    fn devices(&self) -> AudioResult<Vec<Device>> {
        unsafe {
            let collection = self
                .enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE(DEVICE_STATEMASK_ALL))
                .map_err(|e| err("EnumAudioEndpoints", &e))?;
            let count = collection.GetCount().map_err(|e| err("GetCount", &e))?;
            let mut devices = Vec::new();
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
            }
            Ok(devices)
        }
    }

    fn default_output(&self) -> AudioResult<Option<String>> {
        unsafe {
            match self.enumerator.GetDefaultAudioEndpoint(eRender, eConsole) {
                Ok(device) => device_id(&device).map(Some),
                Err(_) => Ok(None),
            }
        }
    }

    fn set_default_output(&self, id: &str) -> AudioResult<()> {
        let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
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
        Ok(())
    }

    /// Live sessions of `process` across all active render endpoints.
    fn sessions(&self, process: &str) -> AudioResult<Vec<ISimpleAudioVolume>> {
        let mut found = Vec::new();
        unsafe {
            let collection = self
                .enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
                .map_err(|e| err("EnumAudioEndpoints", &e))?;
            let count = collection.GetCount().map_err(|e| err("GetCount", &e))?;
            for index in 0..count {
                let Ok(device) = collection.Item(index) else {
                    continue;
                };
                let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                    continue;
                };
                let Ok(sessions) = manager.GetSessionEnumerator() else {
                    continue;
                };
                let total = sessions.GetCount().unwrap_or(0);
                for i in 0..total {
                    let Ok(control) = sessions.GetSession(i) else {
                        continue;
                    };
                    if control
                        .GetState()
                        .is_ok_and(|state| state == AudioSessionStateExpired)
                    {
                        continue;
                    }
                    let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
                        continue;
                    };
                    let Ok(pid) = control2.GetProcessId() else {
                        continue;
                    };
                    if pid == 0 {
                        continue;
                    }
                    let matches = process_file_name(pid)
                        .is_some_and(|name| name.eq_ignore_ascii_case(process));
                    if matches && let Ok(volume) = control.cast::<ISimpleAudioVolume>() {
                        found.push(volume);
                    }
                }
            }
        }
        Ok(found)
    }

    fn app_volume(&self, process: &str) -> AudioResult<Option<f32>> {
        let sessions = self.sessions(process)?;
        Ok(sessions
            .first()
            .and_then(|volume| unsafe { volume.GetMasterVolume().ok() }))
    }

    fn set_app_volume(&self, process: &str, level: f32) -> AudioResult<usize> {
        let sessions = self.sessions(process)?;
        for volume in &sessions {
            unsafe {
                volume
                    .SetMasterVolume(level, std::ptr::null())
                    .map_err(|e| err("SetMasterVolume", &e))?;
            }
        }
        Ok(sessions.len())
    }
}

unsafe fn device_id(device: &IMMDevice) -> AudioResult<String> {
    unsafe {
        let raw: PWSTR = device.GetId().map_err(|e| err("GetId", &e))?;
        let id = raw
            .to_string()
            .map_err(|e| AudioError(format!("endpoint id: {e}")));
        CoTaskMemFree(Some(raw.0.cast_const().cast()));
        id
    }
}

unsafe fn friendly_name(device: &IMMDevice) -> AudioResult<String> {
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
