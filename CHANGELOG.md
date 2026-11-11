# Changelog

What changed in each release of fnstrip. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/): while fnstrip is 0.x, a new minor version (0.2.0)
may add settings, and a patch version (0.1.1) only fixes things.

**Your config:** existing `config.toml` files keep working across releases. Settings are only
ever added; if one is ever renamed, the old name keeps working for a while, and the release
notes here say so.

## [0.1.0] - 2026-11-11

The first public release: a customizable Touch Bar renderer for T1 MacBook Pros (2016-2017)
running Linux with [T1Bridge](https://github.com/standardagents/t1bridge) v0.1.12. Beta:
tested on one machine, a 15" 2016 MacBookPro13,3.

### The bar

- **Controls row:** Esc, sliders for display brightness, keyboard backlight and volume that
  follow your finger, and media keys. Tap the speaker icon to mute, the keyboard icon to turn
  the backlight off and back on.
- **F-keys** while Fn is held, or as the default row with `default_row = "function"`.
- **Extras row** (double-tap Fn; it stays until the next Fn press): up to 10 of your own
  buttons.
  - **Toggles** (`camera`, `mic`, `vpn`) run any shell command and light up while a status
    command says they're on. A lit camera shows its picture, a lit mic a spectrum, a lit VPN
    its traffic.
  - **Meters** (`cpu`, `mem`, `disk`, `net`, `battery`, `temp`, `fan`): live graphs and values.
    They sample in the background, so the row opens on full graphs.
- **Play/pause** lights up while a media player plays, with a spectrum of what you hear.
- **Startup animation:** beams, a flash and the fnstrip logo. A touch or Fn skips it;
  `startup_animation = "off"` turns it off.
- **Themes:** `theme = "default"`, `"mono"`, `"pastel"` or `"rainbow"`, across the whole bar,
  meters included. A `[colors]` section changes any single colour.
- **Temperature** in °C or °F: `temperature_unit = "f"`.
- **Debug gestures** (`debug_gestures = "on"`, off by default): triple-tap Fn and hold for a
  test screen with frame-rate stats; quad-tap Fn for the next theme until restart.

### Safety and footprint

- Runs as you, as an ordinary process, through T1Bridge's documented renderer link: no root,
  no `unsafe` code. Touch ID stays entirely T1Bridge's.
- If fnstrip fails to start or exits, T1Bridge's own bar comes back.
- A mistake in the config costs only its own line: it's logged with its line number and
  skipped (a bad value keeps that setting's default, a broken button is left out), and the
  rest applies. So a config written for a newer fnstrip still mostly works on this one.
- On/off settings take `"on"`/`"off"` or `true`/`false`.
- About 2% of one CPU core with the screen on and nothing playing (measured on the tested
  machine).

### Works with any VPN

The VPN button reads no output from your VPN: `command` toggles it, `status` says whether it's
up (exit code 0 = lit), and `interface` names its network interface for the traffic graph.
`config/default.toml` has WireGuard, NetworkManager and Tailscale examples.

### Also in the repository

- `contrib/desktop-provider`: a small T1Bridge desktop provider for volume, mute and media.
- `contrib/t1-webcam`: the webcam service behind the camera button's picture.
- `docs/`: the project page, with an interactive bar and a config builder.

### Known limits

- Tested on one model so far. Reports from the MacBookPro13,2, 14,2 and 14,3 are welcome.
- Extras buttons use the built-in icons; icons from files may come later.
- fnstrip carries a modified copy of T1Bridge's renderer, so T1Bridge fixes to it need merging
  by hand.

[0.1.0]: https://github.com/acmccubbin/fnstrip/releases/tag/v0.1.0
