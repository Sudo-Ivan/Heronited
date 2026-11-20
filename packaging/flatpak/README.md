# Flatpak

`io.github.sudoivan.Heronited.yml` builds Heronited as a Flatpak on the GNOME runtime
(it has WebKitGTK 4.1 for the stores' login window), with the 32-bit
libraries Wine and Proton need (`org.freedesktop.Platform.Compat.i386` and
`GL32`), `vulkaninfo`, `7z` and `cabextract`. The build is offline:
`cargo-sources.json` lists every crate of `Cargo.lock` and is made by
[`scripts/flatpak.sh`](../../scripts/flatpak.sh), not kept in git.

## Build

```bash
scripts/flatpak.sh            # dist/Heronited-x86_64.flatpak
scripts/flatpak.sh --install  # and install it for this user
flatpak run io.github.sudoivan.Heronited
```

A bundle brings its runtime from Flathub but not the extensions the app
asks for, so Wine and Proton games also need the 32-bit libraries:

```bash
flatpak install --user flathub org.freedesktop.Platform.Compat.i386//26.08 org.freedesktop.Platform.GL32.default//26.08
```

The build needs `flatpak`, `python3` and `flatpak-builder` (or the
`org.flatpak.Builder` Flatpak); the GNOME SDK, Rust and LLVM extensions come
from Flathub. On Ubuntu, [`scripts/ci-flatpak-deps.sh`](../../scripts/ci-flatpak-deps.sh)
installs what CI uses. Every release run and every pull request that
touches the Flatpak build the bundle and keep it as a workflow artifact. It
is not attached to the releases: a bundle gets no updates (neither from
Flatpak nor from the app).

## What changes in the Flatpak

The app finds out it runs in a sandbox (`heronite_platform::flatpak`):

- Its data, settings and caches are in `~/.var/app/io.github.sudoivan.Heronited/`, apart
  from the AppImage's and the packages' `~/.local/share/heronite/`.
- Flatpak installs the updates: the in-app updater says so instead of
  installing them.
- The game sandbox (bubblewrap) cannot run inside Flatpak's sandbox: its
  settings are hidden and games run without it.
- Mods (`docs/MODS.md`) need to lay a game's mods over its folder with
  `fuse-overlayfs`, and a Flatpak cannot mount: "Mods" is not in a game's
  menu, Settings has no Mods section, the mod sites' links are not opened
  and games start without mods.
- Steam (starting and closing it) and Flatpak emulators start on the host
  through `flatpak-spawn --host`; Steam games open through `steam://` links.
- Notifications go through the Notification portal, not through
  `org.freedesktop.Notifications`.
- Game shortcuts, Steam shortcuts and the autostart entry start the app
  through the launcher Flatpak exports (`exports/bin/io.github.sudoivan.Heronited`), and
  go in the host's `~/.local/share/applications` and `~/.config/autostart`.
- MangoHud and gamescope are Flathub's Vulkan layers:
  `flatpak install flathub org.freedesktop.Platform.VulkanLayer.MangoHud//26.08 org.freedesktop.Platform.VulkanLayer.gamescope//26.08`.
- "Keep the computer awake" has no effect: it runs `systemd-inhibit`, which
  the runtime does not have (and the manifest does not give the app the
  system bus name it talks to).
- The data of an emulator installed as a Flatpak (`~/.var/app/<emulator>`)
  is out of reach, as the home folder leaves `~/.var/app` out: what Heronited
  reads there (save states, RetroAchievements settings) is not found for it.

- ReShade (game settings → Performance) writes into the game's folder: the
  DLL, `d3dcompiler_47.dll`, `reshade-shaders/`, `ReShade.ini` and the preset
  copy. A game in a folder outside the home, `/mnt`, `/media` and
  `/run/media` is out of the app's reach until
  `flatpak override --user --filesystem=<folder> io.github.sudoivan.Heronited`.
- GOG offline installers (settings → Downloads) live under
  `~/Games/GOG Offline Installers` by default, which the home access covers.
  The same override applies if you point the folder elsewhere.

Programs installed on the host itself (a native emulator, Ludusavi, the
system's Wine, gpu-screen-recorder) are not seen from inside the sandbox: use
their Flatpak or AppImage, or the AppImage of Heronited.

## AppStream

The screenshots of the AppStream data are in `packaging/linux/screenshots/`
and linked by the raw URL of the commit that added them, not of a branch:
they stay where they are even when the images change later. To change one,
add the new file under a new name and point the metainfo to the commit that
adds it.

To check the files the way Flathub's linter does, after `scripts/flatpak.sh`:

```bash
flatpak run --command=flatpak-builder-lint org.flatpak.Builder appstream packaging/linux/io.github.sudoivan.Heronited.metainfo.xml
flatpak run --command=flatpak-builder-lint org.flatpak.Builder repo target/flatpak/repo
```
