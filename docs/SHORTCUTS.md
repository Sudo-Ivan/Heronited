# Global shortcuts on any desktop

The overlay, the replay, the quick launcher and the screenshot shortcut can all be triggered from outside Heronited, so they work on any Linux desktop: sxhkd, i3, bspwm, dwm, sway, niri, river, KDE, GNOME.

## Commands a key daemon can bind

| Command | URL form | What it does |
| --- | --- | --- |
| `heronite-rs --overlay` | `heronite://overlay` | open or close the in-game overlay |
| `heronite-rs --replay` | `heronite://replay` | save the replay of the running game |
| `heronite-rs --quick-launch` | `heronite://quick-launch` | open or close the quick launcher |
| `heronite-rs --screenshot` | `heronite://screenshot` | capture the running game |

The overlay, replay and screenshot commands act in the running instance. With no instance running they do nothing (there is no game). The quick launcher starts Heronited without its window when it is not running. In a Flatpak the command is `flatpak run io.github.sudoivan.Heronited --overlay` (and likewise for the others). In an AppImage use the AppImage file's path. Settings shows the exact command with a copy button (Overlay, Replay, Quick launcher, Screenshots).

## How the shortcut fields register

When you set a shortcut in Settings, Heronited tries, in order:

1. a temporary Hyprland bind (on Hyprland),
2. a native X11 grab on bare X11 sessions (i3, bspwm, dwm and similar),
3. the GlobalShortcuts portal (KDE, GNOME),
4. nothing: bind the command yourself as below.

With a native X11 grab, only one program can hold a key combination. If sxhkd and Heronited grab the same key, the second one gets the error. Either bind the key in sxhkd to the command, or free it there.

## Examples

sxhkd (`~/.config/sxhkd/sxhkdrc`), also the way for bspwm:

```
super + g
    heronite-rs --overlay

super + shift + r
    heronite-rs --replay
```

i3 (`~/.config/i3/config`):

```
bindsym $mod+g exec heronite-rs --overlay
```

sway (`~/.config/sway/config`):

```
bindsym $mod+g exec heronite-rs --overlay
```

Hyprland (`~/.config/hypr/hyprland.conf`): not needed for the overlay itself, since Heronited installs its own temporary bind. For a key of your own:

```
bind = SUPER SHIFT, R, exec, heronite-rs --replay
```

niri (`~/.config/niri/config.kdl`):

```
binds {
    Mod+G { spawn "heronite-rs" "--overlay"; }
}
```

river (`~/.config/river/init`):

```
riverctl map normal Super G spawn 'heronite-rs --overlay'
```

KDE: System Settings, Shortcuts, Custom Shortcuts. New, Global Shortcut, Command/URL. Set the trigger and the command `heronite-rs --overlay`.

GNOME: Settings, Keyboard, Keyboard Shortcuts, Custom Shortcuts. Add one with the command `heronite-rs --overlay` and press the keys.
