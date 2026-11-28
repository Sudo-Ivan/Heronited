<h1 align="center"><img src="assets/icon.svg" alt="" width="44" height="51" align="absmiddle" /> Heronited</h1>

<p align="center">
  <a href="https://github.com/Sudo-Ivan/Heronited/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/Sudo-Ivan/Heronited?color=2B8CA8&label=release" /></a>
  <a href="https://github.com/Sudo-Ivan/Heronited/releases"><img alt="Downloads" src="https://img.shields.io/github/downloads/Sudo-Ivan/Heronited/total?color=2B8CA8&logo=github&label=downloads" /></a>
  <a href="https://github.com/Sudo-Ivan/Heronited/actions/workflows/ci.yml"><img alt="CI on Linux and Windows" src="https://img.shields.io/github/actions/workflow/status/Sudo-Ivan/Heronited/ci.yml?branch=main&label=CI&logo=githubactions&logoColor=white" /></a>
  <img alt="Built with Rust" src="https://img.shields.io/badge/built%20with-Rust-2B8CA8.svg?logo=rust&logoColor=white" />
  <a href="LICENSE"><img alt="License: GPL v3" src="https://img.shields.io/badge/License-GPL_v3-2B8CA8.svg" /></a>
</p>

<p align="center">
  Every game you own, on Linux and Windows, in one fast native launcher.
</p>

<p align="center">
  <a href="https://github.com/Sudo-Ivan/Heronited/releases/latest"><b>Download</b></a> ·
  <a href="https://github.com/Sudo-Ivan/Heronited/releases"><b>Release notes</b></a> ·
  <a href="https://github.com/RafaelGoulartB/Heronite"><b>Upstream: Heronite</b></a>
</p>

<p align="center">
  <img alt="Heronited's Collection in Focus view: picking CONTROL and back to ELDEN RING, a look at its Compatibility, Performance and GPU settings, then the Grid view and back to Focus" src="assets/screenshots/demo.webp" width="100%" />
</p>

Heronited is a personal fork of [Heronite](https://github.com/RafaelGoulartB/Heronite) by Rafael Goulart. All credit for the app goes to its original author; see [upstream](https://github.com/RafaelGoulartB/Heronite) for the main project.

Heronited brings your games from Steam, Epic Games, GOG, Amazon Games, Xbox and PC Game Pass, Battle.net, EA app, Ubisoft Connect, and Rockstar Games, plus emulated, native Linux, and Windows games, into one Collection. It launches them natively, through Wine/Proton on Linux, or through an emulator, records every session, and [stays light on your PC](#light-on-your-pc) as a native Rust app (GPUI, GPU rendered).

Coming from Heroic, Lutris, Bottles, Faugus, or Playnite? Heronited imports your games from them.

## Differences from upstream

Heronited tracks [Heronite](https://github.com/RafaelGoulartB/Heronite) and picks up its fixes, but some parts are ours:

- **Saves backups** go to a saves server (saves.quad4.io by default, or your own) or a local folder, in place of Ludusavi. Backups can include the game's Wine prefix data, and a game whose saves are not detected can be pointed at a folder by hand.
- **GOG offline installers are kept as backups.** Windows GOG games install from GOG's own offline files, which stay in `~/Games/GOG Offline Installers` so a reinstall needs no download.
- **The overlay draws over games on X11**, and its shortcut works everywhere: a native X11 grab on bare window managers, and commands you bind in sxhkd, i3, sway and similar where the desktop has no GlobalShortcuts portal ([docs/SHORTCUTS.md](docs/SHORTCUTS.md)).
- **Secrets are sealed with the OS keyring** before they are stored.
- **Packaging is ours:** AppImage, Flatpak, and the system packages under the app id `io.github.sudoivan.Heronited`. Upstream's COPR, Scoop, and Winget channels are not carried.
- **CI and releases are hardened for this repository:** pinned actions, zizmor audits, minisign-signed updates.
- **More built-in appearance profiles:** Void, OLED, Nord, Dracula, Catppuccin Mocha, Gruvbox Dark, and Catppuccin Latte join upstream's four.
- **Idles at zero on X11:** the window's refresh timer parks while nothing changes, so an idle window costs no wakeups (upstream redraws at the display's refresh rate).

Upstream main is merged in full with `git merge upstream/main`, so syncing is a plain merge. The points above are where Heronited still differs.

## Download

<div align="center">

| Platform | Package | Download |
|:---|:---|:---|
| 🪟 Windows | Installer | [Heronited-Setup-x86_64.exe](https://github.com/Sudo-Ivan/Heronited/releases/latest/download/Heronited-Setup-x86_64.exe) |
| 🪟 Windows | Portable | [Heronited-Portable-x86_64.zip](https://github.com/Sudo-Ivan/Heronited/releases/latest/download/Heronited-Portable-x86_64.zip) |
| 🐧 Linux | AppImage (any distro, updates itself) | [Heronited-x86_64.AppImage](https://github.com/Sudo-Ivan/Heronited/releases/latest/download/Heronited-x86_64.AppImage) |
| 🐧 Arch, Debian, Fedora | .pkg.tar.zst, .deb, .rpm | [Latest release](https://github.com/Sudo-Ivan/Heronited/releases/latest) |

</div>

Only 64-bit builds are made: Windows x86_64 and Linux x86_64 (AppImage, Flatpak bundle, and system packages).

<details>
<summary><b>How to install</b></summary>

| System | File | Install |
|---|---|---|
| Any distribution (**recommended**) | `Heronited-x86_64.AppImage` | `chmod +x Heronited-x86_64.AppImage && ./Heronited-x86_64.AppImage` |
| Flatpak | `Heronited-x86_64.flatpak` | `flatpak install --user ./Heronited-x86_64.flatpak` |
| Debian 12+, Ubuntu 22.04+, Mint, Pop!_OS | `heronited_<version>-1_amd64.deb` | `sudo apt install ./heronited_*_amd64.deb` |
| Fedora 36+, openSUSE | `heronited-<version>-1.x86_64.rpm` | `sudo dnf install ./heronited-*.x86_64.rpm` |
| Arch, Manjaro, EndeavourOS, CachyOS | `heronited-<version>-1-x86_64.pkg.tar.zst` | `sudo pacman -U heronited-*-x86_64.pkg.tar.zst` |
| Windows 10 and 11 (**recommended**) | `Heronited-Setup-x86_64.exe` | Run it: it installs for your user, no administrator rights needed |
| Windows 10 and 11, portable | `Heronited-Portable-x86_64.zip` | Extract it anywhere and run `Heronited\heronite.exe`; the library, settings, and screenshots stay in that folder |

</details>

The Windows files are not code-signed yet, so SmartScreen may warn about them: choose **More info**, then **Run anyway**.

### Checking a download

Every release file has a SHA-256 in `SHA256SUMS` (itself signed as `SHA256SUMS.minisig` with the minisign key the in-app updater trusts) and a SLSA build provenance attestation. With the [GitHub CLI](https://cli.github.com/):

```sh
sha256sum -c SHA256SUMS --ignore-missing
gh attestation verify Heronited-x86_64.AppImage --repo Sudo-Ivan/Heronited
```

The attestation proves the file came from this repository's release workflow, not just that someone uploaded it.

---

## Your library, organized

- **Collection:** a list with details or a grid of covers, grouped by status, store, genre, series, game mode (co-op, split screen...), or year, and filtered by any of those or by the tags Steam players gave a game. Details show screenshots, How Long To Beat, achievements, and a play timeline.
- **Journal:** notes on your play days and screenshots.
- **Views:** filter, sort, and save presets like Playing now or Backlog. Search forgives typos.
- **Bulk edits:** change many games at once, with undo.
- **Command palette:** <kbd>Ctrl</kbd> <kbd>K</kbd> opens any page, action, or game.
- **Add any game:** programs, browser games, scripts, or ROM folders, with art from Steam, SteamGridDB, IGDB, and more.

<table>
  <tr>
    <td width="50%" align="center"><a href="assets/screenshots/collection.webp"><img src="assets/screenshots/collection.webp" width="100%" alt="Collection in Focus view with ELDEN RING selected, showing screenshots, overview, details, and the play timeline over the game's art" /></a><br /><sub><b>Collection</b></sub></td>
    <td width="50%" align="center"><a href="assets/screenshots/command-palette.webp"><img src="assets/screenshots/command-palette.webp" width="100%" alt="Command palette searching for 'contr', listing matching games and the play, settings, and open folder actions for Control" /></a><br /><sub><b>Command palette</b></sub></td>
  </tr>
</table>

## Stores and launchers

- **Epic Games, GOG, and Amazon Games:** log in and install, update, repair, or move games from one download queue. On Windows, games from their official launchers come in without downloading again.
- **GOG offline installers kept (fork default):** Windows GOG games are fetched as GOG's own offline backup installers into `~/Games/GOG Offline Installers` (a folder per game, with `installers.json`) and set up from them on first start — the files stay as your backup and a reinstall needs no download. Turn it off or pick another folder in Settings → Downloads; games without offline installers fall back to the depot download.
- **Steam:** your library comes in by itself, no login needed.
- **Xbox and PC Game Pass (Windows):** Microsoft Store and Game Pass games show up in your Collection, launch from Heronited, and count their play time.
- **Battle.net, EA app, Ubisoft Connect, and Rockstar Games:** their games join your Collection, through the store's own app on Windows and in its own Wine prefix on Linux. Connect your EA account on the EA app's page to also see the EA games you own that are not installed.
- **Other launchers:** import and sync games from Heroic, Lutris, Bottles, and Faugus on Linux, and from Heroic and Playnite on Windows (Settings → Other launchers).
- **Games you already have:** add one by hand, or a whole folder at once.

## Configure and play

- **Wine and Proton (Linux):** per-game version and prefix, DXVK, gamescope, FPS limit, DLSS/FSR upgrades, Winetricks, and Lossless Scaling frame generation.
- **Wine Manager (Linux):** download Proton-CachyOS, GE-Proton, and other builds, and manage prefixes.
- **Launching:** scripts before and after, programs alongside (like trainers), and presets to reuse.
- **Sandbox (Linux):** run untrusted games without network or access to your home folder.
- **Emulators:** detected by themselves or added with RetroArch cores, with ROM scanning and save states.
- **GOG achievements without GOG Galaxy:** [Comet](https://github.com/imLinguin/comet) runs beside GOG games on Windows and Linux, so what they unlock counts on your GOG account and shows over the game as it happens.
- **In-game overlay:** volume, screenshots and notes over the game; Linux also has live FPS limits and can show or hide MangoHud. Windows supports windowed/borderless games, with Ctrl+Shift+G or an XInput controller's Back+Start ([limits](docs/WINDOWS-MIGRATION.md#-in-game-overlay)).
- **Replay (Linux):** save the last seconds of play with Guide + X, recorded by [gpu-screen-recorder](https://git.dec05eba.com/gpu-screen-recorder/about/).
- **Sharing:** send a screenshot or clip to your own server (S3, SFTP, WebDAV or Nextcloud, FTP, a ShareX-style endpoint) from the overlay, the capture list or a bound key, and get the link on the clipboard ([docs/SHARING.md](docs/SHARING.md)).

<table>
  <tr>
    <td width="50%" align="center"><a href="assets/screenshots/game-settings.webp"><img src="assets/screenshots/game-settings-proton.webp" width="100%" alt="Control's Compatibility settings: GE-Proton-latest as the version, ProtonDB Platinum with the recommended build, the game's prefix, and the Winecfg, Winetricks, and Run .exe in prefix buttons" /></a><br /><sub><b>Proton and prefix, per game</b></sub></td>
    <td width="50%" align="center"><a href="assets/screenshots/wine-manager.webp"><img src="assets/screenshots/wine-manager.webp" width="100%" alt="Wine Manager with Proton-CachyOS, GE-Proton, DW-Proton, UMU-Proton, and Proton-EM, each explained, next to the list of Proton-CachyOS builds" /></a><br /><sub><b>Wine Manager</b></sub></td>
  </tr>
</table>

## Look back at what you played

- **Statistics:** play time by week, month, or year, plus a yearly backlog goal.
- **Year in review:** your year as full-screen stories, with a card to share.
- **Sessions:** every launch is recorded, and screenshots are matched to the game.

<table>
  <tr>
    <td width="50%" align="center"><a href="assets/screenshots/statistics.webp"><img src="assets/screenshots/statistics.webp" width="100%" alt="Statistics Overview for October 2026 with total time played, cards for the other tabs, and a day-by-day chart per game" /></a><br /><sub><b>Overview</b></sub></td>
    <td width="50%" align="center"><a href="assets/screenshots/year-in-review.webp"><img src="assets/screenshots/year-in-review.webp" width="100%" alt="Year in review intro story: Your 2026 in games, with days, games, and sessions next to a wall of covers" /></a><br /><sub><b>Year in review</b></sub></td>
  </tr>
  <tr>
    <td width="50%" align="center"><a href="assets/screenshots/stats-backlog.webp"><img src="assets/screenshots/stats-backlog.webp" width="100%" alt="Backlog tab for 2026 with 9 games beaten, the yearly goal ring, and the beaten games along the year" /></a><br /><sub><b>Backlog</b></sub></td>
    <td width="50%" align="center"><a href="assets/screenshots/year-in-review-wrap.webp"><img src="assets/screenshots/year-in-review-wrap.webp" width="100%" alt="Year in review closing story: 410 screenshots, 9 games beaten, and a share card with 711 hours played in 2026" /></a><br /><sub><b>Share card</b></sub></td>
  </tr>
</table>

## Console mode

A controller-first layout for the couch or a TV. Turn it on from the ☰ menu, the palette, or with `--console`.

- **Themes:** eight layouts (Shelves, Spotlight, Cinema, Grid, Wheel, and more), with your own covers, colours, fonts, and menus.
- **Share them:** seven built-in themes, or export yours as a `.heronite-theme.json` file.
- **Light when off:** nothing loads until you turn it on. It can open at startup or when a controller connects.

<table>
  <tr>
    <td width="50%" align="center"><a href="assets/screenshots/console-home.webp"><img src="assets/screenshots/console-home.webp" width="100%" alt="Console home with SP Football Life 2026 large over its art and a Continue playing shelf of covers" /></a><br /><sub><b>Home</b></sub></td>
    <td width="50%" align="center"><a href="assets/screenshots/console-game.webp"><img src="assets/screenshots/console-game.webp" width="100%" alt="Console game page for ELDEN RING with Play, Status, Favourite, Game settings, Playtime, and save backup rows next to its details" /></a><br /><sub><b>Game page</b></sub></td>
  </tr>
</table>

## Make it yours

- **Appearance:** dark or light, accent colour, fonts, density, and a background picture.
- **Language:** English, Portuguese (Brazil), Spanish, German, and French.
- **Saves and backups:** save backups to a saves server (saves.quad4.io by default, or your own) after each session and, if you choose, every 15 minutes to 2 hours while you play. Files are hashed and verified, limits are the server's, and a secure key is your account. Scheduled Collection backups too.
- **Sync:** status, ratings, notes, and play time stay the same across your PCs or both sides of a dual boot, through a shared folder or a cloud account ([rclone](https://rclone.org/)), which can also keep your backups, saves, and screenshots.
- **Storage:** see what your games take on each disk and clean up leftovers.
- **Startup:** start with the system, and launch games from `heronite://` links.

<table>
  <tr>
    <td width="50%" align="center"><a href="assets/screenshots/console-settings.webp"><img src="assets/screenshots/console-settings.webp" width="100%" alt="Console mode settings: the Minimal, Classic, Cinema, and Pulse themes, and the theme in use with a live preview next to its focus colour, cover shape and size, corners, background picture, font, hint bar, and button glyphs" /></a><br /><sub><b>Console mode themes</b></sub></td>
    <td width="50%" align="center"><a href="assets/screenshots/storage.webp"><img src="assets/screenshots/storage-panel.webp" width="100%" alt="Storage settings with disk usage bars, space to free up, and a treemap of installed games by size" /></a><br /><sub><b>Storage</b></sub></td>
  </tr>
</table>

## Light on your PC

A single native process drawn on the GPU, with no bundled browser like Electron-based launchers carry. The idle numbers come from [`scripts/idle-cost.sh`](scripts/idle-cost.sh), so you can check them on your own PC. Usage from a collection with 200 games.

| | Heronited Usage |
|---|---|
| Memory while idle | **~150 MB** in a single process, **~80 MB** closed to the tray |
| CPU while idle | **~0.1%** (0.05 s of CPU time over 60 s) |
| Install size | **64 MB** (app + store login helper) |
| Start to library on screen | **~215 ms** median |


## Run from source

<details>
<summary>Show build instructions</summary>

<br>

You need stable Rust. On Linux, also `clang` and `lld`, a Vulkan driver, and GTK/WebKitGTK; on Windows, the Visual Studio Build Tools ("Desktop development with C++") and [CMake](https://cmake.org) on the `PATH` (it builds SDL, which reads the controllers in console mode). Heronited downloads legendary, gogdl, and nile itself.

```bash
# quick build (the login window is its own binary, so build it too)
cargo build --profile quick -p heronite-epic-login && cargo run --profile quick -p heronite

# release build, installed for the current user (Linux)
scripts/install.sh

# installer and portable zip in dist\ (Windows, needs Inno Setup 6.3 or later)
powershell -ExecutionPolicy Bypass -File scripts\windows\package.ps1
```

</details>

## Security

The workflows are audited with [zizmor](https://github.com/zizmorcore/zizmor), actions are pinned to commits, dependencies are checked against the RustSec advisory database, and releases are signed with minisign for the in-app updater. Report problems privately, as [SECURITY.md](SECURITY.md) explains.

## License

Heronited is a fork of [Heronite](https://github.com/RafaelGoulartB/Heronite), Copyright (C) 2026 Rafael Goulart.

Licensed under the [GNU General Public License v3.0](LICENSE). Third-party components keep their own licenses; see [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

## Contact

- **Bugs and ideas:** [Issues](https://github.com/Sudo-Ivan/Heronited/issues)
- **Contributing:** start with [CONTRIBUTING.md](CONTRIBUTING.md) and the [architecture notes](docs/ARCHITECTURE.md)
- **Security problems:** report them privately, as [SECURITY.md](SECURITY.md) explains
