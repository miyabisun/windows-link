# windows-link

A small resident Windows server that exposes PC controls as **buttons** over a local HTTP
API. It is the backend for [windows-deck](https://github.com/miyabisun/windows-deck), a
Stream Deck style touch panel, but any HTTP or WebSocket client can use it.

Each button does one thing:

| type | what a press does | state |
| --- | --- | --- |
| `audio.output_toggle` | switches the Windows default output between two devices | the current default and whether each device is connected |
| `audio.app_volume_toggle` | toggles one application's volume between two levels | the application's current volume, or not running |
| `discord.voice` | joins one Discord voice channel, or leaves it when you are in it | whether you are in that channel, or why Discord cannot be reached |

It also keeps the mouse cursor where it was when you touch a touch screen (see
[Touch and the mouse cursor](#touch-and-the-mouse-cursor)), lets a panel follow and switch
Windows virtual desktops, and updates itself from GitHub Releases (see [Updates](#updates)).

## Requirements

- Windows 10 or 11 (x64)
- To build: Rust 1.96 (selected by `rust-toolchain.toml`) with the MSVC toolchain

## Install

Download `windows-link-x86_64-pc-windows-msvc.exe` from the
[latest release](https://github.com/miyabisun/windows-link/releases/latest) and save it as
`%LOCALAPPDATA%\Programs\windows-link\windows-link.exe`, then register it to
[start at logon](#start-at-logon). From PowerShell:

```powershell
$dir = "$env:LOCALAPPDATA\Programs\windows-link"
New-Item -ItemType Directory -Force $dir | Out-Null
Invoke-WebRequest -OutFile "$dir\windows-link.exe" `
  https://github.com/miyabisun/windows-link/releases/latest/download/windows-link-x86_64-pc-windows-msvc.exe
& "$dir\windows-link.exe" devices   # list output devices and their IDs
```

Only a copy in that folder [updates itself](#updates). Each release also has a `.sha256`
file to check the download against.

## Build and run

```powershell
cargo build --release
.\target\release\windows-link.exe devices   # list output devices and their IDs
.\target\release\windows-link.exe           # run the server
```

The server reads these environment variables:

| variable | default | meaning |
| --- | --- | --- |
| `PORT` | `4730` | TCP port to listen on |
| `LOG_LEVEL` | `info` | `off`, `error`, `warn`, `info`, `debug` or `trace` |
| `WINDOWS_LINK_CONFIG` | `%LOCALAPPDATA%\windows-link\config.yaml` | configuration file |
| `WINDOWS_LINK_UPDATE_URL` | this repository's latest release in the GitHub API | where [updates](#updates) come from |
| `WINDOWS_LINK_SECRETS` | `%LOCALAPPDATA%\windows-link\secrets.yaml` | credentials for outside services ([Discord](#discord)) |

The release build has no console window. When it is started without a terminal (for
example at logon), logs go to `%LOCALAPPDATA%\windows-link\windows-link.log`.

## Configuration

The configuration is machine-local YAML in `%LOCALAPPDATA%\windows-link\` (device IDs
differ per PC, so it does not roam); keep it out of version control. Without any file the
server starts with no buttons.

`config.yaml` holds the device aliases and the **shared** buttons, which every virtual
desktop's tab shows:

```yaml
# Aliases for output devices; IDs come from `windows-link devices`.
devices:
  speakers: "{0.0.0.00000000}.{7fcb12f6-05f5-4d23-b3a0-442bbc13e97c}"
  earbuds: "{0.0.0.00000000}.{95e7c4af-0a40-46d0-adf3-9d884ff67c4f}"

buttons:
  - id: output              # letters, digits, '-' and '_'; unique across all files
    label: Output
    type: audio.output_toggle
    devices: [speakers, earbuds]
    except: [dev]           # optional: desktops whose tab leaves it out
```

Each virtual desktop can have its own buttons in `desktops/<desktop name>.yaml`, which
only that desktop's tab shows. The file is matched to the desktop by name (ignoring case),
so renaming or removing the desktop in Windows hides its buttons; create a desktop with the
same name to bring them back (a panel can do this for you, see `POST /desktops`). For
example `desktops/SF6.yaml`:

```yaml
buttons:
  - id: game-volume
    label: Game volume
    type: audio.app_volume_toggle
    process: StreetFighter6.exe   # executable file name, case-insensitive
    levels: [0.2, 1.0]
  - id: sf6
    label: Street Fighter 6
    type: steam.game
    app_id: 1364780                # from the store page URL
    process: StreetFighter6.exe
    icon: C:\Program Files (x86)\Steam\steamapps\common\Street Fighter 6\StreetFighter6.exe
  - id: vc-friends
    label: Friends VC
    type: discord.voice
    channel_id: 1533091153086251103   # from `windows-link discord-channels`
```

and `desktops/Blue Archive.yaml`:

```yaml
buttons:
  - id: blue-archive
    label: Blue Archive
    type: app.launch
    target: C:\YostarGames\BlueArchive_JP_Gamelauncher\BlueArchive_JP_Gamelauncher.exe
```

- `audio.output_toggle` switches to the second device when the first is the default, and
  to the first device otherwise. Windows cannot make a disconnected device the default,
  so pressing toward one fails with `409 device_unavailable`.
- `audio.app_volume_toggle` sets the lower level when the current volume is above the
  midpoint of the two levels, and the higher level otherwise. It applies to every audio
  session of the process. Windows remembers per-application volume, so the state shows
  the remembered value; nothing is restored automatically. Pressing while the process has
  no audio session fails with `409 not_running`.
- `discord.voice` joins its channel (moving you out of any other voice channel), or
  leaves it when you are already there. Add one button per channel. See
  [Discord](#discord) for the one-time setup.
- `app.launch` opens `target` (an exe, a shortcut, a document or a URL) the way
  double-clicking it in Explorer does, with optional `args`. A program starts in its own
  folder.
- `steam.game` starts the game through Steam (`steam://rungameid/<app_id>`) and shows
  `running` while `process` runs; pressing it then asks the game's windows to close, like
  their close button (`409 no_window` while it has none yet).
- `icon` (optional, any button type) is a file whose Windows icon the button shows: an exe,
  a shortcut or an image. `app.launch` buttons show their target's icon without it.

Restart the server after editing the file.

## Discord

The `discord.voice` buttons drive the Discord desktop app on the same PC through its local
RPC, signed in as a Discord application of your own:

1. In the [Developer Portal](https://discord.com/developers/applications), signed in
   with the account you use in the Discord app, create an application. Under OAuth2, add
   the redirect `http://127.0.0.1`. (Until Discord approves an application, only its
   owner and its testers can use RPC with it.)
2. Put its Client ID and Client Secret in `%LOCALAPPDATA%\windows-link\secrets.yaml`
   (`WINDOWS_LINK_SECRETS` overrides the path), which holds credentials per service:

   ```yaml
   discord:
     client_id: 1234567890123456789
     client_secret: your-client-secret
   ```

3. With Discord running, run `windows-link discord-channels`. The first time, Discord
   asks you to approve windows-link; then it lists the voice channels you can join:

   ```text
   My server
     1556097803241918564  general-voice
   ```

4. Add a `discord.voice` button per channel you want, using the IDs from the list, and
   restart the server.

windows-link keeps the approval in `%LOCALAPPDATA%\windows-link\discord-token.json` and
renews it before it expires. It connects to Discord while Discord runs and reconnects
after Discord restarts; until then the buttons report `available: false` with the reason.
Pressing one while Discord is not running starts Discord minimized and joins once it is
ready (up to a minute). Joining does not bring Discord's window to the front.

Press errors: `409 discord_unavailable` (Discord did not start or cannot be used, with the
reason), `409 discord_rejected` (the approval was turned down), `500 discord`.

## API

| method and path | description |
| --- | --- |
| `GET /healthz` | `ok` |
| `GET /buttons` | all buttons, shared ones first: `{id, type, label, desktop, except, icon, state}` (`desktop` is the desktop name for a desktop file's button, `null` for shared ones) |
| `POST /buttons/{id}/press` | press a button; returns `{"button": …}` with the new state |
| `GET /buttons/{id}/icon` | the button's icon as a 256 px PNG, when `icon` is true |
| `GET /desktops` | virtual desktops, and the desktop files without a desktop: `{"desktops":[{id, name, index, current}], "unmatched":[name], "error": null}` |
| `POST /desktops` | `{"name": …}`: create a desktop with this name and switch to it (`201`, the new `GET /desktops` body; `409 exists`) |
| `POST /desktops/{id}/switch` | switch to a virtual desktop; returns the new `GET /desktops` body |
| `POST /windows/{hwnd}/pin` | show a window on every virtual desktop (`hwnd` in decimal or `0x` hex) |
| `GET /touch-monitors`, `PUT /touch-monitors/{id}` | see [Touch and the mouse cursor](#touch-and-the-mouse-cursor) |
| `POST /power/sleep` | put the PC to sleep (answers `202` first) |
| `GET /version` | `{"version": "0.1.0"}` |
| `POST /update/check` | check for an update now (see [Updates](#updates)) |
| `GET /events` | WebSocket: one `snapshot` message, then `button` and `desktops` messages as things change (below) |

Errors are JSON `{"error": code, "message": …}`: `404 not_found`, `409 device_unavailable`,
`409 not_running`, `500 audio`, `409 discord_unavailable`, `409 discord_rejected`,
`500 discord`, `409 no_window`, `500 launch`, `400 invalid_hwnd`, `400 invalid_name`,
`409 exists`, `503 desktops`, or `502 update`.

`/events` messages:

- `{"type":"snapshot","buttons":[…],"desktops":[…],"unmatched":[…],"desktops_error":null}` on connect
- `{"type":"button","button":{…}}` when a button's state changes
- `{"type":"desktops","reason":…,"desktops":[…],"unmatched":[…],"error":null}` when the current desktop
  changes (`changed`), or a desktop is `created`, `removed`, `renamed` or `moved`, and
  `reconnected` after Explorer restarts. Switching with `Win + Ctrl + →` or the
  API both produce `changed`.

State changes made outside the server, such as choosing another output in the Windows
sound settings or moving a slider in the volume mixer, are picked up through Windows
device notifications and a one-second refresh and are pushed on `/events`.

## Virtual desktops

Windows switches the virtual desktop on every monitor at once, so a control panel on a
touch monitor would disappear on desktops it was not opened on. Instead the panel pins its
own window with `POST /windows/{hwnd}/pin` so it stays on every desktop, and shows the
desktops as tabs: tapping a tab calls `POST /desktops/{id}/switch`, and switching any other
way updates the tab through `/events`. Unnamed desktops are listed as `デスクトップ N`, the
same as Windows.

Windows has no public API for virtual desktops. windows-link uses the undocumented COM
interfaces through [winvd](https://crates.io/crates/winvd) (Windows 11 24H2 or later). If
they do not work on this build, only the virtual desktop features are disabled: `GET
/desktops` returns an empty list with the reason in `error`, the switch and pin endpoints
return `503`, and buttons, volume and touch keep working. When Explorer restarts, the
server notices within a few seconds, subscribes again and sends `reconnected`.

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

Register a Task Scheduler task that starts the [installed](#install) exe when you sign
in. The trigger is limited to your own logon, so no administrator rights are needed:

```powershell
$dir = "$env:LOCALAPPDATA\Programs\windows-link"
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

## Updates

The installed server checks the latest GitHub release when it starts and every hour.
When the release is newer than itself it updates without asking:

1. downloads the exe and its `.sha256` file over HTTPS and checks the SHA-256,
2. saves it as `windows-link.exe.new` and checks that it runs and reports the release's
   version,
3. renames the running exe to `windows-link.exe.old` (Windows allows renaming a running
   exe, not replacing it) and moves the new one into its place,
4. starts the new exe with the same arguments and exits. The new process waits for the
   port, then deletes `windows-link.exe.old`.

Any failure leaves the running version as it is and is logged; the next check tries
again. A panel such as windows-deck reconnects by itself, so an update shows as a moment
of "disconnected". Debug builds and copies run from anywhere other than
`%LOCALAPPDATA%\Programs\windows-link` never update.

`POST /update/check` checks immediately and answers with one of:

- `{"result": "up_to_date", "current": "0.1.0", "latest": "0.1.0"}`
- `{"result": "restarting", "current": "0.1.0", "latest": "0.1.1"}`: the server restarts
  into the new version right after answering
- `{"result": "skipped", "current": "0.1.0", "reason": "development build"}`
- `502 {"error": "update", "message": …}` when the release could not be fetched or
  checked

Releases are built by GitHub Actions when a `vX.Y.Z` tag is pushed
(`.github/workflows/release.yml`); the tag must match the version in `Cargo.toml`.

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
machine; there is no token, and requests are not rejected by origin.

Cross-origin reads (CORS) are allowed only for the windows-deck window
(`http://tauri.localhost`, `https://tauri.localhost`, `tauri://localhost`) and for pages
served from `http://localhost` or `http://127.0.0.1` on any port, such as the panel's
development server. Other web sites cannot read the answers.

## Development

```powershell
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --locked
```

The button logic and the HTTP API are tested against an in-memory audio backend; the
Windows Core Audio backend (`src/audio/windows.rs`) is exercised on a real machine.

To try an update without publishing a release, serve a GitHub-style release JSON from
this machine and point `WINDOWS_LINK_UPDATE_URL` at it (plain HTTP is accepted only for
`127.0.0.1`, `localhost` and `[::1]`; every other URL must be HTTPS).

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
