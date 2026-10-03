# windows-link

A small resident Windows server that exposes PC controls as **buttons** over a local HTTP
API. It is the backend for [windows-deck](https://github.com/miyabisun/windows-deck), a
Stream Deck style touch panel, but any HTTP or WebSocket client can use it.

Each button does one thing:

| type | what a press does | state |
| --- | --- | --- |
| `audio.output_toggle` | switches the Windows default output between two devices | the current default and whether each device is connected |
| `audio.app_volume_toggle` | toggles one application's volume between two levels | the application's current volume, or not running |

It also keeps the mouse cursor where it was when you touch a touch screen (see
[Touch and the mouse cursor](#touch-and-the-mouse-cursor)).

## Requirements

- Windows 10 or 11 (x64)
- To build: Rust 1.96 (selected by `rust-toolchain.toml`) with the MSVC toolchain

## Build and run

```powershell
cargo build --release
.\target\release\windows-link.exe devices   # list output devices and their IDs
.\target\release\windows-link.exe           # run the server
```

The server reads two environment variables:

| variable | default | meaning |
| --- | --- | --- |
| `PORT` | `4730` | TCP port to listen on |
| `LOG_LEVEL` | `info` | `off`, `error`, `warn`, `info`, `debug` or `trace` |
| `WINDOWS_LINK_CONFIG` | `%LOCALAPPDATA%\windows-link\config.yaml` | configuration file |

The release build has no console window. When it is started without a terminal (for
example at logon), logs go to `%LOCALAPPDATA%\windows-link\windows-link.log`.

## Configuration

The configuration is a machine-local YAML file (device IDs differ per PC, so it lives in
LocalAppData rather than the roaming profile); keep it out of version control. If the
file does not exist, the server starts with no buttons.

```yaml
# Aliases for output devices; IDs come from `windows-link devices`.
devices:
  speakers: "{0.0.0.00000000}.{7fcb12f6-05f5-4d23-b3a0-442bbc13e97c}"
  earbuds: "{0.0.0.00000000}.{95e7c4af-0a40-46d0-adf3-9d884ff67c4f}"

buttons:
  - id: output              # letters, digits, '-' and '_'
    label: Output
    type: audio.output_toggle
    devices: [speakers, earbuds]
  - id: game-volume
    label: Game volume
    type: audio.app_volume_toggle
    process: StreetFighter6.exe   # executable file name, case-insensitive
    levels: [0.2, 1.0]
```

- `audio.output_toggle` switches to the second device when the first is the default, and
  to the first device otherwise. Windows cannot make a disconnected device the default,
  so pressing toward one fails with `409 device_unavailable`.
- `audio.app_volume_toggle` sets the lower level when the current volume is above the
  midpoint of the two levels, and the higher level otherwise. It applies to every audio
  session of the process. Windows remembers per-application volume, so the state shows
  the remembered value; nothing is restored automatically. Pressing while the process has
  no audio session fails with `409 not_running`.

Restart the server after editing the file.

## API

| method and path | description |
| --- | --- |
| `GET /healthz` | `ok` |
| `GET /buttons` | all buttons in configuration order: `{id, type, label, state}` |
| `POST /buttons/{id}/press` | press a button; returns `{"button": …}` with the new state |
| `GET /events` | WebSocket: a `{"type":"snapshot","buttons":[…]}` message, then `{"type":"button","button":…}` whenever a button's state changes |

Errors are JSON `{"error": code, "message": …}`: `404 not_found`, `409 device_unavailable`,
`409 not_running`, or `500 audio`.

State changes made outside the server, such as choosing another output in the Windows
sound settings or moving a slider in the volume mixer, are picked up through Windows
device notifications and a one-second refresh and are pushed on `/events`.

## Touch and the mouse cursor

Windows has a single cursor: touching a screen moves it to the touched point, so after
tapping a touch panel your mouse continues from the panel instead of where you left it.
windows-link moves the cursor back to the last real mouse position shortly after a touch
ends. The tap or swipe itself still reaches the application.

- It is on by default for every monitor a touch digitizer is mapped to, and can be
  switched per monitor. Settings are stored in `%LOCALAPPDATA%\windows-link\windows-link.db`
  and keyed by the monitor's device path, so they survive restarts and reconnecting the
  monitor to the same port.
- With it off, the cursor stays at the touched point, as Windows normally does. Turn it
  off for old applications that read the cursor position on their own timing after a tap.
- If touches land on the wrong monitor, map the digitizer to the panel first with
  `MultiDigiMon.exe -touch` (Tablet PC settings → Setup).

| method and path | description |
| --- | --- |
| `GET /touch-monitors` | connected monitors: `{id, name, device_path, gdi_name, primary, touch, keep_cursor}` |
| `PUT /touch-monitors/{id}` | body `{"keep_cursor": true \| false}`; returns the updated monitor (`404` if it is not connected) |

How it works: applications that handle touch natively (browsers, WebView2) get no mouse
input at all, so nothing marks the end of a tap except that the cursor moved. A low-level
mouse hook records where the real mouse is, and every 40 ms the cursor is checked: when it
sits still somewhere the real mouse did not put it (the mouse quiet for 150 ms) on a touch
monitor that keeps the cursor, it is moved back. For applications that get touch as
promoted mouse input, nothing is moved while the finger is down, so their drags are not
interrupted.

## Start at logon

Copy the release build to a per-user location and register a Task Scheduler task that
starts it when you sign in. The trigger is limited to your own logon, so no
administrator rights are needed:

```powershell
$dir = "$env:LOCALAPPDATA\Programs\windows-link"
New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item .\target\release\windows-link.exe $dir

$action = New-ScheduledTaskAction -Execute "$dir\windows-link.exe"
$trigger = New-ScheduledTaskTrigger -AtLogOn -User "$env:USERDOMAIN\$env:USERNAME"
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) `
  -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
Register-ScheduledTask -TaskName windows-link -Action $action -Trigger $trigger `
  -Settings $settings -RunLevel Limited -Force
Start-ScheduledTask -TaskName windows-link    # start it now
```

A new instance waits up to ten seconds for the previous one to release the port, so
restarting the task right after stopping it is fine.

It runs as a normal process in your session, not as a Windows service: per-application
audio sessions are not reachable from the service session.

## Network exposure

The server listens on all interfaces on `PORT` and has no authentication, which suits a
single-user PC on a home network. The only intended client is windows-deck on the same
PC, so do not allow inbound connections:

- When Windows Defender Firewall asks whether to allow `windows-link`, choose **Cancel**.
- Or block it explicitly from an administrator PowerShell:

  ```powershell
  New-NetFirewallRule -DisplayName "windows-link (block inbound)" -Direction Inbound `
    -Program "$env:LOCALAPPDATA\Programs\windows-link\windows-link.exe" -Action Block
  ```

Accepted risk: a web page open in a browser on the same PC can send requests to
`http://127.0.0.1:4730` and press buttons. This was a deliberate choice for a personal
machine; there is no token or origin check.

## Development

```powershell
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --locked
```

The button logic and the HTTP API are tested against an in-memory audio backend; the
Windows Core Audio backend (`src/audio/windows.rs`) is exercised on a real machine.

To check the touch cursor keeper without a finger, inject synthetic touch (coordinates
are physical pixels; `park` moves the mouse like a real mouse would):

```powershell
cargo run --example touch_tap -- park 1280 720
cargo run --example touch_tap -- tap 1378 1950
cargo run --example touch_tap -- swipe 1378 2420 1378 2170
```

The tool prints the cursor position before and 300 ms after the gesture. Point it at a
window on the touch monitor that shows whether the tap arrived (for example a page that
counts clicks in its title).

## License

MIT
