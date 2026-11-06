# dimmer

A click-through black overlay for a display whose hardware backlight cannot go
any dimmer.

Some panels bottom out well above what a dark room wants: on this machine the
ARZOPA portable monitor reports brightness `0/100` as its minimum and there is
no `/sys/class/backlight` device at all — the backlight speaks DDC/CI, and
Plasma's idle dim multiplies an already-zero brightness, so it does nothing.
`dimmer` fills that gap: it draws a premultiplied black layer over everything on
the `zwlr_layer_shell_v1` overlay layer, with an empty input region, so pointer
and keyboard input pass straight through it.

Because the overlay is composited rather than drawn by the panel:

- the LED backlight is untouched, so the panel's power draw and its true-black
  bleed stay the same; contrast drops slightly as the overlay gets denser;
- colour is preserved — black scales R, G and B by the same factor, so hue,
  saturation and white balance do not change (unlike a colour-temperature
  filter);
- screenshots and screencasts of the session include the overlay.

## Usage

```text
dimmer               run the daemon (level from the saved state, $DIMMER_PERCENT or 70)
dimmer set <0-100>   set the amount of black laid over the screen
dimmer up [step]     reduce the overlay by <step> percent (default 5)
dimmer down [step]   raise the overlay by <step> percent (default 5)
dimmer get           print the current level
```

The daemon listens on `$XDG_RUNTIME_DIR/dimmer.sock` (mode `0600`), applies
changes without restarting, and saves the level to
`$XDG_CONFIG_HOME/dimmer/level` so it survives restarts.

Level to luminance: the overlay is alpha-blended in the encoded domain, so the
mean pixel value becomes `(100 - level)%` of the unblended value. `0` is off,
`70` is ~30% luminance, `88` is ~12%.

## Install

```sh
cargo build --release
install -Dm755 target/release/dimmer ~/.local/bin/dimmer
install -Dm644 contrib/dimmer.service ~/.config/systemd/user/dimmer.service
systemctl --user daemon-reload
systemctl --user enable --now dimmer.service
```

Optional global shortcuts: copy the two command-shortcut desktop files and let
kglobalaccel register them.

```sh
install -Dm644 contrib/net.local.dimmer-less.desktop ~/.local/share/applications/net.local.dimmer-less.desktop
install -Dm644 contrib/net.local.dimmer-more.desktop ~/.local/share/applications/net.local.dimmer-more.desktop
busctl --user call org.kde.kglobalaccel /kglobalaccel org.kde.KGlobalAccel \
  doRegister as 4 net.local.dimmer-less.desktop "$HOME/.local/bin/dimmer up 5" "" ""
busctl --user call org.kde.kglobalaccel /kglobalaccel org.kde.KGlobalAccel \
  doRegister as 4 net.local.dimmer-more.desktop "$HOME/.local/bin/dimmer down 5" "" ""
```

The registered command *is* the action's friendly name, and an already-assigned
shortcut stays bound to the command it was created with. Moving the binary
therefore means registering under a fresh component id (`…-less2.desktop`) and
re-assigning, not re-registering the same id — the old, keyless action would
otherwise linger next to it.

They appear in System Settings → Shortcuts → Command Shortcuts, where the keys
can be assigned (Meta+Alt+Up / Meta+Alt+Down are a reasonable default).
Assignment can also be done non-interactively; key sequences travel as a Qt key
code OR'd with the modifier bits, e.g. `Meta+Alt+Up` = `0x01000013 \| 0x10000000
\| 0x08000000` = `419430419`:

```sh
busctl --user call org.kde.kglobalaccel /kglobalaccel org.kde.KGlobalAccel \
  setForeignShortcut asai 4 net.local.dimmer-less.desktop _launch "Dim less (overlay -5%)" \
  "Dim less (overlay -5%)" 1 419430419
```

`DIMMER_PERCENT` in the unit only sets the starting level when no saved level
exists.

## How it works

`zwlr_layer_shell_v1` surface on the overlay layer, anchored to all four edges,
`exclusive_zone = -1`, `keyboard_interactivity = none`, and an empty
`wl_surface.set_input_region` so the compositor routes input to whatever is
underneath. The buffer is a `wl_shm` pool filled with premultiplied
`ARGB8888` black, repainted in place when the level changes.

Dependencies: `wayland-client`, `wayland-protocols-wlr`, `memmap2`, and `libc`
(for `memfd_create`/`ftruncate`).
