# Platform roadmap: what YARD still lacks

## Goal

The eight steps of `docs/roadmap.md` are done: an application made from YARD is configurable, drawn with Zed's components, used from the keyboard, framed, laid out, remembered, bundled for macOS, opened by URL, and updated. The release system is designed as a whole in `release-system.md`; its slices are the first rows below. This roadmap lists what a desktop platform is commonly expected to give and YARD does not yet, found while writing the wiki by comparing YARD with Tauri, Electron, Flutter desktop, Wails, and Dioxus, and checked against YARD's code.

## How this roadmap is followed

As every roadmap: features one at a time, in the order of the table, each with a short contract in this folder (`docs/platform/<slug>.md`) approved before any code, and the workflow of `AGENTS.md`. A feature that needs an outside account or a decision of the user (an Apple Developer account, a hosting choice, a licence question) stops at that decision.

## Features

Status is one of: planned, dev, production. Size: S (a day), M (a few days), L (weeks).

| # | Slug | What it adds | Who has it | Size | Needs | Status |
|---|---|---|---|---|---|---|
| 1 | `packaging` | The release system's first slice (`release-system.md`): `cargo xtask bundle` (`macos`, `linux`, `windows`) makes an installable app from `product.toml` alone (macOS `.app`, `.zip`, and `.dmg`, Linux `.tar.gz`, Windows Inno Setup installer and `.zip`), signed with the developer's identity when configured (Developer ID with notarization, Authenticode); the app runs on the three with drawn window controls, its URL scheme, and one instance | Zed's `bundle-mac`, `bundle-linux`, `bundle-windows.ps1` | L | none | dev: `yard_build_config` in production; `cargo xtask bundle macos` with signing and the disk image, and `cargo xtask bundle linux`, in dev; the app runs on Linux with drawn window controls, one instance, and URLs; Windows builds and runs with drawn caption buttons, as one instance, with URLs from the command line; the installer is next |
| 2 | `ci` | `cargo xtask workflows` generates `ci.yml`, checked against the code, run on macOS, Linux, and Windows runners | Zed's `cargo xtask workflows` | M | packaging | planned |
| 3 | `release-local` | `[release]` with its targets; `cargo xtask release plan` and `build`, `--dry-run`, `manifest.json` | cargo-dist, GoReleaser | M | ci | planned |
| 4 | `release-workflow` | The generated `release.yml`: plan, CI, a build per system, a GitHub draft, announce | cargo-dist, Zed's `release.yml` | M | release-local | planned |
| 5 | `release-updates` | `[release.pages]`, `[release.updates]`, the signed `latest.json` written last, pruning, re-runs; installed copies update themselves on the three systems | Tauri, Velopack, Zed's `auto_update` | L | release-workflow | planned |
| 6 | `channels` | Preview derived from a pre-release version: its own name, identifier, scheme, and folders | Zed's release channels, electron-builder | M | release-updates | planned |
| 7 | `release-hosts` | `s3`, `ssh`, `command`, and the destination trait | GoReleaser, electron-builder | M | release-updates | planned |
| 8 | `new-app` | `script/new-app <name> <identifier>`: renames the sample application (crate, binary, `product.toml`, icon, URL scheme) in one step, so that re-branding is not a hand edit | `create-tauri-app`, `dx new`, `flutter create` | S | none | planned |
| 9 | `upstream-updates` | A documented and scripted way for an application made from the template to take later YARD fixes: an `upstream` remote and a merge procedure that re-stamps `production.json`, or engines published as versioned git crates | every framework ships itself as versioned packages | M–L | none | planned |
| 10 | `file-dialogs` | Open and save dialogs through the extension handle, wrapping GPUI's `prompt_for_paths` and `prompt_for_new_path`, with logging | Tauri dialog plugin, Electron `dialog` | S | none | planned |
| 11 | `os-notifications` | Notifications in the OS's notification centre when the window is not in front, through the handle | Tauri notification plugin, Electron notifications | S–M | none | planned |
| 12 | `crash-reports` | Native crash dumps beside the panic log, and an opt-in "send report" at the next start | Electron `crashReporter`; Zed's `crashes` (reference only) | M | telemetry-consent | planned |
| 13 | `api-docs` | `cargo doc` of the public engine APIs published to GitHub Pages by CI, linked from the wiki | Tauri, Electron, Dioxus API references | S | ci | planned |
| 14 | `extension-toggle` | A settings section that turns a compiled-in extension off without a rebuild | Zed's extension settings | S | none | planned |
| 15 | `secrets` | An extension keeps a secret in the OS's store of credentials (the state roadmap's `secrets`, by need until now) | Tauri Stronghold, Electron `safeStorage` | S | none | planned |
| 16 | `telemetry-consent` | An opt-in setting and a first-run question for usage and error reports, honoured by every engine that would send anything | Zed's opt-in telemetry | M | none | planned |
| 17 | `tray` | A menu-bar item with a menu built from actions | Tauri system tray, Electron `Tray` | M | none | planned |
| 18 | `autostart` | Start at login, through the handle (`SMAppService`) | Tauri autostart, Electron login items | S | packaging | planned |
| 19 | `accessibility` | Roles and labels on `yard_ui`'s components through GPUI's AccessKit support, checked with VoiceOver | Electron, Flutter, AccessKit | M | none | planned |
| 20 | `settings-ui` | A settings editor and a JSON schema of the declared sections for completion in an editor | Zed's settings UI | M | none | planned |
| 21 | `i18n` | A string catalog, and the UI kit and menus taking keys instead of literal strings | Flutter, Dioxus, Project Fluent | M–L | none | planned |
| 22 | `universal-binary` | `cargo xtask bundle macos` builds arm64 and x86_64 and joins them with `lipo` | Tauri, Electron | S | none | planned |
| 23 | `mac-app-store` | A sandboxed build with entitlements and a `.pkg`, without the updater | Tauri, Electron | L | packaging | planned |

## Not planned

- A webview. YARD draws with the GPU through GPUI; that is its point.
- Loading extensions as WebAssembly at runtime, as Zed does: large, and Tauri does not either. `extension-toggle` covers turning one off.
