# TODO

Planned work for Heronited, roughly in the order I want to do it. Nothing here is promised, and the order can change.

## Mods

Mods already work per game with a fuse-overlayfs layer stack and Nexus Mods links (see [docs/MODS.md](docs/MODS.md)).

- [ ] Steam Workshop: browse, subscribe and install items for Steam games, mounted through the same mod layers
- [ ] Minecraft mods and modpacks (Modrinth and CurseForge), per instance
- [ ] Mod sources beyond Nexus Mods, behind one interface so more can be added

## Minecraft

- [ ] Minecraft as a game type with instances, Java runtime selection and version management
- [ ] Offline accounts (local username, no sign in)
- [ ] Official Microsoft account sign in
- [ ] Third-party auth servers: Ely.by, Drasl and similar Yggdrasil servers
- [ ] Import accounts and instances from Prism-style launchers such as FreesmLauncher

## Self-hosted game libraries

- [ ] LANCommander: sign in, list games, download and install
- [ ] GameVault: sign in, list games, download and install
- [ ] Gameyfin: sign in, list games, download and install
- [ ] RomM: sign in, list platforms and ROMs, fetch ROMs and hand them to the configured emulator, sync metadata and covers

## Emulation

- [ ] DOSBox (DOSBox-X and DOSBox Staging): per game configs, mount handling, launch from the Collection

## Sharing

- [x] Clip and screenshot sharing service: upload from the overlay or the capture list, get a link back, choose the server in Settings ([docs/SHARING.md](docs/SHARING.md))
- [x] Optional self-hosted target so nothing has to go through a third party

## Fork maintenance

- [ ] Keep picking useful upstream fixes from [Heronite](https://github.com/RafaelGoulartB/Heronite)
