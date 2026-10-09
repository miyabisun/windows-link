# windows-link

A small resident Windows server that exposes PC controls as **buttons** over a local HTTP
API. It is the backend for [windows-deck](https://github.com/miyabisun/windows-deck), a
Stream Deck style touch panel, but any HTTP or WebSocket client can use it.

Each button does one thing:

| type | what a press does | state |
| --- | --- | --- |
| `audio.output_toggle` | switches the Windows default output between two devices | the current default and whether each device is connected |
| `audio.app_volume_toggle` | toggles one application's volume between two levels | the application's current volume, or not running |
| `audio.app_mute_toggle` | mutes one application, or unmutes it, as its speaker in Windows' volume mixer does | whether it is muted, or not running |
| `audio.mute_toggle` | mutes the default output, or unmutes it | whether it is muted, and its volume |
| `audio.mixer` | nothing: a panel opens the mixer (`GET /audio/mixer`) | the default output's volume and whether it is muted |
| `discord.server` | brings Discord to the front showing one server, starting Discord if needed | whether Discord runs; the button shows the server's icon |
| `app.launch` | opens a program, shortcut, URL or Store app, or brings it to the front while it runs | whether it runs |
| `steam.game` | starts a Steam game, or closes it while it runs | whether it runs |
| `steam.library` | nothing: a panel opens the Steam library to search, start and pin games ([Steam library](#steam-library)) | the games pinned to the button, and whether pictures are store art (`cover`) or shown whole (`whole`) |
| `dlsite.library` | nothing: a panel opens the DLsite games in a folder the same way ([DLsite library](#dlsite-library)) | the games pinned to the button, and whether pictures are store art (`cover`) or shown whole (`whole`) |
| `fanza.library` | nothing: a panel opens the FANZA games bought the same way ([FANZA library](#fanza-library)) | the same, and `sign_in: "fanza"` while FANZA needs signing in through the panel |

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
| `WINDOWS_LINK_SECRETS` | `%LOCALAPPDATA%\windows-link\secrets.yaml` | credentials for outside services ([Discord](#discord), [Steam](#steam-library), [DLsite](#dlsite-library)) |

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
    device_icons: [speaker, headphones]   # optional: what each is, for the panel's icon
    except: [dev]           # optional: desktops whose tab leaves it out
  - id: mute
    label: Mute
    type: audio.mute_toggle
  - id: mixer
    label: Mixer
    type: audio.mixer
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
  - id: game-mute
    label: Game sound
    type: audio.app_mute_toggle
    process: StreetFighter6.exe
  - id: sf6
    label: Street Fighter 6
    type: steam.game
    app_id: 1364780                # from the store page URL
    process: StreetFighter6.exe
    icon: C:\Program Files (x86)\Steam\steamapps\common\Street Fighter 6\StreetFighter6.exe
  - id: friends
    label: Friends
    type: discord.server
    guild_id: 1533091152293789877   # from `windows-link discord-servers`
```

and `desktops/dev.yaml`:

```yaml
buttons:
  - id: terminal
    label: Terminal
    type: app.launch
    target: shell:AppsFolder\Microsoft.WindowsTerminal_8wekyb3d8bbwe!App   # a Store app
    process: WindowsTerminal.exe   # while it runs, a press brings it to the front
  - id: terminal-admin
    label: Terminal (admin)
    type: app.launch
    target: wt.exe
    admin: true
    icon: shell:AppsFolder\Microsoft.WindowsTerminal_8wekyb3d8bbwe!App
  - id: blue-archive
    label: Blue Archive
    type: app.launch
    target: C:\YostarGames\BlueArchive_JP_Gamelauncher\BlueArchive_JP_Gamelauncher.exe
    process: BlueArchive.exe       # the game the launcher starts
```

- `audio.output_toggle` switches to the second device when the first is the default, and
  to the first device otherwise. Windows cannot make a disconnected device the default,
  so pressing toward one fails with `409 device_unavailable`.
- `audio.app_volume_toggle` sets the lower level when the current volume is above the
  midpoint of the two levels, and the higher level otherwise. It applies to every audio
  session of the process. Windows remembers per-application volume, so the state shows
  the remembered value; nothing is restored automatically. Pressing while the process has
  no audio session fails with `409 not_running`.
- `audio.app_mute_toggle` mutes every audio session of the process, or unmutes them,
  keeping its volume (the same mute as `PUT /audio/apps/{process}` with `muted`). A session
  the process starts later, such as on another output device, may begin unmuted; the state
  shows it. Pressing while the process has no audio session fails with `409 not_running`.
- `audio.mixer` is opened by the panel, not pressed: `GET /audio/mixer` gives the default
  output's volume and mute and the applications with sound on it (one per program, named
  by its file name without `.exe`), and `PUT /audio/master` and `PUT /audio/apps/{process}`
  change them. Moving the volume of a muted output or application unmutes it, as Windows'
  own slider does.
- `discord.server` opens `discord://-/channels/<guild_id>` through Discord's own launcher
  (`%LOCALAPPDATA%\Discord\Update.exe`; the `discord://` registration names a version
  folder Discord removes when it updates): Discord starts if needed, shows the server, and
  its window moves to the virtual desktop on screen and comes to the front. Its state is
  `launch` (running while Discord runs). The button shows the server's icon once
  windows-link has read it from Discord; see [Discord](#discord) for the one-time setup
  that needs.
- `app.launch` opens `target` (an exe, a shortcut, a document, a URL, or a Store app as
  `shell:AppsFolder\<app ID>`; PowerShell's `Get-StartApps` lists the IDs) the way
  double-clicking it in Explorer does, with optional `args`. A program starts in its own
  folder. With `process`, the state shows `running` while that exe runs, and a press then
  brings its window to the virtual desktop on screen and to the front (restoring it when
  minimized) instead of opening another one. `admin: true` opens it as administrator, after Windows asks for consent.
- `steam.game` starts the game through Steam (`steam://rungameid/<app_id>`) and brings its
  window to the virtual desktop on screen and to the front as soon as it appears (started
  this way, a game would otherwise open behind the window that had the focus and may not
  go full screen). It shows `running`
  while `process` runs; pressing it then asks the game's windows to close, like their close
  button (`409 no_window` while it has none yet).
- `icon` (optional, any button type) is a file whose Windows icon the button shows: an exe,
  a shortcut, an image or a `shell:AppsFolder\…` app; or a picture's `https://` URL, which
  the panel loads itself (`GET …/icon` redirects there). `app.launch` buttons show their
  target's icon without it, and `dlsite.library` and `fanza.library` buttons their shop's
  favicon.
- `device_icons` (optional, `audio.output_toggle`) says what each of the two devices is,
  `speaker` or `headphones`, in the same order: each option in the state carries it as
  `icon`, so the panel can show the current one. Windows calls Bluetooth earbuds
  speakers too, so it is not read from Windows.

Restart the server after editing the file.

## Steam library

A `steam.library` button gives a panel your Steam library: every game you own, whether it
is installed, and its labels: Steam's favorites and hidden collections and the collections
you made (dynamic collections are left out). For example in `desktops/ゲーム.yaml`:

```yaml
buttons:
  - id: steam-library
    label: Steam
    type: steam.library
    hide: [非表示]         # collections whose games are listed only while selected
    icon: C:\Program Files (x86)\Steam\steam.exe
```

The games you own come from the Steam Web API, for the account that signed in to Steam on
this PC last. Get a key at <https://steamcommunity.com/dev/apikey> (any domain name will
do) and put it in `secrets.yaml`:

```yaml
steam:
  api_key: 0123456789ABCDEF0123456789ABCDEF
```

windows-link fetches the list at start and every 30 minutes, so a new purchase shows up
within half an hour (or after a restart). Without a key, or while the API cannot be
reached, the library lists the installed games and says why in `partial`. Install state
is read from the Steam folder (found through the registry) on every listing, so it is
always current.

Games are named as the Steam client shows them, in the language Steam is set to (for
example `45番電車` rather than `Train45`), asked of Steam on every listing while it runs and
kept from the last answer while it does not. When that name differs from the one in the
game's files by more than marks such as ™, the latter is the game's `detail`, so either
name finds it.

Starting a game opens `steam://rungameid/<app ID>` and brings the game's window to the front
once a program from its install folder shows one; while such a window exists, starting
brings it to the front instead. A game that is not installed opens Steam's install dialog
(`steam://install/<app ID>`). Pictures are Steam's own 460×215 headers: the newest one in
Steam's library cache, which follows the Steam client's language, or the store's.

Pinned games are kept per button in `windows-link.db`, in the order they were pinned.

### Labels

A panel can make, rename and delete collections and put games in them or take them out
(Steam's favorites and hidden can hold games but cannot be renamed or deleted). Steam has
no API for this, so windows-link asks the Steam client itself, as if you did it in Steam's
window, and Steam saves and syncs the change. For that, Steam must accept remote control:

```powershell
New-Item -ItemType File -Force "${env:ProgramFiles(x86)}\Steam\.cef-enable-remote-debugging"
```

then restart Steam. Steam then serves the Chrome DevTools protocol on `127.0.0.1:8080`,
where windows-link calls the collection store of Steam's library page, and its downloads
and installs for [updating the library](#updating-the-library). Accepted risk: any
program on this PC can control the Steam client through that port; remove the file and
restart Steam to close it.

While Steam answers there, the labels come from it, so changes show at once. When it is
not running or does not accept remote control, the labels are read from Steam's
collection file and `labels_locked` says why they cannot be changed. Names must be new
(ignoring case): Steam would replace a collection with the same name.

### Updating the library

`POST /buttons/{id}/library/update` asks the Steam client, through the same remote
control, for every game the account has and how each is on this PC (with or without the
Web API key). Leaving out the games in the button's `hide` collections and apps that are
not games (dedicated servers, SDKs, soundtracks), it goes on with each update or
download Steam has not run (queued, required, paused or failed), as Steam's own library
does, and opens Steam's install screen for the games that are not installed. That screen
shows on Steam's window: pick the folder there and accept any EULA to start the installs.
The answer counts both: `{"updates": 3, "installs": 24}`.

## DLsite library

A `dlsite.library` button gives a panel the DLsite games in the folders DLsiteNest makes,
`<maker>\<title>` under `D:\DLsiteNest\Game`, leaving out the `<title>.bak` copies
DLsiteNest keeps of updated games; DLsiteNest itself is not needed. With a DLsite account,
windows-link also downloads the games bought but not there yet and keeps every game up to
date ([Downloads and updates](#downloads-and-updates)). For
example in `desktops/アダルト.yaml`:

```yaml
buttons:
  - id: dlsite
    label: DLsite
    type: dlsite.library
    root: D:\DLsiteNest\Game   # optional; this is the default
    hide: [非表示]
```

The listing answers from `windows-link.db` at once, even while the disk is slow. It holds
the games found when the folder was last read. The folder is read again in the
background after each listing and each download. A game added or removed by hand shows
the second time the list is opened. Starting a game reads its own folder again, so a
renamed program still starts. Each game lists its maker as `detail` and comes
recently started first, then recently added. Its picture is the work's art on DLsite
(`image` in the listing), or its program's icon until the work is known; the button's
state says `"pictures": "whole"`, as both are shown whole.

Some games ask for a license key (serial number) when they first start. `GET …/keys` reads it
from DLsite each time it is asked (with the account in `secrets.yaml`), so the keys are kept
nowhere on this PC and never logged; the panel shows them with a copy button.

- **Programs**: the `.exe` files in the game's folder, or else in the folders right below
  it, leaving out helpers such as crash reporters, uninstallers and runtime installers.
  With one, or with one that is not a tool (settings, setup, patcher, launcher), starting
  runs it in its own folder. Otherwise starting answers `409 choose_program` until one is
  chosen with `PUT …/program`; the choice is remembered. A game without any program
  answers `409 no_program`. Its window is brought to the front like a Steam game's.
- **Labels**: お気に入り and 非表示 always exist and cannot be renamed or deleted; more can be
  made, renamed and deleted like Steam's. They, the chosen programs and the start times are
  kept in `windows-link.db` by the game's ID: its DLsite work ID once known, so they stay
  when its folder is renamed, or else an ID made from its maker and title folders.
- **Which work a game is**: at start and every 6 hours windows-link works out each game's
  DLsite work (such as `RJ01464588`) and keeps it with the game's folder in the
  `dlsite_games` table of `windows-link.db` (`work_id`, `path` in lower case, `image`),
  one row per work, so it stays after DLsiteNest is uninstalled. Each work goes to one
  folder, found in this order:
  1. the folder's row in `dlsite_games` (a row added by hand names a work it cannot find);
  2. DLsiteNest's own records (`%APPDATA%\DLsiteNest`), which name each work's folder;
  3. when `secrets.yaml` has a DLsite account, the purchase with the same title (the only
     one, or the one by the same maker), first as written and then without sale text such
     as `【30%OFF!!】` or `✅…特典✅`;
  4. the maker's only purchase left, when the game is the maker's only one left.

  Pictures come from the purchases, or from DLsite's public product information for works
  known only through DLsiteNest. The account is also what downloading and updating games
  uses. Put it in `secrets.yaml` (a login ID and password; signing in through Google
  or other services is not supported) and restart windows-link:

  ```yaml
  dlsite:
    login_id: you@example.com
    password: your-password
  ```

  The log says how many purchases were read and games identified, which are not, or why
  signing in failed (such as DLsite asking for a CAPTCHA, which windows-link does not answer).

### Downloads and updates

After working out the games (at start, every 6 hours and when the panel asks for a
[library update](#api)), windows-link downloads, one at a time, the purchased games that are not in the folder yet, then the updates. Nothing pops
up; the panel's list and the log show how it goes.

- **Which games**: games (DLsite's game work types) that run on Windows. The AI-translated
  game data in other languages that comes with some purchases (`…ゲームデータ（AI翻訳）`),
  phone-only games and other works (voice, manga, video) are left out.
- **In the list**: a game not here yet is listed first by its work ID, with its maker and
  picture, `installed: false` and a `status` such as `ダウンロード待ち`, `ダウンロード中 40%`,
  `展開中` or why it failed; an updating game has a `status` too. Starting one not here yet
  answers `409 not_downloaded` with its status.
- **Where**: a new game goes to `<root>\<maker>\<title>`, named as DLsiteNest names folders
  (characters Windows does not allow become `_`, dots are left out), with its work ID after
  the title when that folder is taken. DLsite's archives wrap the game in a folder named
  after the work; that folder is left out. The download waits in `<root>\.windows-link\<ID>`
  (not listed) until it is unpacked and in place, so a stopped download goes on next time.
  ZIPs are unpacked with Windows' own `tar.exe` (names without the UTF-8 mark read as
  UTF-8, as DLsite writes them, else as CP932); RARs, which DLsite uses for works split
  into parts, with UnRAR.
- **Updates**: a game's version is DLsite's `upgrade_date` (else its release date), kept in
  `dlsite_games.version`. A folder windows-link filled is updated when DLsite has a newer
  version; a folder DLsiteNest filled is taken as current when it changed after the latest
  version came out, and updated otherwise. An update is laid over the folder: new files
  are added, files are replaced except saves (anything under a folder or named with
  `save` in it), and files the update lacks stay.
- **Failures** (DLsite refusing, a full disk, an archive that does not unpack) are logged,
  shown as the game's `status`, and tried again in the next round.

## FANZA library

A `fanza.library` button gives a panel the PC games bought on FANZA (DMM's adult shop; its
library also lists games from DMM's all-ages shop), run the same way as the [DLsite
library](#dlsite-library): windows-link reads the purchases at start, every 6 hours,
right after signing in and when the panel asks for a library update, downloads the games not there yet into `<brand>\<title>` under
`D:\fanza`, and lists them with their package pictures. For example in
`desktops/コミック.yaml`:

```yaml
buttons:
  - id: fanza
    label: FANZA
    type: fanza.library
    root: D:\fanza   # optional; this is the default
    hide: [非表示]
```

- **Signing in**: DMM checks its login page for bots, so windows-link never signs in with a
  password. While it has no sign-in, or DMM no longer takes it, the button's state and the
  listing say `sign_in: "fanza"`, and the panel offers a login window where you sign in
  yourself (it shows on the primary monitor). The panel hands that window's DMM cookies to
  windows-link (`PUT /fanza/session`), which keeps them in
  `%LOCALAPPDATA%\windows-link\fanza-session.json` and from then on renews DMM's session
  itself, saving every cookie DMM replaces. The window is a private one of its own, so
  signing in there does not touch a browser's sign-in, and a browser's sign-in is never
  read.
- **Downloads**: one game at a time from FANZA's library (`/ajax/v1/library`), going on
  where a stopped download left off. A ZIP, a self-extracting ZIP or RAR, and a split RAR
  (`<name>setup.exe` with `<name>setup_.r00` on, or `.part1.exe` with `.part<N>.rar`) are
  unpacked. Many games need FANZA's `ソフト電池` runtime, which asks you to sign in when
  such a game first starts. Sets of several works are not read yet.
- **Serial codes**: FANZA sends a game's serial code by e-mail, so `GET …/keys` has none.
- **Labels, programs and pins** work as in the DLsite library, kept in `fanza_*` tables of
  `windows-link.db`; a game's ID is its FANZA product ID (such as `alice_0024`). A folder
  without a record is matched to the purchase of the same brand and title.

## Discord

A `discord.server` button opens its server without any setup. To show the server's icon,
windows-link reads it from the Discord desktop app on the same PC through its local RPC,
signed in as a Discord application of your own:

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

3. With Discord running, run `windows-link discord-servers`. The first time, Discord
   asks you to approve windows-link; then it lists your servers with their IDs:

   ```text
   1533091152293789877  My server
   ```

4. Add a `discord.server` button per server you want, using the IDs from the list, and
   restart the server.

windows-link keeps the approval in `%LOCALAPPDATA%\windows-link\discord-token.json` and
renews it before it expires. It connects to Discord while Discord runs, reads the
servers' icons then and every hour, and reconnects after Discord restarts; the icons read
last stay while Discord is closed. `GET /buttons/{id}/icon` redirects to the icon on
Discord's CDN.

## API

| method and path | description |
| --- | --- |
| `GET /healthz` | `ok` |
| `GET /buttons` | all buttons, shared ones first: `{id, type, label, desktop, except, icon, state}` (`desktop` is the desktop name for a desktop file's button, `null` for shared ones) |
| `POST /buttons/{id}/press` | press a button; returns `{"button": …}` with the new state |
| `GET /buttons/{id}/icon` | the button's icon as a 256 px PNG, or a redirect to it on the web, when `icon` is true |
| `GET /buttons/{id}/library` | a library button's games: `{"items":[{id, name, detail, choosable, image, installed, labels, pinned}], "labels":[{id, name, editable}], "hide":[label id], "partial": null, "labels_locked": null, "sign_in": null}` (`labels` of an item are label IDs; `detail` is a second line such as the maker; `choosable` says the game has programs to choose from; `image` is its picture on the web when known, else ask `…/image`; `partial` says why only the installed games are listed, `labels_locked` why labels cannot be changed now, `sign_in` the shop to sign in to through the panel) |
| `POST /buttons/{id}/library/update` | bring a library button's games up to date, leaving out its `hide` labels: Steam updates its games and shows its install screen for those not installed, answering `{"updates": n, "installs": n}` ([Updating the library](#updating-the-library)); DLsite and FANZA start their round of downloads and updates now, or right after the one under way (`202 {"round": true}`; the listing shows how each game goes). `409 sign_in` while FANZA needs signing in, `409 update_unavailable` with the reason (Steam not running or not accepting remote control, no DLsite account) |
| `POST /buttons/{id}/library/{item}/start` | start a game, or bring it to the front when it runs (`204`) |
| `GET /buttons/{id}/library/{item}/image` | the game's picture (JPEG), or a redirect to it on the web |
| `GET /audio/mixer` | the default output's volume and mute and the applications with sound on it: `{"master": {volume, muted}, "apps": [{process, name, volume, muted}]}` (volumes 0–1) |
| `PUT /audio/master` | `{"volume": …}` and/or `{"muted": …}` (a volume alone also unmutes); returns the mixer (`400 invalid_volume` outside 0–1) |
| `PUT /audio/apps/{process}` | `{"volume": …}`, `{"muted": …}` or both for every session of the program (a volume alone unmutes); returns the mixer (`404` when it has no sound now, `400 invalid_change` with neither) |
| `PUT /buttons/{id}/pins/{item}`, `DELETE …` | pin a game to the button, or take it off; returns `{"button": …}` |
| `POST /buttons/{id}/library/{item}/folder` | show an installed game's folder in Explorer (`204`; `404` when not installed) |
| `GET /buttons/{id}/library/{item}/programs` | a game's programs to choose from: `{"candidates": ["Game.exe", …], "chosen": null}` (`404` when it has no choice) |
| `PUT /buttons/{id}/library/{item}/program` | `{"program": …}`: remember which program starts the game (`204`) |
| `GET /buttons/{id}/library/{item}/keys` | a game's license keys from its store: `{"keys": [{label, value}]}` (empty when it has none; `404` when the library does not know them; `409 keys_unavailable` with the reason when the store cannot be asked). A library button's state says `"license_keys": true` when its games have them (DLsite) |
| `POST /buttons/{id}/labels` | `{"name": …}`: make a label; `201 {"label": {id, name, editable}}` |
| `PATCH /buttons/{id}/labels/{label}`, `DELETE …` | rename a label (`{"name": …}`) or delete it, keeping its games (`204`) |
| `PUT /buttons/{id}/labels/{label}/items/{item}`, `DELETE …` | put a game in a label, or take it out (`204`) |
| `GET /desktops` | virtual desktops, and the desktop files without a desktop: `{"desktops":[{id, name, index, current}], "unmatched":[name], "error": null}` |
| `POST /desktops` | `{"name": …}`: create a desktop with this name and switch to it (`201`, the new `GET /desktops` body; `409 exists`) |
| `POST /desktops/{id}/switch` | switch to a virtual desktop; returns the new `GET /desktops` body |
| `POST /windows/{hwnd}/pin` | show a window on every virtual desktop (`hwnd` in decimal or `0x` hex) |
| `GET /touch-monitors`, `PUT /touch-monitors/{id}` | see [Touch and the mouse cursor](#touch-and-the-mouse-cursor) |
| `PUT /fanza/session` | `{"cookies": [{name, value, domain, path, expires, secure, http_only}]}` from the panel's FANZA login window: keep DMM's cookies and read the purchases at once; returns `{"kept": n}` (`400 invalid_session` without a login cookie, `404` without a FANZA library) |
| `POST /power/sleep` | put the PC to sleep (answers `202` first) |
| `GET /version` | `{"version": "0.1.0"}` |
| `POST /update/check` | check for an update now (see [Updates](#updates)) |
| `GET /events` | WebSocket: one `snapshot` message, then `button` and `desktops` messages as things change (below) |

Errors are JSON `{"error": code, "message": …}`: `404 not_found`, `409 device_unavailable`,
`409 not_running`, `500 audio`, `409 no_window`, `500 launch`, `409 not_pressable` (a library button),
`409 choose_program`, `409 no_program`,
`400 invalid_label` (an empty or taken name, or Steam's own label), `409 labels_unavailable`
(Steam cannot be asked now, with the reason), `500 labels`,
`500 storage`, `400 invalid_hwnd`, `400 invalid_name`, `409 exists`, `503 desktops`, or
`502 update`.

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

To try the label scripts against the Steam client on this PC (it must accept remote
control, see [Labels](#labels)), run them on a throwaway label: it is made, renamed, given
the game and emptied again, then deleted.

```powershell
cargo run --example steam_labels -- 105600   # an app ID you own
```

## License

MIT. The released exe includes UnRAR (through the [unrar](https://crates.io/crates/unrar) crate),
which comes under the [UnRAR license](https://www.rarlab.com/license.htm): it may be used to
unpack RAR archives but not to make a RAR-compatible archiver.
