# Slice 1 of the release system: packaging

The first slice of `docs/platform/release-system.md`. A developer's `product.toml` goes in. An installable application for macOS, Linux, or Windows comes out, and it runs there the way it runs on macOS today. Nothing is released yet.

Everything here follows Zed, except where it says otherwise and why. The configs in this slice are test inputs: YARD packages nothing real.

## What the developer writes

The keys stay the ones `product.toml` has today. What changes is that every system reads them:

```toml
name = "Lab Notes"               # the app's name on every system
version = "0.3.0"
identifier = "test.yard.labnotes" # reverse-domain: bundle id, Wayland app_id, Windows AppUserModelID
url_scheme = "labnotes"
icon = "icon.png"                 # one square PNG, at least 512 × 512; every icon format is made from it
```

`PRODUCT_CONFIG=<path>` still names another file at build time.

How each system's package is made, signed, and wrapped is set in the target tables of `[release]`, as the release system's design keeps all delivery in `[release]`. Every table is optional: without it, the package is made unsigned (ad hoc on macOS) and without a disk image.

```toml
[release.macos]
dmg = true                         # a disk image beside the zip; default false
entitlements = "entitlements.plist" # optional, relative to the config; default: none beyond the hardened runtime
[release.macos.signing]            # absent: signed ad hoc
identity = "Developer ID Application: Acme Co (AB12CD34EF)"
notarize = true                    # default true

[release.windows.signing]          # absent: the installer and the .exe are not signed
timestamp_url = "http://timestamp.digicert.com"   # default
```

**Secrets never go in the config.** They come from the environment, one variable each, as Zed and Tauri read them:

| Variable | What it holds | Needed when |
|---|---|---|
| `MACOS_CERTIFICATE`, `MACOS_CERTIFICATE_PASSWORD` | the `.p12` of the identity, base64 | signing on a machine that does not have the identity, such as CI |
| `APPLE_NOTARIZATION_KEY`, `APPLE_NOTARIZATION_KEY_ID`, `APPLE_NOTARIZATION_ISSUER_ID` | an App Store Connect API key (`.p8`, base64) | `notarize = true` |
| `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD` | the `.pfx` of the code-signing certificate, base64 | `[release.windows.signing]` |

Without `MACOS_CERTIFICATE`, the identity is looked up in the keychains the developer already has, read only.

## Signing and the disk image

**macOS**, in the order of Zed's `script/bundle-mac`:
1. With `MACOS_CERTIFICATE`: a temporary keychain is made in a private folder under `TMPDIR`, put first in the user's search list right after, and the certificate imported into it. The login keychain is never touched.
   - Every `security` command runs in its own process group, so a terminal's Ctrl-C reaches only xtask.
   - Passwords and paths reach `security -i` on standard input. A value holding a line break or a NUL is refused, and so is a command of 4096 bytes or more, since `security` would split or cut it. The refusal names the value, never its content.
   - At the end, even after a failure, a signal, or a panic, `security delete-keychain` deletes the keychain, which also takes it out of the search list. Every other keychain stays, including ones added during the run. If the keychain cannot be deleted, the error gives the command that deletes it.
   - If `security list-keychains` prints a line that is not one quoted path, as a path with a line break or a double quote does, the search list is not set, and the temporary keychain is deleted.
2. The binary, then the `.app`, are signed with the identity: `--options runtime`, `--timestamp`, and the entitlements. Without an identity, the app is signed ad hoc, still with the entitlements when the config names them, as Zed's local bundle is. `codesign --verify --deep --strict` then checks the result.
3. With `notarize`: the app is zipped into a temporary file, submitted with `notarytool submit --wait` and the API key, and the ticket stapled to the app. A rejection stops the run, and the notarization log is printed.
4. With `--zip`: the `.zip` is made after stapling, from the stapled app in the staging folder, so its digest is of what users download. Without `--zip`, no zip is made.
5. With `dmg = true`: an image holding the staged app and a link to `/Applications` (`hdiutil`, UDZO). With an identity it is signed itself, with the signature identifier `<identifier>.dmg`; with `notarize = true` too, it is notarized and stapled, not only the app inside it. Electron Forge notarizes only the app, and Gatekeeper then blocks the image. Without an identity, the image is not signed.

The app, the zip, and the disk image are all made inside a staging folder in `--out`. Only after the last step are they moved into place together. If one move fails, the ones before it are taken back and the previous files restored, so `--out` is either fully the new release or exactly as it was.

**Secrets stay with signing.** The seven secret variables, Windows' included, are removed from the environment of the `cargo` commands xtask runs (the stages check and the build), so no build script of the app sees them. `cargo xtask` itself first compiles xtask with the caller's environment, so a CI job builds xtask in an earlier step (`cargo build -p xtask`) and gives the secrets only to the step that bundles.

**Stopping a run.** A SIGINT, SIGTERM, or SIGHUP does not kill xtask. The running command finishes, or dies with the terminal's Ctrl-C. xtask then stops before its next step, leaves `--out` as it was, cleans up as above, logs the stop at `info`, and exits with code 130. A run is an interrupt when the signal came at any time before the clean-up ended, or when a command it ran ended by one of those signals. A second Ctrl-C does not kill xtask either.

**Logs.** xtask logs through `yard_log`, as the product `xtask`. The diagnostic log `xtask.log` (rotated to `xtask.log.old`) and the lifecycle trace `lifecycle.jsonl` are in `CARGO_TARGET_DIR`, or in `target/` of the repository; never in `--out`, which holds only what is published. `XTASK_LIFECYCLE` sets the trace's filter, `info` by default.

**Windows:** with `[release.windows.signing]`, `signtool sign /fd sha256 /tr <timestamp_url> /td sha256` signs the app's `.exe` before it goes into the installer, then signs the installer. Inno Setup's `SignTool` directive does the latter, as in Zed's `zed.iss`. The `.pfx` is imported for the run only.

**Linux** packages are not signed, as Zed's are not.

## The parts

### 1. `yard_build_config`: the config, typed

- **Reading.** A new engine crate, without GPUI. It reads `product.toml` into a Rust type, checking each table against its list of keys and refusing any other key. It then checks today's rules:
  - non-empty single-line strings;
  - the reverse-domain identifier;
  - the lowercase URL scheme;
  - a square PNG of at least 512 pixels;
  - the update address.
- **Errors.** Each refusal names the file and the key.
- **One home for each rule.** `build.rs` reads only through the crate. The identifier rule lives in `yard_product` (`is_identifier`), and the crate calls it, so the build and the running app accept the same identifiers.
- **Who reads it.** `build.rs` and `xtask` both read the config through this crate. The script that read TOML on its own is deleted.

### 2. `cargo xtask bundle <macos|linux|windows> [--config <file>] [--out <folder>]`

- **The tool.** `tooling/xtask`, with the alias `cargo xtask` in `.cargo/config.toml`, as in Zed.
- **What it does.** It builds the production binary with `PRODUCT_CONFIG` set to the config. It makes every icon format from the one PNG (YARD's own choice: Zed commits its icons in each format). It then lays out the package below.
- **Kept from `script/bundle-macos`:**
  - the review bundle (`--dev --env KEY=VALUE`), with its separate name, identifier, and scheme;
  - the stages check before building;
  - the executable path Cargo reports;
  - restoring every previous file when publishing fails.

| System | What comes out (`<out>/`) | From Zed |
|---|---|---|
| macOS | `<Name>.app`, signed ad hoc or with the identity; with `--zip`, `<Name>-<version>.zip` made with `ditto`; with `dmg = true`, `<Name>-<version>.dmg` | `script/bundle-mac` |
| Linux | `<name>-<version>-linux-<arch>.tar.gz`, holding `<name>.app/` (see below) | `script/bundle-linux` |
| Windows | `<Name>-<version>-<arch>-setup.exe` made by Inno Setup, and `<Name>-<version>-windows-<arch>.zip` | `script/bundle-windows.ps1`, `zed.iss` |

**The Linux package.** Zed's layout and install script. On Linux, `<name>` is the product name in lower case, each run of characters other than ASCII letters and digits turned into one `-` (`Lab Notes Pro` → `lab-notes-pro`), as Zed's files are named `zed`; a config whose name gives an empty `<name>` is refused.

```text
<name>.app/
  bin/<name>                                   the app; with --dev, a sh wrapper that sets the --env
                                               values and runs libexec/<name> dev
  libexec/<name>                               with --dev only: the app
  share/applications/<identifier>.desktop      Name, Exec=<name> %U, TryExec, Icon, StartupNotify,
                                               MimeType=x-scheme-handler/<url_scheme>;
  share/icons/hicolor/512x512/apps/<identifier>.png
  share/icons/hicolor/1024x1024/apps/<identifier>.png   only when the config's icon is at least 1024 × 1024
  install.sh                                   copies <name>.app to ~/.local, links ~/.local/bin/<name>,
                                               puts the .desktop file in $XDG_DATA_HOME/applications
                                               (else ~/.local/share/applications) with absolute paths, runs
                                               update-desktop-database when present, and warns of libraries
                                               that ldd does not find
```

- `cargo xtask bundle linux` runs on Linux only, and refuses on any other system. It writes `<out>/<name>-<version>-linux-<arch>.tar.gz` and the same package unpacked as `<out>/<name>.app`, and prints `sha256 <hex>`, then the archive's path.
- Icons are resized from the config's PNG with Lanczos3, at Zed's sizes, and never scaled up.
- A `--dev` package must differ from production in its Linux `<name>` too.
- **Reinstalling.** `install.sh` keeps the previous install until every step has succeeded. A failure or an interrupt puts it back.
- **Install paths with a space or `%`.** On such a path, GIO and the generic `xdg-open` of xdg-utils 1.1.3 may fail to start the app from the menu entry or a URL. In that case `install.sh` warns. The command `~/.local/bin/<name>` still works.
- It is built for the runner's `x86_64-unknown-linux-gnu` on Ubuntu 22.04, the oldest GitHub-hosted image. That image sets the oldest glibc the app runs on.
- It is stripped of debug data, as Zed's is.
- `install.sh` sits inside the archive. Zed's `install.sh` is downloaded from Zed's server, and a YARD app has no server, so the script ships with the package.

**The Windows installer.** Zed's `zed.iss`, cut to what YARD has:

- **Install location.** A per-user install (`PrivilegesRequired=lowest`) to `%LOCALAPPDATA%\Programs\<Name>`, with a Start-menu shortcut whose AppUserModelID is the identifier.
- **The URL scheme, in the registry:** `HKCU\Software\Classes\<url_scheme>` with `URL Protocol`, `DefaultIcon`, and `shell\open\command "<app>.exe" "%1"`.
- **The installer's own settings:**
  - `AppId` is derived from the identifier.
  - `AppMutex` names the same mutex the app creates. Both take it from one function, unlike Zed, whose installer names a different one.
  - Uninstall removes all of the above.
- **The executable.** `build.rs` makes a resource script from the config, written from Microsoft's VERSIONINFO reference, and compiles it into the `.exe` with `embed-resource`:
  - **Resource 1, the icon.** An ICO at 16, 24, 32, 48, 64, and 256 px, made from the config's PNG. GPUI loads it for the window and the taskbar.
  - **VERSIONINFO.** ProductName and FileDescription come from the name. FileVersion and ProductVersion come from the version. The numeric version is `major.minor.patch.0`, read before any `-` or `+`. A number over 65535 fails the build, naming the config. There is no CompanyName, since the config has none.
  - **Built on Windows only.** The resource compiles only on a Windows host. A Windows build made on another system has neither, and the build warns.
  - **No console.** A release build runs without a console (`windows_subsystem = "windows"`). A debug build keeps one, for the lifecycle trace.
- **Not copied from Zed:** file associations, context menus, PATH, a WSL wrapper.
- Signing is described in "Signing and the disk image" above, and comes with part 5 of this slice.

### 3. The app on Linux and Windows

These change engines that are in production today. Each change goes to the user for approval before it is stamped again.

- **Window controls (`yard_ui`'s title bar).** They are drawn as Zed's `platform_title_bar` draws them:
  - **Linux.** Client-side decorations, as Zed's `client_side_decorations` does:
    - minimize, maximize, and close in the desktop's button layout;
    - double-click maximizes, right-click opens the window menu, dragging the bar moves, and the edges resize;
    - a shadow, and rounded corners when not tiled, over a transparent window background.
  - **When GPUI falls back to the system frame.** On X11, GPUI draws them only when a compositing window manager supports `_GTK_FRAME_EXTENTS` (as GNOME, KDE, and Xfce do); otherwise the window manager's own frame is used. On Wayland they are always drawn.
  - **Windows.** The three caption buttons are drawn with Segoe Fluent Icons, and GPUI's `WindowControlArea` hit-testing does the rest.
  - **The GPL boundary.** What is copied from Zed's GPL crates (`platform_title_bar`, `workspace`) stays inside `yard_ui`, with its notices, as the GPL boundary requires.
- **Window options.**
  - `app_id` is the identifier, so Wayland and X11 match the window to its `.desktop` file.
  - The X11 icon comes from the config.
  - On Windows, `cx.set_app_identity(identifier, name)` is called first thing in the app's run, logged as `app identity set`. The shortcut uses the same identifier.
- **One instance (`yard_shell`).** On Linux, a start first looks for the running instance through a socket at `<data>/instance/instance.sock`:
  - It holds a lock (`instance.lock` in the same folder) while it looks, so two starts at the same moment end with exactly one instance.
  - The folder is private to the user (0700) and the socket is 0600. A start sends only to a socket owned by its own user, and removes only a stale socket of its own user, never a file or a link. The lock is never followed through a link and never waits on a FIFO.
  - The instance folder is used only when its parent, the data folder, belongs to the user and others cannot write to it. Its group may write to it only if that group is the user's own, as Ubuntu gives each user a group of their own. A data folder YARD creates itself is private (0700) whatever the umask. Otherwise the app runs without one instance, with a warning.
  - If an instance runs, the start hands it only its URLs of the product's scheme, in one message, and exits with code 0; other arguments never leave the process. It waits at most 2 seconds to hand them over, and a message must be under 64 KiB; otherwise it prints the reason and exits with code 1.
  - Otherwise the start becomes the instance. On quit, under the lock, it removes its socket only if it is still the one it bound; on Linux the identity includes a modification time the instance sets, because a new socket can get the inode of a removed one. After a crash or `kill`, the socket stays, and the next start removes it as stale.
  - FreeBSD runs without one instance. Windows gets it in part 5, with a named mutex and a named pipe, both named from the identifier.
  - **No separate CLI binary** (YARD's own choice). Zed sends through a separate `cli` binary; YARD's app does it itself.
- **URLs.** GPUI does not deliver `on_open_urls` on Linux or Windows, so URLs come as arguments, to the first start or forwarded by a later one. Only arguments whose scheme is the product's `url_scheme` count as URLs. Everything else is ignored, and arguments are never logged. The log names a URL's host only when it is a registered extension; other hosts are refused as `no such extension`, without the value. URLs reach the same handler that macOS's `on_open_urls` feeds, so an extension's `on_open_url` works the same on every system.
- **Bringing the window to the front.** Every forwarded request, with or without URLs, asks for the window last used to come to the front. The system may refuse:
  - On X11, the request is `_NET_ACTIVE_WINDOW`, which a window manager may refuse under focus-stealing prevention.
  - On Wayland, a compositor gives focus only with a recent input event; KWin and Mutter otherwise mark the window as demanding attention.
- **Directories (`yard_paths`), as Zed's `paths`:**
  - **Linux:** `$XDG_CONFIG_HOME/<name>` (else `~/.config/<name>`) and `$XDG_DATA_HOME/<name>` (else `~/.local/share/<name>`), with logs in `<data>/logs`, as Zed's. State and cache folders come when something uses them.
  - **Windows:** `%APPDATA%\<Name>` for settings, and `%LOCALAPPDATA%\<Name>` for data, with logs in `<data>\logs`, as Zed's. `XDG_*` variables are ignored there.
- **Updates.** On Linux and Windows, `yard_update` compiles and does nothing, with a `debug` line, until the `release-updates` slice. The macOS-only code (`renamex_np`, `ditto`, `codesign`) is put behind `cfg(target_os = "macos")`.

## Configs under test

The configs live in `crates/yard_build_config/tests/configs/`, where `xtask` reads them too. Every one is a test input, and each is a different app:

| Config | What it proves |
|---|---|
| `plain` | Name `Lab`, identifier `test.yard.lab`, scheme `yardlab`: the basic case |
| `spaced` | Name `Lab Notes Pro`, with spaces: kept as written in the macOS bundle, the Windows installer and shortcuts, and the directories on macOS and Windows; `lab-notes-pro` in the Linux files and directories |
| `other` | Another name, identifier, scheme, and icon: two apps installed side by side do not meet (directories, socket, mutex, scheme). Two names that give the same Linux `<name>`, such as `Lab Notes` and `Lab-Notes`, share their Linux directories; an app's name must differ from another's in its letters or digits |
| `signed` | `[release.macos]` with `dmg` and `[release.macos.signing]`, and `[release.windows.signing]`. Its identity is a self-signed code-signing certificate made by the test (`Developer ID Application: YARD Test (YARDTEST01)`), since no real Developer ID belongs in YARD |
| `bad-*` | A missing key, a bad identifier, a name with no ASCII letter or digit (no `<name>` for Linux), an icon that is not square or too small, an unknown key, an unknown key in `[release.macos]`: each refused, naming the file and the key |

## How it is checked

**Tests, in the repository:**
- `yard_build_config`: every refusal.
- `xtask`: the package laid out from each config, with no build, on any system. The `Info.plist` values, the `.desktop` fields, the `.iss` values, the Linux layout, and the icon sizes are each compared with the config.
- The engines: the instance socket and pipe; the URL forwarded from a second start to the handler; the directories per system.

**On GitHub Actions runners, in a throwaway repository** (`macos-latest`, `ubuntu-22.04`, `windows-latest`), for `plain`, `spaced`, and `other`:

| | What runs |
|---|---|
| macOS | Bundle, then `codesign --verify`, then `plutil` against the config; open the app and take a screenshot; `open <scheme>://…` reaches the extension |
| Linux | Bundle, then `desktop-file-validate`; run `install.sh`, then `xdg-open <scheme>://…` with no instance running and again with one running: the first starts the app with the URL, the second reaches the running instance (read in its log). The app runs on Xorg's dummy driver with xfwm4's compositor; the runner has no GPU, so its window shows black, and screenshots come from GPUI's headless renderer (`examples/screenshot`) |
| Windows | Bundle; install silently (`/VERYSILENT`), then read the registry keys; start the app and take a screenshot; `start <scheme>://…` reaches the running instance; uninstall silently and check that the keys are gone |

The lead reads every log and screenshot, and runs the macOS bundles locally too.

**Signing, checked with the `signed` config:**
- On macOS, `codesign -dv` names the test identity, with the runtime flag and the entitlements. The DMG is signed with the same identity. The temporary keychain is deleted afterwards and is no longer in the search list, also after an interrupted run.
- Notarization is checked as far as the command: `notarytool submit` with the key's id, issuer, and file. A real submission needs the publisher's Apple account, and is not verified.
- On Windows, `signtool verify /pa` on the `.exe` and the installer shows the test certificate, made by the runner with `New-SelfSignedCertificate`.

**Not checked by a machine:** how the window controls look next to Zed's on Linux and Windows. The lead compares the runners' screenshots with Zed's own on each system.

## Order inside the slice

Each part goes up for the user's review on its own:

1. `yard_build_config`, and `build.rs` reading through it.
2. `cargo xtask bundle macos`, replacing `script/bundle-macos`, with the configs under test.
3. macOS signing, notarization, and the DMG: `[release.macos]`, the secrets, the temporary keychain.
4. The app on Linux, then `bundle linux`.
5. The app on Windows, then `bundle windows`, with its signing.
6. The runner checks above, on the throwaway repository.

## Not in this slice

- Releasing, CI in the template, channels, updates on Linux and Windows, ARM builds for Linux and Windows: later slices.
- Flatpak, `.deb`, AppImage, MSI. Zed ships a tarball and an Inno Setup installer; others can be added later.
