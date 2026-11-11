// fnstrip in the browser: the bar redrawn on a 2170x60 canvas (the panel's own size) with the
// renderer's icons (sprites.js, cut from its own frames by tools/make_demo_media.py), the
// same plate layout and colours, the Fn gestures, the extras row's live faces and the
// startup animation (a port of src/intro.rs). Meters show sample data, except the battery
// where the browser shares it; the camera and mic are real, and stay in this tab.
(() => {
  'use strict';

  const W = 2170;
  const H = 60;
  const S = window.SPRITES;

  const rgb = (r, g, b) => `rgb(${r},${g},${b})`;
  // ---- Themes (src/theme.rs: the same colours and maths) -----------------------------------

  const THEMES = {
    default: { background: [0, 0, 0], plate: [53, 53, 53], pressed: [76, 76, 76], label: [242, 242, 242], dim: [150, 150, 150],
      lit: [32, 128, 64], litPressed: [58, 164, 92], fill: [110, 110, 110], muted: [72, 72, 72], hues: 'plain' },
    mono: { background: [0, 0, 0], plate: [26, 26, 26], pressed: [70, 70, 70], label: [255, 255, 255], dim: [110, 110, 110],
      lit: [118, 118, 118], litPressed: [150, 150, 150], fill: [88, 88, 88], muted: [44, 44, 44], hues: 'grey' },
    pastel: { background: [26, 24, 32], plate: [205, 205, 205], pressed: [236, 236, 236], label: [46, 40, 60], dim: [120, 112, 132],
      lit: [96, 178, 136], litPressed: [128, 200, 162], fill: [160, 160, 160], muted: [190, 190, 190], hues: 'pastel' },
    rainbow: { background: [0, 0, 0], plate: [66, 66, 66], pressed: [100, 100, 100], label: [242, 242, 242], dim: [150, 150, 150],
      lit: [225, 30, 120], litPressed: [245, 80, 155], fill: [108, 108, 108], muted: [80, 80, 80], hues: 'rainbow' },
  };
  let theme = THEMES.default;
  let fahrenheit = false; // the builder's temperature setting
  let themeName = 'default';
  const COLOR = {};
  const LIVE_LABEL = rgb(242, 242, 242); // icons over live faces keep their light colour
  const same = (c) => c;
  let tint = same; // how the live face being drawn is recoloured (Theme::face_tint)

  // The builder's [colors] overrides, by config key, as [r, g, b]; on top of any theme.
  const COLOR_KEYS = {
    background: 'background', plate: 'plate', pressed: 'pressed', label: 'label', dim: 'dim',
    lit: 'lit', lit_pressed: 'litPressed', fill: 'fill', muted: 'muted',
  };
  const overrides = {};

  /** Theme `name`, with the builder's colour overrides unless `plain` (the gallery). */
  function applyTheme(name, plain = false) {
    themeName = THEMES[name] ? name : 'default';
    theme = { ...THEMES[themeName] };
    if (!plain) {
      for (const [key, value] of Object.entries(overrides)) theme[COLOR_KEYS[key]] = value;
    }
    const c = (v) => rgb(v[0], v[1], v[2]);
    Object.assign(COLOR, {
      background: c(theme.background), plate: c(theme.plate), pressed: c(theme.pressed),
      on: c(theme.lit), onPressed: c(theme.litPressed), fill: c(theme.fill), mutedFill: c(theme.muted),
      label: c(theme.label), dim: c(theme.dim),
    });
  }
  applyTheme('default');

  // src/theme.rs, in JS: the same colours and maths.
  const luma = (c) => 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
  const toByte = (v) => Math.round(Math.min(1, Math.max(0, v)) * 255);
  function fromHue(hue, chroma, offset) {
    const sector = ((hue / 60) % 6 + 6) % 6;
    const second = chroma * (1 - Math.abs((sector % 2) - 1));
    const [r, g, b] = sector < 1 ? [chroma, second, 0] : sector < 2 ? [second, chroma, 0] : sector < 3 ? [0, chroma, second]
      : sector < 4 ? [0, second, chroma] : sector < 5 ? [second, 0, chroma] : [chroma, 0, second];
    return [toByte(r + offset), toByte(g + offset), toByte(b + offset)];
  }
  const hsv = (h, s, v) => fromHue(h, v * s, v - v * s);
  function hsl(h, s, l) {
    const chroma = (1 - Math.abs(2 * l - 1)) * s;
    return fromHue(h, chroma, l - chroma / 2);
  }
  function toHsv(c) {
    const [r, g, b] = c.map((v) => v / 255);
    const max = Math.max(r, g, b);
    const chroma = max - Math.min(r, g, b);
    let hue = 0;
    if (chroma > 0) {
      if (max === r) hue = 60 * ((((g - b) / chroma) % 6) + 6) % 360;
      else if (max === g) hue = 60 * ((b - r) / chroma + 2);
      else hue = 60 * ((r - g) / chroma + 4);
    }
    return [hue, max > 0 ? chroma / max : 0, max];
  }
  const pastelHue = (plateX) => (Math.floor(plateX / 95) * 67) % 360;
  const rainbowHue = (plateX) => (plateX / W) * 300;
  /** Theme::plate_colour: surface colour `base` on the plate starting at plateX. */
  function plateColour(base, plateX) {
    const level = luma(base) / 255;
    if (theme.hues === 'pastel') return hsl(pastelHue(plateX), 0.6, level);
    if (theme.hues === 'rainbow') return hsv(rainbowHue(plateX), 0.72, Math.min(1, level * 1.9));
    return base;
  }
  /** Theme::face_tint: how the live face on the plate starting at plateX is recoloured. */
  function faceTint(plateX) {
    const rehue = (hue, scale) => (c) => {
      const [, s, v] = toHsv(c);
      return hsv(hue, Math.min(1, s * scale), v);
    };
    switch (theme.hues) {
      case 'grey': return (c) => { const v = toByte(luma(c) / 255); return [v, v, v]; };
      case 'pastel': return rehue(pastelHue(plateX), 0.5);
      case 'rainbow': return rehue(rainbowHue(plateX), 1);
      default: return same;
    }
  }
  /** A plate (or the left `width` px of one, for a slider fill) in theme surface `base`. */
  function surface(x, w, base, width = w) {
    if (width <= 0) return;
    const c = plateColour(base, x);
    clipPlate(x, w);
    ctx.fillStyle = rgb(c[0], c[1], c[2]);
    ctx.fillRect(x, 0, width, H);
    ctx.restore();
  }

  const RADIUS = 8;
  const SLIDER_ICON = 72;
  const SLIDER_TEXT_MARGIN = 16;
  const LIVE_ICON = 64;
  const VALUE_FONT = 'Inter, system-ui, sans-serif';

  // ---- Layout (from the renderer's StockBar) ----------------------------------------------

  const ESC = { id: 'esc', x: 0, w: 162, icon: 'esc', say: () => 'Esc' };
  const CONTROLS = [
    ESC,
    { id: 'brightness', x: 194, w: 372, slider: true, icon: 'brightness' },
    { id: 'keyboard', x: 602, w: 372, slider: true, icon: 'keyboard' },
    { id: 'previous', x: 1010, w: 182, icon: 'previous', say: () => 'previous track <span class="cmd">(MPRIS, via the desktop provider)</span>' },
    { id: 'playpause', x: 1200, w: 182, icon: 'playpause', say: () => 'play/pause <span class="cmd">(MPRIS, via the desktop provider)</span>' },
    { id: 'next', x: 1390, w: 182, icon: 'next', say: () => 'next track <span class="cmd">(MPRIS, via the desktop provider)</span>' },
    { id: 'volume', x: 1608, w: 562, slider: true, icon: 'volume' },
  ];
  const FUNCTION = [ESC].concat(Array.from({ length: 12 }, (_, i) => {
    const x = 172 + 167 * i;
    return { id: `F${i + 1}`, x, w: Math.min(157, W - x), icon: `F${i + 1}`, say: () => `F${i + 1}` };
  }));
  // The extras row: toggles fill from the left after Esc, meters from the right end, each
  // in config order (StockBar::extra_controls).
  const EXTRA_LEFT = 194;
  const EXTRA_PITCH = 190;
  const EXTRA_WIDTH = 182;
  const MAX_EXTRAS = 10;
  const EXTRA_INFO = {
    camera: {
      kind: 'toggle', title: 'Camera',
      what: 'Starts and stops your webcam service. Lit, the plate shows the live picture.',
      command: 'webcam toggle', status: 'systemctl --user --quiet is-active t1-webcam.service',
    },
    mic: {
      kind: 'toggle', title: 'Mic',
      what: 'Mutes and unmutes your mic. Lit while it\'s live, with a spectrum of what it hears.',
      command: 'wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle',
      status: '! wpctl get-volume @DEFAULT_AUDIO_SOURCE@ | grep -q MUTED',
    },
    vpn: {
      kind: 'toggle', title: 'VPN',
      what: 'Runs your VPN up or down. Lit while the tunnel is up, with its traffic.',
      command: 'my-vpn-toggle', status: 'test -e /sys/class/net/wg0',
    },
    cpu: { kind: 'meter', title: 'CPU', what: 'User (bottom) and system time, and the total.', short: 'user and system time' },
    temp: { kind: 'meter', title: 'Temperature', what: 'The CPU package in °C or °F; red within 20 °C of critical.', short: 'CPU package temperature' },
    fan: { kind: 'meter', title: 'Fans', what: 'Both fans\' speeds, as two lines on one rpm scale.', short: 'both fans, rpm' },
    mem: { kind: 'meter', title: 'Memory', what: 'Memory in use (bottom) and page cache.', short: 'in use and page cache' },
    disk: { kind: 'meter', title: 'Disk', what: 'Bytes read and written per second, scaled to the recent peak.', short: 'read and write rates' },
    net: { kind: 'meter', title: 'Network', what: 'Bytes received and sent per second, scaled to the recent peak.', short: 'receive and send rates' },
    battery: {
      kind: 'meter', title: 'Battery',
      what: 'Fills to the charge: green with a bolt on power, amber at 20%, red at 10%, with the time to empty or full.',
      short: 'green with a bolt on power',
    },
  };
  // Ready-made commands for the toggles (config/default.toml has the same examples). Each
  // toggle starts from its first; "do not disturb" ones fit any toggle icon.
  const DO_NOT_DISTURB = [
    { name: 'Do not disturb (mako)', command: 'makoctl mode -t do-not-disturb', status: 'makoctl mode | grep -q do-not-disturb' },
    { name: 'Do not disturb (dunst)', command: 'dunstctl set-paused toggle', status: 'dunstctl is-paused | grep -q true' },
    { name: 'Do not disturb (SwayNC)', command: 'swaync-client -d', status: 'swaync-client -D | grep -q true' },
  ];
  const PRESETS = {
    camera: [
      { name: 'fnstrip webcam service', command: 'webcam toggle', status: 'systemctl --user --quiet is-active t1-webcam.service',
        hint: 'The webcam service and its script are in contrib/t1-webcam; they also give the lit button its picture.' },
    ],
    mic: [
      { name: 'Mic mute (PipeWire)', command: 'wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle', status: '! wpctl get-volume @DEFAULT_AUDIO_SOURCE@ | grep -q MUTED' },
      { name: 'Mic mute (PulseAudio)', command: 'pactl set-source-mute @DEFAULT_SOURCE@ toggle', status: "LC_ALL=C pactl get-source-mute @DEFAULT_SOURCE@ | grep -q 'Mute: no'" },
    ],
    vpn: [
      { name: 'WireGuard (wg-quick)', command: 'if [ -e /sys/class/net/wg0 ]; then wg-quick down wg0; else wg-quick up wg0; fi', status: 'test -e /sys/class/net/wg0', interface: 'wg0',
        hint: 'wg-quick needs root, so this works only if you can run it without a password; NetworkManager can bring WireGuard up and down as you.' },
      { name: 'NetworkManager connection', command: 'if nmcli -t -f NAME connection show --active | grep -qx Work; then nmcli connection down Work; else nmcli connection up Work; fi',
        status: 'nmcli -t -f NAME connection show --active | grep -qx Work', interface: 'tun0',
        hint: 'Change Work to your connection\'s name (nmcli connection show lists them). OpenVPN uses tun0; a WireGuard connection uses its own name.' },
      { name: 'Tailscale', command: 'tailscale status >/dev/null && tailscale down || tailscale up', status: 'tailscale status >/dev/null', interface: 'tailscale0' },
    ],
  };
  const presetsFor = (name) => (PRESETS[name] || []).concat(DO_NOT_DISTURB);
  // What each toggle runs: starts as its first preset, then whatever you type.
  const custom = Object.fromEntries(Object.keys(PRESETS).map((name) => [name, { command: PRESETS[name][0].command, status: PRESETS[name][0].status }]));
  // The network interface the VPN's traffic graph and the net meter follow ('' = net: every
  // network device that's up). fnstrip's own default for the VPN is wg0.
  custom.vpn.interface = PRESETS.vpn[0].interface;
  custom.net = { interface: '' };
  /** Linux interface names: up to 15 characters, no spaces, slashes or colons. */
  const validInterface = (name) => /^[^\s/:]{1,15}$/.test(name);

  // The builder starts from the built-in default (config/default.toml), so its download
  // works as-is; the hero bar shows every button until the builder is used.
  let chosen = ['mic', 'cpu', 'mem', 'disk', 'net', 'battery'];
  let builderUsed = false;
  let EXTRAS = [];
  function layoutExtras() {
    EXTRAS = extrasRow(chosen);
  }
  /** The extras row for these buttons: toggles from the left after Esc, meters from the right. */
  function extrasRow(names) {
    const meterCount = names.filter((name) => EXTRA_INFO[name].kind === 'meter').length;
    let meterX = W - meterCount * EXTRA_PITCH + (EXTRA_PITCH - EXTRA_WIDTH);
    let toggleX = EXTRA_LEFT;
    return [ESC].concat(names.map((name) => {
      const meter = EXTRA_INFO[name].kind === 'meter';
      const x = meter ? meterX : toggleX;
      if (meter) meterX += EXTRA_PITCH;
      else toggleX += EXTRA_PITCH;
      return { id: name, x, w: EXTRA_WIDTH, icon: name, extra: EXTRA_INFO[name] };
    }));
  }
  layoutExtras();
  const SHOWCASE = extrasRow(Object.keys(EXTRA_INFO));

  // ---- State ------------------------------------------------------------------------------

  const state = {
    levels: { brightness: 75, keyboard: 40, volume: 45 },
    muted: false,
    lit: { camera: false, mic: false, vpn: false },
    extras: false,
    fnDown: false,
    pressed: null,
  };
  const layout = () => (state.extras ? (builderUsed ? EXTRAS : SHOWCASE) : state.fnDown ? FUNCTION : CONTROLS);

  // ---- Canvas -----------------------------------------------------------------------------

  const display = document.getElementById('bar');
  const shown = display.getContext('2d');
  const frame = document.createElement('canvas');
  frame.width = W;
  frame.height = H;
  const barContext = frame.getContext('2d', { willReadFrequently: true });
  let ctx = barContext; // where the drawing functions draw (the bar, or a builder canvas)
  // Narrow screens show part of the bar at a finger-friendly size, panned along it.
  const NARROW_SCALE = 0.42; // CSS px per bar px
  let narrow = false;
  let view = W; // bar px on screen
  let pan = 0; // bar x at the left edge
  let panTarget = null;

  const sprites = new Image();
  sprites.src = window.SPRITE_SHEET;
  const tinted = {}; // colour -> canvas of the sheet in that colour

  function sheet(color) {
    if (!tinted[color]) {
      const c = document.createElement('canvas');
      c.width = sprites.width;
      c.height = sprites.height;
      const g = c.getContext('2d');
      g.drawImage(sprites, 0, 0);
      g.globalCompositeOperation = 'source-in';
      g.fillStyle = color;
      g.fillRect(0, 0, c.width, c.height);
      tinted[color] = c;
    }
    return tinted[color];
  }

  /** An icon at its place in the plate (as the renderer draws it), or at x, y if given. */
  function icon(name, plateX, color = COLOR.label, at = null) {
    const s = S[name];
    if (!s || !sprites.complete) return;
    const x = at ? at[0] : plateX + s.dx;
    const y = at ? at[1] : s.dy;
    ctx.drawImage(sheet(color), s.x, 0, s.w, s.h, Math.round(x), Math.round(y), s.w, s.h);
  }

  function roundRect(x, w, color) {
    ctx.fillStyle = color;
    ctx.beginPath();
    ctx.roundRect(x, 0, w, H, RADIUS);
    ctx.fill();
  }

  function clipPlate(x, w) {
    ctx.save();
    ctx.beginPath();
    ctx.roundRect(x, 0, w, H, RADIUS);
    ctx.clip();
  }

  // ---- Drawing the rows -------------------------------------------------------------------

  function drawSlider(p) {
    const level = state.levels[p.id];
    const muted = (p.id === 'volume' && state.muted) || (p.id === 'keyboard' && level === 0);
    surface(p.x, p.w, theme.plate);
    surface(p.x, p.w, muted ? theme.muted : theme.fill, Math.round((p.w * level) / 100));
    const label = muted ? COLOR.dim : COLOR.label;
    icon(muted && p.id === 'volume' ? 'muted' : p.icon, p.x, label);
    ctx.fillStyle = label;
    ctx.font = `500 24px ${VALUE_FONT}`;
    ctx.letterSpacing = '2px';
    ctx.textAlign = 'right';
    ctx.textBaseline = 'middle';
    ctx.fillText(`${level}%`, p.x + p.w - SLIDER_TEXT_MARGIN + 2, H / 2 + 1);
    ctx.letterSpacing = '0px';
  }

  function drawKey(p) {
    const pressed = state.pressed === p.id;
    if (p.id === 'playpause' && music.playing) {
      // Lit while playing, with the speakers' spectrum under the icon.
      clipPlate(p.x, p.w);
      tint = faceTint(p.x);
      music.spectrum.draw(p.x, p.w);
      tint = same;
      if (pressed) {
        ctx.fillStyle = 'rgba(242, 242, 242, 0.25)';
        ctx.fillRect(p.x, 0, p.w, H);
      }
      ctx.restore();
      icon(p.icon, p.x, LIVE_LABEL);
      return;
    }
    surface(p.x, p.w, pressed ? theme.pressed : theme.plate);
    icon(p.icon, p.x);
  }

  function drawExtra(p, showLit = false) {
    tint = faceTint(p.x);
    try {
      drawExtraFace(p, showLit);
    } finally {
      tint = same;
    }
  }

  function drawExtraFace(p, showLit) {
    const lit = state.lit[p.id] || showLit;
    const pressed = state.pressed === p.id;
    const meter = meters[p.id];
    if (p.id === 'camera' && lit && camera.ready) {
      clipPlate(p.x, p.w);
      camera.draw(p.x, p.w);
      ctx.restore();
      icon('camera-on', p.x, LIVE_LABEL);
    } else if (p.id === 'mic' && lit && mic.ready) {
      clipPlate(p.x, p.w);
      mic.draw(p.x, p.w);
      ctx.restore();
      icon('mic-on', p.x, LIVE_LABEL);
    } else if (meter && (p.id !== 'vpn' || lit)) {
      clipPlate(p.x, p.w);
      meter.draw(p.x, p.w);
      ctx.restore();
      const name = p.id === 'vpn' ? 'vpn-on' : p.id === 'battery' && meter.charging ? 'battery-on' : p.id;
      const s = S[name];
      icon(name, p.x, LIVE_LABEL, [p.x + (LIVE_ICON - s.w) / 2, (H - s.h) / 2]);
    } else {
      if (lit) roundRect(p.x, p.w, pressed ? COLOR.onPressed : COLOR.on);
      else surface(p.x, p.w, pressed ? theme.pressed : theme.plate);
      icon(lit ? `${p.id}-on` : p.id, p.x);
    }
  }

  function drawRow(plates = layout()) {
    ctx.fillStyle = COLOR.background;
    ctx.fillRect(0, 0, W, H);
    for (const p of plates) {
      if (p.slider) drawSlider(p);
      else if (p.extra) drawExtra(p);
      else drawKey(p);
    }
  }

  // ---- Meters (src/meters.rs, with sample data) ---------------------------------------------

  const COLUMN = 2;
  const PALETTE = {
    cpu: [[16, 30, 54], [70, 150, 235], [235, 110, 90]],
    mem: [[38, 22, 52], [180, 110, 240], [100, 70, 130]],
    disk: [[50, 36, 12], [240, 190, 70], [240, 110, 60]],
    net: [[12, 40, 46], [70, 215, 220], [235, 140, 200]],
    vpn: [[16, 56, 30], [100, 220, 130], [225, 225, 120]],
    fan: [[14, 34, 50], [80, 180, 240], [160, 125, 245]],
    temp: [[48, 26, 14], [240, 150, 60], [240, 60, 50]],
    battery: [[30, 33, 38], [125, 133, 146], [125, 133, 146]],
    batteryPowered: [[16, 56, 30], [100, 220, 130], [100, 220, 130]],
    batteryLow: [[56, 40, 10], [240, 175, 50], [240, 175, 50]],
    batteryCritical: [[60, 16, 14], [235, 75, 60], [235, 75, 60]],
  };
  const css = (c) => rgb(c[0], c[1], c[2]);

  function formatRate(bytes) {
    let value = Math.max(0, bytes);
    let unit = 0;
    while (Math.round(value) >= 1000 && unit < 3) {
      value /= 1000;
      unit += 1;
    }
    const suffix = ['B', 'kB', 'MB', 'GB'][unit];
    if (unit === 0 || value >= 99.95) return `${value.toFixed(0)}${suffix}`;
    if (value >= 9.995) return `${value.toFixed(1)}${suffix}`;
    return `${value.toFixed(2)}${suffix}`;
  }
  const formatHours = (hours) => {
    const minutes = Math.round(Math.max(0, hours) * 60);
    return `${Math.floor(minutes / 60)}:${String(minutes % 60).padStart(2, '0')}`;
  };

  class Meter {
    constructor(kind) {
      this.kind = kind;
      this.history = []; // [first, second], oldest first
      this.capacity = 91;
      this.scale = kind === 'cpu' || kind === 'mem' ? 100 : 1;
      this.charging = true;
      this.level = 0.68;
      this.hours = 0.75;
    }

    push(first, second) {
      this.history.push([first, second]);
      if (this.history.length > this.capacity) this.history.shift();
      if (this.kind === 'disk' || this.kind === 'net' || this.kind === 'vpn') {
        const floor = this.kind === 'disk' ? 256 * 1024 : 32 * 1024;
        const peak = Math.max(floor, ...this.history.map(([a, b]) => a + b));
        this.scale = peak > this.scale ? peak : this.scale * 0.96 + peak * 0.04;
      }
    }

    mean(part) {
      const recent = this.history.slice(-4);
      return recent.reduce((sum, s) => sum + s[part], 0) / Math.max(1, recent.length);
    }

    palette() {
      return this.ownPalette().map(tint);
    }

    ownPalette() {
      if (this.kind !== 'battery') return PALETTE[this.kind];
      if (this.charging) return PALETTE.batteryPowered;
      if (this.level <= 0.1) return PALETTE.batteryCritical;
      if (this.level <= 0.2) return PALETTE.batteryLow;
      return PALETTE.battery;
    }

    draw(x, width) {
      const [background, lower, upper] = this.palette();
      ctx.fillStyle = css(background);
      ctx.fillRect(x, 0, width, H);
      if (this.kind === 'fan') this.drawLines(x, width, lower, upper);
      else this.drawGraph(x, width, lower, upper);
      this.drawValue(x, width, background, lower, upper);
    }

    normalized(sample) {
      if (this.kind === 'temp') {
        const hot = 80;
        return [Math.min(sample[0], hot) / 100, Math.max(0, sample[0] - hot) / 100];
      }
      const first = Math.min(1, Math.max(0, sample[0] / this.scale));
      const second = Math.min(1 - first, Math.max(0, sample[1] / this.scale));
      return [first, second];
    }

    drawGraph(x, width, lower, upper) {
      const n = this.history.length;
      for (let column = 0; column < n; column += 1) {
        const [first, second] = this.normalized(this.history[n - 1 - column]);
        const right = x + width - column * COLUMN;
        const low = Math.round(first * H);
        const high = Math.round((first + second) * H);
        ctx.fillStyle = css(lower);
        ctx.fillRect(right - COLUMN, H - low, COLUMN, low);
        ctx.fillStyle = css(upper);
        ctx.fillRect(right - COLUMN, H - high, COLUMN, high - low);
      }
    }

    drawLines(x, width, right, left) {
      const low = 2000 * 0.9;
      const high = 5927;
      const rowOf = (rpm) => Math.round((1 - Math.min(1, Math.max(0, (rpm - low) / (high - low)))) * (H - 2));
      for (const [part, color] of [[1, right], [0, left]]) {
        ctx.fillStyle = css(color);
        let previous = null;
        const skipped = this.capacity - this.history.length;
        this.history.forEach((sample, column) => {
          const y = rowOf(sample[part]);
          const top = previous === null ? y : Math.min(previous, y);
          const bottom = previous === null ? y : Math.max(previous, y);
          previous = y;
          const left = x + (skipped + column) * COLUMN - (this.capacity * COLUMN - width);
          ctx.fillRect(left, top, COLUMN, bottom + 2 - top);
        });
      }
    }

    lines(lower, upper) {
      const white = [255, 255, 255];
      switch (this.kind) {
        case 'cpu': return [[`${Math.round(this.mean(0) + this.mean(1))}%`, white]];
        case 'mem': return [[`${Math.round(this.mean(0))}%`, white]];
        case 'temp': {
          const celsius = this.mean(0);
          return [[fahrenheit ? `${Math.round(celsius * 1.8 + 32)}°F` : `${Math.round(celsius)}°C`, white]];
        }
        case 'fan': return [[`${Math.round(this.mean(0))}`, upper], [`${Math.round(this.mean(1))}`, lower]];
        case 'battery': return [[`${Math.round(this.level * 100)}%`, white], [formatHours(this.hours), lower]];
        default: return [[formatRate(this.mean(1)), upper], [formatRate(this.mean(0)), lower]];
      }
    }

    drawValue(x, width, background, lower, upper) {
      if (!this.history.length) return;
      const lines = this.lines(lower, upper);
      const size = lines.length === 1 ? 24 : 18;
      const lineHeight = lines.length === 1 ? 17 : 13;
      const gap = 6;
      const block = lines.length * lineHeight + (lines.length - 1) * gap;
      let top = (H - block) / 2;
      ctx.font = `500 ${size}px ${VALUE_FONT}`;
      ctx.letterSpacing = lines.length === 1 ? '2px' : '1px';
      ctx.textAlign = 'right';
      ctx.textBaseline = 'alphabetic';
      ctx.lineJoin = 'round';
      for (const [text, color] of lines) {
        const baseline = top + lineHeight;
        ctx.lineWidth = 4;
        ctx.strokeStyle = css(background);
        ctx.strokeText(text, x + width - 8 + 2, baseline);
        ctx.fillStyle = css(color);
        ctx.fillText(text, x + width - 8 + 2, baseline);
        top += lineHeight + gap;
      }
      ctx.letterSpacing = '0px';
    }
  }

  const meters = {};
  for (const kind of ['vpn', 'cpu', 'temp', 'fan', 'mem', 'disk', 'net', 'battery']) {
    meters[kind] = new Meter(kind);
  }

  // Sample data: smooth random walks, with the CPU (and so temperature and fans) busier
  // while you play with the page.
  const sim = { busy: 0, load: 0.1, temp: 50, fans: [2400, 2250], mem: 41, disk: 0, net: 60e3, vpn: 20e3 };
  const real = { battery: null };
  const jitter = (amount) => (Math.random() - 0.5) * 2 * amount;

  function sample() {
    sim.busy *= 0.9;
    const target = 0.07 + 0.55 * Math.min(1, sim.busy) + (Math.random() < 0.06 ? 0.3 * Math.random() : 0);
    sim.load += (target - sim.load) * 0.35;
    const load = Math.max(0.02, Math.min(0.98, sim.load + jitter(0.03)));
    meters.cpu.push(load * 72, load * 28);
    sim.temp += (46 + load * 48 - sim.temp) * 0.08 + jitter(0.4);
    meters.temp.push(sim.temp, 0);
    const heat = Math.max(0, Math.min(1, (sim.temp - 48) / 40));
    sim.fans[0] += (2160 + heat * 3600 - sim.fans[0]) * 0.1 + jitter(25);
    sim.fans[1] += (2000 + heat * 3300 - sim.fans[1]) * 0.1 + jitter(25);
    meters.fan.push(sim.fans[0], sim.fans[1]);
    sim.mem += (41 + 6 * Math.min(1, sim.busy) - sim.mem) * 0.05 + jitter(0.15);
    meters.mem.push(sim.mem, 22 + jitter(0.3));
    const burst = Math.random() < 0.08;
    meters.disk.push(burst ? 2e6 * Math.random() : 15e3 * Math.random(),
      Math.random() < 0.12 ? 1.2e6 * Math.random() : 8e3 * Math.random());
    sim.net = Math.max(2e3, sim.net * (0.85 + 0.3 * Math.random()) + (Math.random() < 0.05 ? 400e3 * Math.random() : 0));
    meters.net.push(sim.net, sim.net * (0.15 + 0.2 * Math.random()));
    sim.vpn = Math.max(1e3, sim.vpn * (0.85 + 0.3 * Math.random()) + (Math.random() < 0.05 ? 90e3 * Math.random() : 0));
    meters.vpn.push(sim.vpn, sim.vpn * (0.4 + 0.4 * Math.random()));
    const battery = meters.battery;
    if (real.battery) {
      const b = real.battery;
      battery.level = b.level;
      battery.charging = b.charging;
      const seconds = b.charging ? b.chargingTime : b.dischargingTime;
      battery.hours = Number.isFinite(seconds) && seconds > 0 ? seconds / 3600 : battery.hours;
    } else {
      battery.level = Math.min(1, battery.level + 0.0004);
      battery.hours = Math.max(0, (1 - battery.level) * 2.3);
    }
    battery.push(battery.level * battery.scale, 0);
  }
  for (let i = 0; i < 91; i += 1) sample();
  setInterval(sample, 250);
  if (navigator.getBattery) {
    navigator.getBattery().then((b) => { real.battery = b; }).catch(() => {});
  }

  // ---- Camera and mic (real, local only) --------------------------------------------------

  const camera = {
    video: null,
    stream: null,
    ready: false,
    async start() {
      this.stream = await navigator.mediaDevices.getUserMedia({ video: { width: 640, height: 360 }, audio: false });
      this.video = document.createElement('video');
      this.video.muted = true;
      this.video.playsInline = true;
      this.video.srcObject = this.stream;
      await this.video.play();
      this.ready = true;
    },
    stop() {
      this.stream?.getTracks().forEach((t) => t.stop());
      this.stream = null;
      this.ready = false;
    },
    draw(x, width) {
      const v = this.video;
      const vw = v.videoWidth || 640;
      const vh = v.videoHeight || 360;
      // Cover the plate: the full width of the picture, a band through its middle.
      const bandHeight = (vw * H) / width;
      ctx.save();
      ctx.translate(x + width, 0);
      ctx.scale(-1, 1); // mirrored, like a selfie view
      if (theme.hues === 'grey') ctx.filter = 'grayscale(1)'; // mono: a grey picture too
      ctx.drawImage(v, 0, Math.max(0, (vh - bandHeight) / 2), vw, Math.min(vh, bandHeight), 0, 0, width, H);
      ctx.restore();
    },
  };

  const MIC = { background: [16, 56, 30], bottom: [58, 164, 92], top: [150, 235, 170], peak: [220, 255, 225] };
  const BANDS = 24;

  // 24 log-spaced bands from 80 Hz to 8 kHz with falling peaks, drawn like src/extras.rs
  // draw_mic_spectrum. Shared by the mic button and the playing play/pause button.
  class Spectrum {
    constructor() {
      this.levels = new Array(BANDS).fill(0);
      this.peaks = new Array(BANDS).fill(0);
    }
    update(analyser, sampleRate) {
      if (!this.data || this.data.length !== analyser.frequencyBinCount) {
        this.data = new Float32Array(analyser.frequencyBinCount);
      }
      analyser.getFloatFrequencyData(this.data);
      const nyquist = sampleRate / 2;
      const bins = this.data.length;
      for (let band = 0; band < BANDS; band += 1) {
        const from = 80 * (8000 / 80) ** (band / BANDS);
        const to = 80 * (8000 / 80) ** ((band + 1) / BANDS);
        let peak = -Infinity;
        for (let i = Math.floor((from / nyquist) * bins); i <= Math.ceil((to / nyquist) * bins); i += 1) {
          peak = Math.max(peak, this.data[i] ?? -Infinity);
        }
        const level = Math.min(1, Math.max(0, (peak + 95) / 65));
        this.levels[band] = Math.max(level, this.levels[band] * 0.82);
        this.peaks[band] = Math.max(this.levels[band], this.peaks[band] - 0.012);
      }
    }
    draw(x, width) {
      ctx.fillStyle = css(tint(MIC.background));
      ctx.fillRect(x, 0, width, H);
      const pitch = 7;
      const barWidth = 5;
      const first = x + (width - BANDS * pitch) / 2 + (pitch - barWidth) / 2;
      const gradient = ctx.createLinearGradient(0, H, 0, 0);
      gradient.addColorStop(0, css(tint(MIC.bottom)));
      gradient.addColorStop(1, css(tint(MIC.top)));
      for (let bar = 0; bar < BANDS; bar += 1) {
        const left = first + bar * pitch;
        const height = Math.round(this.levels[bar] * H);
        ctx.fillStyle = gradient;
        ctx.fillRect(left, H - height, barWidth, height);
        if (this.peaks[bar] > 0.02) {
          ctx.fillStyle = css(tint(MIC.peak));
          ctx.fillRect(left, H - Math.round(this.peaks[bar] * H) - 1, barWidth, 1);
        }
      }
    }
  }

  const mic = {
    stream: null,
    context: null,
    analyser: null,
    ready: false,
    spectrum: new Spectrum(),
    async start() {
      this.stream = await navigator.mediaDevices.getUserMedia({ audio: true, video: false });
      this.context = new AudioContext();
      const source = this.context.createMediaStreamSource(this.stream);
      this.analyser = this.context.createAnalyser();
      this.analyser.fftSize = 2048;
      this.analyser.smoothingTimeConstant = 0.5;
      source.connect(this.analyser);
      this.ready = true;
    },
    stop() {
      this.stream?.getTracks().forEach((t) => t.stop());
      this.context?.close();
      this.stream = null;
      this.ready = false;
    },
    update() {
      if (this.ready) this.spectrum.update(this.analyser, this.context.sampleRate);
    },
    draw(x, width) {
      this.spectrum.draw(x, width);
    },
  };

  // ---- Music for the play/pause button ------------------------------------------------------
  // The real bar lights play/pause green while a media player plays and shows a spectrum of
  // what the speakers play. Here the "player" is a small synth made in the browser (no
  // stream, nothing downloaded): three loops, so previous and next do something too.

  const NOTE = (n) => 440 * 2 ** ((n - 69) / 12);
  const TRACKS = [
    // bpm, bass line (MIDI, 0 = rest) per 16th, chord per bar, arp pattern (chord index)
    { name: 'Night Drive', bpm: 104, swing: 0,
      bass: [36, 0, 36, 0, 0, 36, 0, 43, 36, 0, 36, 0, 0, 39, 0, 41],
      chords: [[60, 63, 67, 70], [56, 60, 63, 67], [58, 62, 65, 69], [55, 58, 62, 65]],
      roots: [0, -4, -2, -5], arp: [0, 1, 2, 3, 2, 1, 0, 2], kick: [0, 6, 8, 11], hats: 2 },
    { name: 'Glass Steps', bpm: 122, swing: 0.12,
      bass: [41, 0, 0, 41, 0, 0, 41, 0, 44, 0, 0, 44, 0, 46, 0, 48],
      chords: [[65, 69, 72, 76], [62, 65, 69, 72], [58, 62, 65, 69], [60, 64, 67, 70]],
      roots: [0, -3, -7, -5], arp: [0, 2, 1, 3, 0, 3, 2, 1], kick: [0, 4, 8, 12], hats: 1 },
    { name: 'Slow Orbit', bpm: 86, swing: 0.2,
      bass: [38, 0, 0, 0, 0, 0, 45, 0, 38, 0, 0, 41, 0, 0, 43, 0],
      chords: [[62, 65, 69, 72], [58, 62, 65, 69], [60, 64, 67, 71], [57, 60, 64, 67]],
      roots: [0, -4, -2, -5], arp: [0, 1, 2, 3, 3, 2, 1, 0], kick: [0, 7, 10], hats: 4 },
  ];

  const music = {
    context: null,
    analyser: null,
    output: null,
    playing: false,
    track: 0,
    step: 0,
    nextTime: 0,
    timer: 0,
    gain: -1,
    spectrum: new Spectrum(),
    setup() {
      if (this.context) return;
      this.context = new AudioContext();
      const c = this.context;
      this.bus = c.createGain();
      this.bus.gain.value = 0.5;
      this.analyser = c.createAnalyser();
      this.analyser.fftSize = 2048;
      this.analyser.smoothingTimeConstant = 0.5;
      this.output = c.createGain();
      this.output.gain.value = 0;
      // Before the volume, like the sink monitor fnstrip listens to.
      this.bus.connect(this.analyser);
      this.bus.connect(this.output).connect(c.destination);
      this.noise = c.createBuffer(1, c.sampleRate, c.sampleRate);
      const samples = this.noise.getChannelData(0);
      for (let i = 0; i < samples.length; i += 1) samples[i] = Math.random() * 2 - 1;
    },
    play() {
      this.setup();
      this.context.resume();
      this.playing = true;
      this.step = 0;
      this.nextTime = this.context.currentTime + 0.06;
      clearInterval(this.timer);
      this.timer = setInterval(() => this.schedule(), 25);
      this.schedule();
    },
    pause() {
      this.playing = false;
      clearInterval(this.timer);
      if (this.output) this.output.gain.setTargetAtTime(0, this.context.currentTime, 0.02);
      this.gain = -1;
    },
    skip(by) {
      this.track = (this.track + by + TRACKS.length) % TRACKS.length;
      if (this.playing) this.play();
    },
    // Follows the page's volume slider and mute.
    update() {
      if (!this.context) return;
      const target = this.playing && !state.muted ? 0.9 * (state.levels.volume / 100) ** 2 : 0;
      if (Math.abs(target - this.gain) > 0.001) {
        this.output.gain.setTargetAtTime(target, this.context.currentTime, 0.03);
        this.gain = target;
      }
      if (this.playing) this.spectrum.update(this.analyser, this.context.sampleRate);
    },
    schedule() {
      const t = TRACKS[this.track];
      const sixteenth = 60 / t.bpm / 4;
      while (this.nextTime < this.context.currentTime + 0.12) {
        const step = this.step % 16;
        const bar = Math.floor(this.step / 16) % 4;
        const when = this.nextTime + (step % 2 ? t.swing * sixteenth : 0);
        this.note(t, step, bar, when, sixteenth);
        this.nextTime += sixteenth;
        this.step += 1;
      }
    },
    note(t, step, bar, when, sixteenth) {
      if (t.kick.includes(step)) this.kick(when);
      if (step % t.hats === 0) this.hat(when, step % 4 === 2 ? 0.16 : 0.08);
      if (step === 4 || step === 12) this.snare(when);
      const bass = t.bass[step];
      if (bass) this.bass(NOTE(bass + t.roots[bar]), when, sixteenth * 1.6);
      if (step === 0) for (const n of t.chords[bar]) this.pad(NOTE(n), when, sixteenth * 15);
      if (step % 2 === 0) {
        const n = t.chords[bar][t.arp[(step / 2) % t.arp.length]] + 12;
        this.pluck(NOTE(n), when, sixteenth * 1.5);
      }
    },
    envelope(node, when, peak, attack, length) {
      node.gain.setValueAtTime(0.0001, when);
      node.gain.exponentialRampToValueAtTime(peak, when + attack);
      node.gain.exponentialRampToValueAtTime(0.0001, when + length);
    },
    voice(type, frequency, when, length, peak, attack, cutoff) {
      const c = this.context;
      const osc = c.createOscillator();
      const filter = c.createBiquadFilter();
      const gain = c.createGain();
      osc.type = type;
      osc.frequency.setValueAtTime(frequency, when);
      filter.type = 'lowpass';
      filter.frequency.setValueAtTime(cutoff, when);
      filter.frequency.exponentialRampToValueAtTime(Math.max(200, cutoff / 4), when + length);
      this.envelope(gain, when, peak, attack, length);
      osc.connect(filter).connect(gain).connect(this.bus);
      osc.start(when);
      osc.stop(when + length + 0.05);
      return osc;
    },
    kick(when) {
      const osc = this.voice('sine', 150, when, 0.35, 0.9, 0.003, 4000);
      osc.frequency.exponentialRampToValueAtTime(45, when + 0.12);
    },
    bass(frequency, when, length) {
      this.voice('sawtooth', frequency, when, length, 0.35, 0.005, 900);
    },
    pad(frequency, when, length) {
      this.voice('triangle', frequency, when, length, 0.06, 0.4, 2500);
    },
    pluck(frequency, when, length) {
      this.voice('square', frequency, when, length, 0.05, 0.004, 3200);
    },
    burst(when, length, peak, type, cutoff) {
      const c = this.context;
      const source = c.createBufferSource();
      const filter = c.createBiquadFilter();
      const gain = c.createGain();
      source.buffer = this.noise;
      filter.type = type;
      filter.frequency.value = cutoff;
      this.envelope(gain, when, peak, 0.002, length);
      source.connect(filter).connect(gain).connect(this.bus);
      source.start(when, Math.random() * 0.5);
      source.stop(when + length + 0.05);
    },
    hat(when, peak) {
      this.burst(when, 0.05, peak, 'highpass', 7000);
    },
    snare(when) {
      this.burst(when, 0.18, 0.3, 'bandpass', 1800);
      this.voice('triangle', 190, when, 0.1, 0.25, 0.002, 3000);
    },
  };

  // ---- Saying what a tap does --------------------------------------------------------------

  const said = document.getElementById('said');
  let saidTimer = 0;
  function say(html) {
    said.innerHTML = html;
    clearTimeout(saidTimer);
    saidTimer = setTimeout(() => { said.innerHTML = '&nbsp;'; }, 4000);
  }

  // ---- The page reacting to the sliders ------------------------------------------------------

  const dim = document.getElementById('dim');
  const deck = document.getElementById('deck');
  function applyLevels() {
    const b = state.levels.brightness;
    dim.style.opacity = String(Math.max(0, (70 - b) / 70) * 0.78);
    const k = state.levels.keyboard / 100;
    deck.style.setProperty('--key-glyph', `rgba(${Math.round(140 + 115 * k)},${Math.round(140 + 115 * k)},${Math.round(150 + 105 * k)},1)`);
    deck.style.setProperty('--key-glow', k > 0.05 ? `0 0 ${Math.round(2 + 8 * k)}px rgba(200,225,255,${(0.75 * k).toFixed(2)})` : 'none');
  }

  let audio = null;
  function tick() {
    if (state.muted) return;
    audio = audio || new AudioContext();
    const t = audio.currentTime;
    const osc = audio.createOscillator();
    const gain = audio.createGain();
    osc.frequency.setValueAtTime(880, t);
    osc.frequency.exponentialRampToValueAtTime(440, t + 0.08);
    gain.gain.setValueAtTime(0.25 * (state.levels.volume / 100) ** 2, t);
    gain.gain.exponentialRampToValueAtTime(0.0001, t + 0.12);
    osc.connect(gain).connect(audio.destination);
    osc.start(t);
    osc.stop(t + 0.13);
  }

  // ---- Touches on the bar ------------------------------------------------------------------

  function barX(event) {
    const rect = display.getBoundingClientRect();
    return pan + ((event.clientX - rect.left) / rect.width) * display.width;
  }

  // Two fingers pan the bar (narrow screens); one finger uses it.
  const pointers = new Map();
  let panning = null;
  const averageX = () => [...pointers.values()].reduce((sum, x) => sum + x, 0) / pointers.size;

  const plateAt = (x) => layout().find((p) => x >= p.x && x < p.x + p.w);
  let touch = null;

  function setLevel(p, x) {
    const level = Math.round(Math.min(100, Math.max(0, ((x - p.x) / p.w) * 100)));
    if (level !== state.levels[p.id]) {
      state.levels[p.id] = level;
      applyLevels();
    }
  }

  display.addEventListener('pointerdown', (event) => {
    pointers.set(event.pointerId, event.clientX);
    if (pointers.size >= 2) {
      state.pressed = null;
      touch = null;
      panning = { last: averageX() };
      panTarget = null;
      return;
    }
    if (intro.running) {
      stopIntro();
      return;
    }
    const x = barX(event);
    if (x === null) return;
    if (testScreen.running) {
      testScreen.tap(x);
      return;
    }
    const p = plateAt(x);
    if (!p) return;
    display.setPointerCapture(event.pointerId);
    sim.busy += 0.4;
    // The speaker and keyboard icons are buttons: mute, and backlight off/on.
    const iconTap = (p.id === 'volume' || p.id === 'keyboard') && x < p.x + SLIDER_ICON;
    touch = { plate: p, startX: x, moved: false, muteTap: iconTap };
    state.pressed = p.slider ? null : p.id;
    if (p.slider && !touch.muteTap) setLevel(p, x);
  });

  display.addEventListener('pointermove', (event) => {
    if (pointers.has(event.pointerId)) pointers.set(event.pointerId, event.clientX);
    if (panning) {
      const now = averageX();
      const rect = display.getBoundingClientRect();
      pan = clampPan(pan - ((now - panning.last) / rect.width) * display.width);
      panning.last = now;
      return;
    }
    if (!touch) return;
    const x = barX(event);
    if (x === null) return;
    sim.busy += 0.05;
    const p = touch.plate;
    if (Math.abs(x - touch.startX) > 12) touch.moved = true;
    if (p.slider && (!touch.muteTap || touch.moved)) setLevel(p, Math.min(Math.max(x, p.x), p.x + p.w));
    if (!p.slider) state.pressed = x >= p.x && x < p.x + p.w ? p.id : null;
  });

  function release(event) {
    pointers.delete(event.pointerId);
    if (panning) {
      if (pointers.size < 2) panning = null;
      return;
    }
    if (!touch) return;
    const p = touch.plate;
    const x = barX(event);
    const inside = x !== null && x >= p.x && x < p.x + p.w;
    if (p.slider) {
      if (touch.muteTap && !touch.moved && p.id === 'keyboard') {
        const current = state.levels.keyboard;
        if (current > 0) state.keyboardRestore = current;
        state.levels.keyboard = current > 0 ? 0 : state.keyboardRestore || 50;
        applyLevels();
        say(`<b>keyboard backlight ${current > 0 ? 'off' : `on, ${state.levels.keyboard}%`}</b>`);
      } else if (touch.muteTap && !touch.moved) {
        state.muted = !state.muted;
        say(state.muted ? '<b>muted</b>' : '<b>unmuted</b>');
      } else {
        const names = { brightness: 'display brightness', keyboard: 'keyboard backlight', volume: 'volume' };
        say(`<b>${names[p.id]} ${state.levels[p.id]}%</b>`);
        if (p.id === 'volume') tick();
      }
    } else if (inside && event.type === 'pointerup') {
      activate(p);
    }
    state.pressed = null;
    touch = null;
  }
  display.addEventListener('pointerup', release);
  display.addEventListener('pointercancel', release);

  async function activate(p) {
    if (p.id === 'playpause' || p.id === 'previous' || p.id === 'next') {
      if (p.id === 'playpause') {
        if (music.playing) music.pause(); else music.play();
      } else {
        music.skip(p.id === 'next' ? 1 : -1);
      }
      const track = `&ldquo;${TRACKS[music.track].name}&rdquo;`;
      say(music.playing
        ? `<b>playing ${track}</b>, a loop synthesized in your browser <span class="cmd">(on the bar: any MPRIS player)</span>`
        : p.id === 'playpause' ? '<b>paused</b>' : `<b>${track}</b> is up next: tap play`);
      return;
    }
    if (!p.extra) {
      say(`<b>${p.say()}</b>`);
      return;
    }
    if (p.extra.kind === 'meter') {
      say(`<b>${p.extra.title}</b>: ${p.extra.what}`);
      return;
    }
    const on = !state.lit[p.id];
    say(`<b>${p.id}</b> runs <span class="cmd">sh -c '${escapeHtml(custom[p.id]?.command ?? '')}'</span>`);
    try {
      if (p.id === 'camera') {
        if (on) await camera.start(); else camera.stop();
      } else if (p.id === 'mic') {
        if (on) await mic.start(); else mic.stop();
      }
      state.lit[p.id] = on;
    } catch (error) {
      say(`<b>${p.id}</b>: the browser didn't allow it (${error.name})`);
    }
  }

  // ---- Fn: hold for the F-keys, double-tap for the extras row -------------------------------

  const FN_TAP_MAX = 300;
  const FN_DOUBLE_TAP_GAP = 300;
  const fnKey = document.getElementById('fn');
  // Fn taps, as the renderer counts them (FnTaps in src/builtin_renderer.rs): a double-tap
  // for the extras row, and, as with debug_gestures = "on", a triple-tap-and-hold for the
  // test screen and a quad-tap for the next theme.
  let fnDownAt = 0;
  let taps = 0; // quick taps in a row, up to the last release
  let lastTap = -Infinity;
  let before = 0; // taps chained before the current press
  let chainOver = false; // the current press already did its thing
  let holdTimer = 0;

  function fnPress() {
    if (state.fnDown) return;
    if (intro.running) stopIntro();
    const now = performance.now();
    state.fnDown = true;
    fnKey.classList.add('down');
    if (testScreen.running) {
      // A new Fn press leaves the test screen, as on the bar.
      testScreen.stop();
      chainOver = true;
      before = 0;
      fnDownAt = now;
      return;
    }
    before = now - lastTap <= FN_DOUBLE_TAP_GAP ? taps : 0;
    chainOver = false;
    const wasExtras = state.extras;
    state.extras = before === 1;
    if (state.extras && !wasExtras) say('<b>extras row</b>: it stays until the next Fn press');
    if (before === 2) {
      holdTimer = setTimeout(() => {
        if (!state.fnDown) return;
        chainOver = true;
        testScreen.start();
      }, FN_TAP_MAX);
    } else if (before === 3) {
      chainOver = true;
      const names = Object.keys(THEMES);
      setTheme(names[(names.indexOf(themeName) + 1) % names.length]);
      say(`<b>theme "${themeName}"</b>: quad-tap fn again for the next (debug_gestures = "on")`);
    }
    fnDownAt = now;
  }

  function fnRelease() {
    if (!state.fnDown) return;
    const now = performance.now();
    state.fnDown = false;
    fnKey.classList.remove('down');
    clearTimeout(holdTimer);
    const wasTap = !chainOver && before <= 3 && now - fnDownAt <= FN_TAP_MAX;
    taps = wasTap ? before + 1 : 0;
    lastTap = wasTap ? now : -Infinity;
    before = 0;
  }

  fnKey.addEventListener('pointerdown', (event) => {
    event.preventDefault();
    fnKey.setPointerCapture(event.pointerId);
    fnPress();
  });
  fnKey.addEventListener('pointerup', fnRelease);
  fnKey.addEventListener('pointercancel', fnRelease);
  fnKey.addEventListener('keydown', (event) => {
    if ((event.key === ' ' || event.key === 'Enter') && !event.repeat) {
      event.preventDefault();
      fnPress();
    }
  });
  fnKey.addEventListener('keyup', (event) => {
    if (event.key === ' ' || event.key === 'Enter') fnRelease();
  });
  const typing = (event) => event.target.closest?.('input, textarea, [contenteditable]');
  window.addEventListener('keydown', (event) => {
    if (typing(event) || event.metaKey || event.ctrlKey || event.altKey) return;
    if (event.key === 'f' || event.key === 'F') {
      if (!event.repeat) fnPress();
    }
  });
  window.addEventListener('keyup', (event) => {
    if (event.key === 'f' || event.key === 'F') fnRelease();
  });
  window.addEventListener('blur', fnRelease);
  document.addEventListener('pointermove', () => { sim.busy += 0.004; }, { passive: true });

  // ---- The test screen (a small taste of src/screensaver.rs) --------------------------------
  // Triple-tap fn and hold. The bar's version has more scenes, live audio spectra and the
  // camera; this one shows the idea: full-colour animation with its frame rate and render time.
  // Tap the bar for the next scene, its right end (or fn) to leave.

  const testScreen = {
    running: false,
    scene: 0,
    frames: [],
    renderMs: 0,
    small: Object.assign(document.createElement('canvas'), { width: 434, height: 12 }),
    start() {
      this.running = true;
      this.scene = 0;
      this.frames = [];
      say('<b>test screen</b>: tap for the next scene, the right end (or fn) to leave <span class="cmd">(debug_gestures = "on")</span>');
    },
    stop() {
      this.running = false;
    },
    tap(x) {
      if (x > W - 220) this.stop();
      else this.scene = (this.scene + 1) % 3;
    },
    draw(now) {
      const started = performance.now();
      const t = now / 1000;
      if (this.scene === 0) {
        // Plasma, computed small and scaled up.
        const g = this.small.getContext('2d');
        const image = g.createImageData(434, 12);
        for (let y = 0; y < 12; y += 1) {
          for (let x = 0; x < 434; x += 1) {
            const v = Math.sin(x / 16 + t * 2) + Math.sin((x + y * 8) / 23 - t * 1.4) + Math.sin(Math.hypot(x - 217 + 120 * Math.sin(t / 2), y * 9) / 11 - t * 2.6);
            const [r, gr, b] = hsv((v * 60 + t * 40) % 360 + 360, 0.9, 0.95);
            const i = (y * 434 + x) * 4;
            image.data[i] = r; image.data[i + 1] = gr; image.data[i + 2] = b; image.data[i + 3] = 255;
          }
        }
        g.putImageData(image, 0, 0);
        ctx.imageSmoothingEnabled = true;
        ctx.drawImage(this.small, 0, 0, W, H);
      } else if (this.scene === 1) {
        // Colour bars racing along.
        for (let x = 0; x < W; x += 31) {
          ctx.fillStyle = `hsl(${(x / 3 + t * 220) % 360}, 90%, ${45 + 15 * Math.sin(x / 90 - t * 5)}%)`;
          ctx.fillRect(x, 0, 31, H);
        }
      } else {
        // A pulse running the length of the bar.
        ctx.fillStyle = '#000';
        ctx.fillRect(0, 0, W, H);
        for (let i = 0; i < 6; i += 1) {
          const cx = ((t * 600 + i * 380) % (W + 200)) - 100;
          const glow = ctx.createRadialGradient(cx, H / 2, 0, cx, H / 2, 140);
          glow.addColorStop(0, `hsla(${i * 60 + t * 50}, 100%, 65%, 1)`);
          glow.addColorStop(1, 'hsla(0, 0%, 0%, 0)');
          ctx.fillStyle = glow;
          ctx.fillRect(cx - 140, 0, 280, H);
        }
      }
      this.renderMs = this.renderMs * 0.9 + (performance.now() - started) * 0.1;
      this.frames.push(now);
      while (this.frames.length && now - this.frames[0] > 1000) this.frames.shift();
      // The stats, at the right end, as the bar shows them.
      ctx.fillStyle = 'rgba(0, 0, 0, 0.78)';
      ctx.fillRect(W - 220, 0, 220, H);
      ctx.fillRect(0, 0, 64, H);
      ctx.fillStyle = '#f2f2f2';
      ctx.font = '500 20px "JetBrains Mono", monospace';
      ctx.textBaseline = 'middle';
      ctx.textAlign = 'left';
      ctx.fillText(`${this.scene + 1}/3`, 12, H / 2);
      ctx.fillText(`${this.frames.length} FPS`, W - 206, H / 2 - 12);
      ctx.fillText(`${this.renderMs.toFixed(1)} MS`, W - 206, H / 2 + 12);
      ctx.fillStyle = '#ff6b5a';
      ctx.fillText('EXIT', W - 70, H / 2);
    },
  };

  // ---- The startup animation (a port of src/intro.rs) ---------------------------------------

  const DURATION = 1.8;
  const COLLISION = 0.42;
  const LOGO_IN = [0.42, 0.62];
  const LOGO_COOL = [0.45, 0.95];
  const SHIMMER = [0.7, 1.05];
  const LOGO_OUT = [1.1, 1.4];
  const REVEAL = [1.05, 1.65];
  const AFTERGLOW_OUT = [1.1, 1.6];
  const CYAN = [0, 0.78, 1];
  const MAGENTA = [1, 0.24, 0.78];
  const WARM_WHITE = [1, 0.95, 0.88];
  const HALO_PAD = 12;
  const FEATHER = 30;

  const gauss = (offset, width) => Math.exp(-((offset / width) ** 2));
  const smooth = ([a, b], t) => {
    const x = Math.min(1, Math.max(0, (t - a) / (b - a)));
    return x * x * (3 - 2 * x);
  };
  const lerp3 = (from, to, amount) => [0, 1, 2].map((c) => from[c] + (to[c] - from[c]) * amount);
  function add(light, color, amount) {
    light[0] += color[0] * amount;
    light[1] += color[1] * amount;
    light[2] += color[2] * amount;
  }
  function addHot(light, color, amount) {
    add(light, color, amount);
    const white = Math.max(0, amount - 0.6) * 1.2;
    add(light, [1, 1, 1], white);
  }

  const intro = { running: false, start: 0, mask: null, halo: null, mw: 0, mh: 0 };

  function prepareWordmark() {
    const s = S.wordmark;
    const c = document.createElement('canvas');
    c.width = s.w;
    c.height = s.h;
    const g = c.getContext('2d');
    g.drawImage(sprites, s.x, 0, s.w, s.h, 0, 0, s.w, s.h);
    const data = g.getImageData(0, 0, s.w, s.h).data;
    intro.mw = s.w;
    intro.mh = s.h;
    intro.mask = new Float32Array(s.w * s.h);
    for (let i = 0; i < intro.mask.length; i += 1) intro.mask[i] = data[i * 4 + 3] / 255;
    const hw = s.w + 2 * HALO_PAD;
    const hh = s.h + 2 * HALO_PAD;
    const halo = new Float32Array(hw * hh);
    for (let y = 0; y < s.h; y += 1) {
      for (let x = 0; x < s.w; x += 1) halo[(y + HALO_PAD) * hw + x + HALO_PAD] = intro.mask[y * s.w + x];
    }
    const blur = (start, step, count) => {
      const out = new Float32Array(count);
      for (let i = 0; i < count; i += 1) {
        let sum = 0;
        for (let j = Math.max(0, i - 3); j <= Math.min(count - 1, i + 3); j += 1) sum += halo[start + j * step];
        out[i] = sum / 7;
      }
      for (let i = 0; i < count; i += 1) halo[start + i * step] = out[i];
    };
    for (let pass = 0; pass < 3; pass += 1) {
      for (let y = 0; y < hh; y += 1) blur(y * hw, 1, hw);
      for (let x = 0; x < hw; x += 1) blur(x, hw, hh);
    }
    let brightest = 0;
    for (const v of halo) brightest = Math.max(brightest, v);
    for (let i = 0; i < halo.length; i += 1) halo[i] /= brightest || 1;
    intro.halo = halo;
  }

  function drawIntro(t) {
    const image = ctx.getImageData(0, 0, W, H);
    const px = image.data;
    const center = W / 2;
    const middle = (H - 1) / 2;
    const thinProfile = new Float32Array(H);
    const broadProfile = new Float32Array(H);
    for (let y = 0; y < H; y += 1) {
      thinProfile[y] = gauss(y - middle, 3.5) + 0.35 * gauss(y - middle, 14);
      broadProfile[y] = 0.7 + 0.3 * gauss(y - middle, 18);
    }
    const thin = new Float32Array(W * 3);
    const broad = new Float32Array(W * 3);
    const uncovered = new Float32Array(W);
    const afterglow = 1 - smooth(AFTERGLOW_OUT, t);
    const since = t - COLLISION;
    const reveal = smooth(REVEAL, t);
    const logoHalf = intro.mw / 2 + 10;
    const radius = logoHalf + (1 - (1 - reveal) ** 3) * (center + 80 - logoHalf);
    const logoGone = smooth([LOGO_OUT[1] - 0.05, LOGO_OUT[1] + 0.15], t);
    const fronts = t < REVEAL[0] ? 0
      : smooth([REVEAL[0], REVEAL[0] + 0.05], t) * (1 - smooth([REVEAL[1] - 0.12, REVEAL[1]], t));
    const light = [0, 0, 0];
    const lightBroad = [0, 0, 0];

    for (let x = 0; x < W; x += 1) {
      const column = x + 0.5;
      const distance = Math.abs(column - center);
      const side = column < center ? CYAN : MAGENTA;
      light[0] = light[1] = light[2] = 0;
      lightBroad[0] = lightBroad[1] = lightBroad[2] = 0;
      if (t < COLLISION) {
        const progress = t / COLLISION;
        const headDistance = (1 - progress * progress) * (center + 60);
        const fadeIn = Math.min(1, progress * 4);
        const pastHead = headDistance - distance;
        const beam = pastHead >= 0 ? Math.exp(-pastHead / 10) : Math.exp(pastHead / 260);
        addHot(light, side, 1.4 * fadeIn * beam);
      }
      if (since >= 0) {
        const strength = (1 - Math.exp(-since / 0.03)) * Math.exp(-since / 0.22) * afterglow;
        const spread = 60 + 1400 * since;
        addHot(lightBroad, WARM_WHITE, 1.3 * strength * gauss(distance, spread) + 0.12 * strength);
        add(light, lerp3(CYAN, MAGENTA, column / W), 0.55 * Math.exp(-since / 0.35) * afterglow);
      }
      if (t >= REVEAL[0]) {
        const toFront = distance - radius;
        const front = gauss(toFront, 26) + (toFront < 0 ? 0.35 * Math.exp(toFront / 120) : 0);
        addHot(lightBroad, side, 1.3 * fronts * front);
        uncovered[x] = Math.min(1, Math.max(0, 0.5 - toFront / FEATHER));
        if (distance < logoHalf + FEATHER) uncovered[x] *= logoGone;
      }
      thin.set(light, x * 3);
      broad.set(lightBroad, x * 3);
    }

    for (let y = 0; y < H; y += 1) {
      const tp = thinProfile[y];
      const bp = broadProfile[y];
      for (let x = 0; x < W; x += 1) {
        const i = (y * W + x) * 4;
        const keep = uncovered[x];
        for (let c = 0; c < 3; c += 1) {
          const value = (px[i + c] / 255) * keep + thin[x * 3 + c] * tp + broad[x * 3 + c] * bp;
          px[i + c] = Math.min(255, Math.max(0, value * 255));
        }
      }
    }
    drawWordmark(px, t);
    ctx.putImageData(image, 0, 0);
  }

  function drawWordmark(px, t) {
    const opacity = smooth(LOGO_IN, t) * (1 - smooth(LOGO_OUT, t));
    if (opacity <= 0) return;
    const { mw, mh } = intro;
    const hw = mw + 2 * HALO_PAD;
    const hh = mh + 2 * HALO_PAD;
    const left = Math.floor((W - mw) / 2);
    const top = Math.floor((H - mh) / 2);
    const whiteHot = Math.max(1 - smooth(LOGO_COOL, t), Math.sqrt(smooth(LOGO_OUT, t)));
    const glow = opacity * (0.45 + 0.75 * whiteHot);
    const shimmering = t > SHIMMER[0] && t < SHIMMER[1];
    const shimmerAt = -0.2 + 1.4 * smooth(SHIMMER, t);
    for (let hy = 0; hy < hh; hy += 1) {
      const y = top + hy - HALO_PAD;
      if (y < 0 || y >= H) continue;
      for (let hx = 0; hx < hw; hx += 1) {
        const x = left + hx - HALO_PAD;
        if (x < 0 || x >= W) continue;
        const across = (hx - HALO_PAD) / mw;
        const tint = lerp3(CYAN, MAGENTA, Math.min(1, Math.max(0, across)));
        const halo = intro.halo[hy * hw + hx] * glow;
        const mx = hx - HALO_PAD;
        const my = hy - HALO_PAD;
        const ink = (mx >= 0 && my >= 0 && mx < mw && my < mh ? intro.mask[my * mw + mx] : 0) * opacity;
        let letters = lerp3(tint, [1, 1, 1], 0.25 + 0.75 * whiteHot);
        if (shimmering) {
          const slant = across + 0.15 * (hy / hh);
          letters = lerp3(letters, [1, 1, 1], gauss(slant - shimmerAt, 0.06));
        }
        const i = (y * W + x) * 4;
        for (let c = 0; c < 3; c += 1) {
          const below = px[i + c] / 255 + tint[c] * halo;
          px[i + c] = Math.min(255, Math.max(0, (below * (1 - ink) + letters[c] * ink) * 255));
        }
      }
    }
  }

  function playIntro() {
    try {
      if (!intro.mask) prepareWordmark();
    } catch (error) {
      console.error(error);
      return;
    }
    state.extras = false;
    intro.running = true;
    intro.start = performance.now();
  }
  function stopIntro() {
    intro.running = false;
    panTarget = 0;
  }
  document.getElementById('replay').addEventListener('click', () => {
    playIntro();
    display.scrollIntoView({ behavior: 'smooth', block: 'center' });
  });

  // ---- Presenting: the whole bar, or on narrow screens a panned part of it -----------------

  const clampPan = (x) => Math.min(Math.max(0, x), W - view);
  const mini = document.getElementById('mini');
  const miniContext = mini.getContext('2d');

  function layoutDisplay() {
    const width = display.parentElement.clientWidth;
    narrow = width < 700;
    view = narrow ? Math.min(W, Math.round(width / NARROW_SCALE)) : W;
    if (display.width !== view) display.width = view;
    // During the startup animation, look at the middle, where the wordmark is.
    pan = clampPan(intro.running || intro.frozen !== undefined ? (W - view) / 2 : pan);
    mini.parentElement.hidden = !narrow; // the scrubber
  }

  function present() {
    if (panTarget !== null) {
      pan += (panTarget - pan) * 0.12;
      if (Math.abs(panTarget - pan) < 0.5) {
        pan = panTarget;
        panTarget = null;
      }
      pan = clampPan(pan);
    }
    shown.drawImage(frame, pan, 0, view, H, 0, 0, view, H);
    if (!narrow) return;
    // Fade the ends that run on past the screen.
    const fade = (from, to, x) => {
      const gradient = shown.createLinearGradient(from, 0, to, 0);
      gradient.addColorStop(0, 'rgba(5,5,6,0)');
      gradient.addColorStop(1, 'rgba(5,5,6,0.92)');
      shown.fillStyle = gradient;
      shown.fillRect(x, 0, 90, H);
    };
    if (pan < W - view - 1) fade(view - 90, view, view - 90);
    if (pan > 1) fade(90, 0, 0);
    // The whole strip, with the part on screen framed.
    const mw = mini.width;
    const mh = mini.height;
    const scale = mw / W;
    miniContext.drawImage(frame, 0, 0, W, H, 0, 0, mw, mh);
    miniContext.fillStyle = 'rgba(5,5,6,0.6)';
    miniContext.fillRect(0, 0, pan * scale, mh);
    miniContext.fillRect((pan + view) * scale, 0, mw - (pan + view) * scale, mh);
    miniContext.strokeStyle = 'rgba(255,255,255,0.9)';
    miniContext.lineWidth = 2;
    miniContext.strokeRect(pan * scale + 1, 1, view * scale - 2, mh - 2);
    thumb.style.left = `${(pan / W) * 100}%`;
    thumb.style.width = `${(view / W) * 100}%`;
  }

  // The scrubber: anywhere in it, a finger's x along it is where the view centres.
  const scrub = document.getElementById('scrub');
  const thumb = document.getElementById('thumb');
  function panFromScrub(event) {
    const rect = mini.getBoundingClientRect();
    const x = ((event.clientX - rect.left) / rect.width) * W;
    panTarget = null;
    pan = clampPan(x - view / 2);
  }
  scrub.addEventListener('pointerdown', (event) => {
    scrub.setPointerCapture(event.pointerId);
    scrub.classList.add('sliding');
    panFromScrub(event);
  });
  scrub.addEventListener('pointermove', (event) => {
    if (scrub.hasPointerCapture(event.pointerId)) panFromScrub(event);
  });
  const endScrub = () => scrub.classList.remove('sliding');
  scrub.addEventListener('pointerup', endScrub);
  scrub.addEventListener('pointercancel', endScrub);

  function loop(now) {
    requestAnimationFrame(loop); // first, so one bad frame can't stop the bar
    try {
      layoutDisplay();
      mic.update();
      music.update();
      if (testScreen.running) testScreen.draw(now);
      else drawRow();
      if (intro.frozen !== undefined) {
        drawIntro(intro.frozen);
      } else if (intro.running) {
        const t = (now - intro.start) / 1000;
        if (t >= DURATION) stopIntro();
        else drawIntro(t);
      }
    } catch (error) {
      // Skip the animation, keep the bar.
      intro.running = false;
      intro.frozen = undefined;
      console.error(error);
    }
    present();
    if (builderVisible) drawBuilder();
  }

  // ---- The builder: pick the extras row, get the config ----------------------------------

  const palette = document.getElementById('palette');
  const preview = document.getElementById('preview').getContext('2d');
  const configOut = document.getElementById('config');
  const defaultRow = document.getElementById('default-row');
  const startup = document.getElementById('startup');
  const debugGestures = document.getElementById('debug-gestures');
  const tiles = {};
  let builderVisible = false;

  for (const [name, info] of Object.entries(EXTRA_INFO)) {
    const wrap = document.createElement('div');
    wrap.className = 'tile-wrap';
    const tile = document.createElement('button');
    tile.type = 'button';
    tile.className = 'tile';
    tile.innerHTML = `<canvas width="${EXTRA_WIDTH}" height="${H}" aria-hidden="true"></canvas>`
      + '<span class="order"></span>'
      + `<span class="tile-title">${info.title}<span class="kind ${info.kind}">${info.kind}</span></span>`
      + `<span class="tile-what">${info.what}</span>`;
    tile.addEventListener('click', () => pick(name));
    const moves = document.createElement('div');
    moves.className = 'moves';
    moves.innerHTML = `<button type="button" aria-label="Move ${info.title} left">&lsaquo;</button>`
      + `<button type="button" aria-label="Move ${info.title} right">&rsaquo;</button>`;
    const [left, right] = moves.querySelectorAll('button');
    left.addEventListener('click', () => move(name, -1));
    right.addEventListener('click', () => move(name, 1));
    wrap.append(tile, moves);
    palette.append(wrap);
    tiles[name] = { tile, left, right, context: tile.querySelector('canvas').getContext('2d') };
  }

  function switchOff(name) {
    if (!state.lit[name]) return;
    if (name === 'camera') camera.stop();
    if (name === 'mic') mic.stop();
    state.lit[name] = false;
  }

  const isMeter = (name) => EXTRA_INFO[name].kind === 'meter';
  /** Toggles first, then meters, each keeping its order: the config order then matches the
   * bar left to right (the bar puts toggles on the left and meters on the right anyway). */
  const grouped = (names) => names.filter((n) => !isMeter(n)).concat(names.filter(isMeter));

  function pick(name) {
    builderUsed = true;
    if (chosen.includes(name)) {
      chosen = chosen.filter((other) => other !== name);
      switchOff(name);
    } else if (chosen.length < MAX_EXTRAS) {
      chosen = grouped(chosen.concat(name));
    }
    rebuild();
  }

  /** Moves a button one place left or right among its own kind. */
  function move(name, step) {
    const group = chosen.filter((n) => isMeter(n) === isMeter(name));
    const from = group.indexOf(name);
    const to = from + step;
    if (from < 0 || to < 0 || to >= group.length) return;
    builderUsed = true;
    [group[from], group[to]] = [group[to], group[from]];
    chosen = isMeter(name) ? chosen.filter((n) => !isMeter(n)).concat(group) : group.concat(chosen.filter(isMeter));
    rebuild();
  }

  function rebuild() {
    layoutExtras();
    for (const [name, { tile, left, right }] of Object.entries(tiles)) {
      const index = chosen.indexOf(name);
      const group = chosen.filter((n) => isMeter(n) === isMeter(name));
      const place = group.indexOf(name);
      tile.setAttribute('aria-pressed', String(index >= 0));
      tile.querySelector('.order').textContent = index >= 0 ? String(index + 1) : '';
      left.disabled = place <= 0;
      right.disabled = place < 0 || place >= group.length - 1;
      tile.parentElement.classList.toggle('picked', index >= 0);
    }
    document.getElementById('picked').textContent = `${chosen.length} of ${MAX_EXTRAS} buttons`;
    renderToggleEditor();
    configOut.innerHTML = highlight(configText());
  }

  // ---- What each toggle runs ----------------------------------------------------------------

  const toggleEditor = document.getElementById('toggle-editor');
  const toggleCards = document.getElementById('toggle-cards');
  let shownToggles = '';

  /** One card per chosen toggle: a preset to start from, then its command and status. */
  function renderToggleEditor() {
    const toggles = chosen.filter((name) => EXTRA_INFO[name].kind === 'toggle' || name === 'net');
    toggleEditor.hidden = toggles.length === 0;
    if (toggles.join() === shownToggles) return; // keep typing undisturbed
    shownToggles = toggles.join();
    toggleCards.replaceChildren(...toggles.map((name) => (name === 'net' ? netCard() : toggleCard(name))));
  }

  /** Rows for a command box: two to five, by length (about 40 characters a line). */
  const fitRows = (text) => Math.min(5, Math.max(2, Math.ceil(text.length / 40)));

  function toggleCard(name) {
    const card = document.createElement('fieldset');
    card.className = 'toggle-card';
    const legend = document.createElement('legend');
    legend.textContent = EXTRA_INFO[name].title;
    const select = document.createElement('select');
    for (const [index, preset] of presetsFor(name).entries()) select.add(new Option(preset.name, String(index)));
    select.add(new Option('Your own', 'own'));
    const field = (label, key, placeholder) => {
      const wrap = document.createElement('label');
      wrap.className = 'cmd-field';
      const text = document.createElement('span');
      text.textContent = label;
      // Two wrapping lines, so long commands stay readable.
      const input = document.createElement('textarea');
      Object.assign(input, { value: custom[name][key], placeholder, spellcheck: false, autocomplete: 'off' });
      input.rows = fitRows(input.value);
      input.addEventListener('input', () => { input.rows = fitRows(input.value); });
      input.setAttribute('autocapitalize', 'off');
      wrap.append(text, input);
      return [wrap, input];
    };
    const [commandField, command] = field('command (runs on tap)', 'command', 'any shell command');
    const [statusField, status] = field('status (exit 0 = lit)', 'status', 'optional: a command that succeeds while it\'s on');
    const [interfaceField, iface] = name === 'vpn' ? interfaceInput('interface (its traffic graph)', 'vpn', 'wg0') : [null, null];
    if (iface) iface.addEventListener('input', () => update());
    const note = document.createElement('p');
    note.className = 'cmd-note';
    const update = () => {
      const presets = presetsFor(name);
      const index = presets.findIndex((p) => p.command === custom[name].command && p.status === custom[name].status);
      select.value = index >= 0 ? String(index) : 'own';
      const notes = [];
      if (index >= 0 && presets[index].hint) notes.push(presets[index].hint);
      if (!custom[name].command.trim()) notes.push('No command: tapping does nothing.');
      if (!custom[name].status.trim()) notes.push('No status: the button is never lit.');
      if (iface && iface.value.trim() && !validInterface(iface.value.trim())) notes.push(INTERFACE_HINT);
      note.textContent = notes.join(' ');
      note.hidden = notes.length === 0;
      configOut.innerHTML = highlight(configText());
    };
    select.addEventListener('change', () => {
      if (select.value === 'own') return;
      const preset = presetsFor(name)[Number(select.value)];
      custom[name] = { command: preset.command, status: preset.status };
      command.value = preset.command;
      status.value = preset.status;
      if (iface && preset.interface) {
        custom.vpn.interface = preset.interface;
        iface.value = preset.interface;
      }
      command.rows = fitRows(command.value);
      status.rows = fitRows(status.value);
      update();
    });
    command.addEventListener('input', () => { custom[name].command = command.value; update(); });
    status.addEventListener('input', () => { custom[name].status = status.value; update(); });
    const top = document.createElement('label');
    top.className = 'cmd-preset';
    top.append('Start from ', select);
    card.append(legend, top, commandField, statusField, ...(interfaceField ? [interfaceField] : []), note);
    update();
    return card;
  }

  const INTERFACE_HINT = 'Interface names are up to 15 characters, with no spaces, slashes or colons; ip link lists yours.';

  /** A one-line box for a network interface name, kept in custom[name].interface. */
  function interfaceInput(label, name, placeholder) {
    const wrap = document.createElement('label');
    wrap.className = 'cmd-field';
    const text = document.createElement('span');
    text.textContent = label;
    const input = document.createElement('input');
    Object.assign(input, { type: 'text', value: custom[name].interface, placeholder, spellcheck: false, autocomplete: 'off' });
    input.setAttribute('autocapitalize', 'off');
    input.addEventListener('input', () => { custom[name].interface = input.value; });
    wrap.append(text, input);
    return [wrap, input];
  }

  /** The net meter: every network device that's up, or the one interface you name. */
  function netCard() {
    const card = document.createElement('fieldset');
    card.className = 'toggle-card';
    const legend = document.createElement('legend');
    legend.textContent = 'Network';
    const about = document.createElement('p');
    about.className = 'cmd-about';
    about.textContent = 'Leave it empty to count every network device that\'s up (Wi-Fi, USB Ethernet, …), or name one to graph only that.';
    const [field, input] = interfaceInput('interface', 'net', 'all devices that are up');
    const note = document.createElement('p');
    note.className = 'cmd-note';
    const update = () => {
      const value = input.value.trim();
      note.hidden = !value || validInterface(value);
      note.textContent = INTERFACE_HINT;
      configOut.innerHTML = highlight(configText());
    };
    input.addEventListener('input', update);
    card.append(legend, about, field, note);
    update();
    return card;
  }

  document.getElementById('pick-all').addEventListener('click', () => {
    builderUsed = true;
    chosen = grouped(chosen.concat(Object.keys(EXTRA_INFO).filter((n) => !chosen.includes(n))));
    rebuild();
  });
  document.getElementById('pick-none').addEventListener('click', () => {
    builderUsed = true;
    chosen.forEach(switchOff);
    chosen = [];
    rebuild();
  });
  defaultRow.addEventListener('change', rebuild);
  startup.addEventListener('change', rebuild);
  debugGestures.addEventListener('change', rebuild);

  // ---- Fine-tune colours: [colors] overrides on top of the theme -----------------------------

  const toHex = (c) => `#${c.map((v) => v.toString(16).padStart(2, '0')).join('')}`;
  const fromHex = (hex) => [1, 3, 5].map((at) => parseInt(hex.slice(at, at + 2), 16));
  const COLOR_INFO = {
    background: 'Behind the buttons',
    plate: 'Buttons and sliders',
    pressed: 'A button while touched',
    label: 'Icons, text, percentages',
    dim: 'Muted or off icons',
    lit: 'Buttons that are on',
    lit_pressed: 'An "on" button while touched',
    fill: 'Slider levels',
    muted: 'The volume level while muted',
  };
  const colorGrid = document.getElementById('color-grid');
  const pickers = {};
  for (const [key, what] of Object.entries(COLOR_INFO)) {
    const row = document.createElement('div');
    row.className = 'color-row';
    const input = Object.assign(document.createElement('input'), { type: 'color', id: `color-${key}` });
    const label = document.createElement('label');
    label.htmlFor = input.id;
    label.innerHTML = `<code>${key}</code><span>${what}</span>`;
    const reset = Object.assign(document.createElement('button'), { type: 'button', className: 'color-reset', textContent: 'reset' });
    reset.title = `Back to the ${key} colour of the theme`;
    input.addEventListener('input', () => {
      overrides[key] = fromHex(input.value);
      applyTheme(themeName);
      syncColorPickers();
      rebuild();
    });
    reset.addEventListener('click', () => {
      delete overrides[key];
      applyTheme(themeName);
      syncColorPickers();
      rebuild();
    });
    row.append(input, label, reset);
    colorGrid.append(row);
    pickers[key] = { input, reset, row };
  }
  document.getElementById('colors-reset').addEventListener('click', () => {
    for (const key of Object.keys(overrides)) delete overrides[key];
    applyTheme(themeName);
    syncColorPickers();
    rebuild();
  });
  /** Each picker shows the colour in use; changed ones are marked and can go back. */
  function syncColorPickers() {
    for (const [key, { input, reset, row }] of Object.entries(pickers)) {
      input.value = toHex(theme[COLOR_KEYS[key]]);
      reset.hidden = !overrides[key];
      row.classList.toggle('changed', Boolean(overrides[key]));
    }
    const count = Object.keys(overrides).length;
    document.getElementById('colors-count').textContent = count ? `${count} changed` : 'as the theme';
    document.getElementById('colors-reset').hidden = count === 0;
  }

  // Theme pickers: chips under the bar and the builder's select, kept in step.
  const themeSelect = document.getElementById('theme');
  const skins = document.getElementById('skins');
  const chips = {};
  for (const name of Object.keys(THEMES)) {
    themeSelect.add(new Option(name, name));
    const chip = document.createElement('button');
    chip.type = 'button';
    chip.className = 'chip skin';
    chip.textContent = name;
    chip.addEventListener('click', () => setTheme(name, true));
    skins.append(chip);
    chips[name] = chip;
  }
  // The themes gallery: each theme's controls row, drawn once by the bar's own code.
  const gallery = {};
  for (const name of Object.keys(THEMES)) {
    const item = document.createElement('button');
    item.type = 'button';
    item.className = 'theme-strip';
    item.innerHTML = `<span class="bezel"><canvas width="${W}" height="${H}" aria-hidden="true"></canvas>`
      + `<canvas width="${W}" height="${H}" aria-hidden="true"></canvas></span>`
      + `<span class="theme-name">theme = "${name}"</span>`;
    item.setAttribute('aria-label', `Use the ${name} theme on the bar above`);
    item.addEventListener('click', () => {
      setTheme(name, true);
      display.scrollIntoView({ behavior: 'smooth', block: 'center' });
    });
    document.getElementById('theme-strips').append(item);
    gallery[name] = item;
  }
  // Each theme's controls row, and its extras row with every button (the VPN lit).
  const galleryExtras = SHOWCASE;
  function drawGallery() {
    const shown = themeName;
    const playing = music.playing;
    const pressed = state.pressed;
    const vpn = state.lit.vpn;
    music.playing = false;
    state.pressed = null;
    state.lit.vpn = true;
    try {
      for (const [name, item] of Object.entries(gallery)) {
        applyTheme(name, true);
        const [controls, extras] = item.querySelectorAll('canvas');
        ctx = controls.getContext('2d');
        drawRow(CONTROLS);
        ctx = extras.getContext('2d');
        drawRow(galleryExtras);
      }
    } finally {
      ctx = barContext;
      music.playing = playing;
      state.pressed = pressed;
      state.lit.vpn = vpn;
      applyTheme(shown);
    }
  }
  // Keep the gallery's meters moving while it's on screen.
  let galleryVisible = false;
  new IntersectionObserver((entries) => {
    galleryVisible = entries.some((entry) => entry.isIntersecting);
  }).observe(document.getElementById('theme-strips'));
  setInterval(() => { if (galleryVisible) drawGallery(); }, 500);

  function setTheme(name, announce = false) {
    applyTheme(name);
    syncColorPickers();
    themeSelect.value = themeName;
    for (const [n, chip] of Object.entries(chips)) chip.setAttribute('aria-pressed', String(n === themeName));
    for (const [n, item] of Object.entries(gallery)) item.classList.toggle('active', n === themeName);
    rebuild();
    if (announce) say(`<b>theme = "${themeName}"</b> in the config`);
  }
  themeSelect.addEventListener('change', () => setTheme(themeSelect.value));
  const unitSelect = document.getElementById('temperature-unit');
  unitSelect.addEventListener('change', () => {
    fahrenheit = unitSelect.value === 'f';
    rebuild();
  });

  /** A TOML string fnstrip's parser reads back exactly: "basic" (with \\" \\\\ \\n \\t escapes) unless a
   *  'literal' one avoids escapes (no ' and no line breaks or tabs). */
  function tomlString(value) {
    if (!/["\\\n\t]/.test(value)) return `"${value}"`;
    if (!/['\n\t]/.test(value)) return `'${value}'`;
    return `"${value.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\n').replace(/\t/g, '\\t')}"`;
  }
  function configText() {
    const lines = [
      '# fnstrip config, made with the builder on the fnstrip page.',
      '# Save as ~/.config/fnstrip/config.toml, then:',
      '#   systemctl --user restart t1-touchbar.service',
      '# Every setting is described in config/default.toml.',
      '',
      `default_row = "${defaultRow.value}"`,
      `startup_animation = "${startup.checked ? 'on' : 'off'}"`,
      `theme = "${themeName}"   # ${Object.keys(THEMES).join(', ')}`,
      `temperature_unit = "${fahrenheit ? 'f' : 'c'}"`,
      `debug_gestures = "${debugGestures.checked ? 'on' : 'off'}"   # fn triple-tap-and-hold: test screen; quad-tap: next theme`,
    ];
    const changed = Object.keys(COLOR_KEYS).filter((key) => overrides[key]);
    if (changed.length) {
      lines.push('', `# Colours changed on top of the ${themeName} theme.`, '[colors]');
      for (const key of changed) lines.push(`${key} = "${toHex(overrides[key])}"`);
    }
    for (const name of chosen) {
      const info = EXTRA_INFO[name];
      lines.push('', '[[extras]]');
      if (info.kind === 'toggle') {
        const { command, status } = custom[name];
        const preset = presetsFor(name).find((p) => p.command === command && p.status === status);
        if (preset) lines.push(`# ${preset.name}`);
        lines.push(`icon    = "${name}"`, `command = ${tomlString(command)}`);
        if (status.trim()) lines.push(`status  = ${tomlString(status)}`);
        else lines.push('# No status command: the button is never lit.');
        if (name === 'vpn' && custom.vpn.interface.trim()) lines.push(`interface = ${tomlString(custom.vpn.interface.trim())}   # its traffic graph`);
      } else {
        lines.push(`icon = "${name}"   # ${info.short}`);
        if (name === 'net' && custom.net.interface.trim()) lines.push(`interface = ${tomlString(custom.net.interface.trim())}`);
      }
    }
    if (!chosen.length) lines.push('', '# No [[extras]]: the extras row shows just Esc.');
    return `${lines.join('\n')}\n`;
  }

  const escapeHtml = (text) => text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  function highlight(text) {
    return text.split('\n').map((line) => {
      if (line.startsWith('#')) return `<span class="c">${escapeHtml(line)}</span>`;
      if (line.startsWith('[[')) return `<span class="h">${escapeHtml(line)}</span>`;
      const match = line.match(/^(\w+)(\s*=\s*)("[^"]*"|'[^']*')(.*)$/);
      if (!match) return escapeHtml(line);
      const [, key, equals, value, rest] = match;
      return `<span class="k">${key}</span>${equals}<span class="s">${escapeHtml(value)}</span>`
        + (rest.trim() ? `<span class="c">${escapeHtml(rest)}</span>` : escapeHtml(rest));
    }).join('\n');
  }

  function flash(button, text) {
    const original = button.dataset.label || button.textContent;
    button.dataset.label = original;
    button.textContent = text;
    setTimeout(() => { button.textContent = original; }, 1600);
  }
  /** Copies text, or (where the clipboard isn't allowed) selects `node` for copying by hand. */
  async function copyText(text, button, node) {
    try {
      await navigator.clipboard.writeText(text);
      flash(button, 'Copied');
    } catch {
      const range = document.createRange();
      range.selectNodeContents(node);
      getSelection().removeAllRanges();
      getSelection().addRange(range);
      flash(button, 'Selected');
    }
  }
  const copyButton = document.getElementById('copy');
  copyButton.addEventListener('click', () => copyText(configText(), copyButton, configOut));
  document.getElementById('download').addEventListener('click', (event) => {
    const link = document.createElement('a');
    link.href = URL.createObjectURL(new Blob([configText()], { type: 'application/toml' }));
    link.download = 'config.toml';
    link.click();
    setTimeout(() => URL.revokeObjectURL(link.href), 1000);
    flash(event.currentTarget, 'Saved');
  });

  /** The preview row and the tiles' faces, drawn with the bar's own code. */
  // Drag a button along the preview row to reorder it (among its own kind).
  const previewCanvas = document.getElementById('preview');
  let drag = null; // { name, grab: finger x minus plate x, x: plate x now }
  const previewX = (event) => {
    const rect = previewCanvas.getBoundingClientRect();
    return ((event.clientX - rect.left) / rect.width) * W;
  };
  previewCanvas.addEventListener('pointerdown', (event) => {
    const x = previewX(event);
    const plate = EXTRAS.find((p) => p.extra && x >= p.x && x < p.x + p.w);
    if (!plate) return;
    previewCanvas.setPointerCapture(event.pointerId);
    previewCanvas.classList.add('dragging');
    drag = { name: plate.id, grab: x - plate.x, x: plate.x };
  });
  previewCanvas.addEventListener('pointermove', (event) => {
    const x = previewX(event);
    if (!drag) {
      previewCanvas.style.cursor = EXTRAS.some((p) => p.extra && x >= p.x && x < p.x + p.w) ? 'grab' : '';
      return;
    }
    // Keep the plate within its own kind's stretch of the row.
    const slots = EXTRAS.filter((p) => p.extra && isMeter(p.id) === isMeter(drag.name));
    drag.x = Math.min(Math.max(x - drag.grab, slots[0].x), slots[slots.length - 1].x);
    // The slot whose centre is nearest the dragged plate's centre is where it goes.
    const centre = drag.x + EXTRA_WIDTH / 2;
    const nearest = slots.reduce((best, p, i) => (
      Math.abs(p.x + EXTRA_WIDTH / 2 - centre) < Math.abs(slots[best].x + EXTRA_WIDTH / 2 - centre) ? i : best), 0);
    const from = slots.findIndex((p) => p.id === drag.name);
    if (nearest !== from) move(drag.name, nearest - from > 0 ? 1 : -1);
  });
  const endDrag = () => {
    if (drag) say(`<b>${EXTRA_INFO[drag.name].title}</b> moved: the config below follows`);
    drag = null;
    previewCanvas.classList.remove('dragging');
  };
  previewCanvas.addEventListener('pointerup', endDrag);
  previewCanvas.addEventListener('pointercancel', endDrag);

  function drawBuilder() {
    try {
      ctx = preview;
      if (drag) {
        // The row without the dragged button, then the button lifted, under the finger.
        drawRow(EXTRAS.filter((p) => p.id !== drag.name));
        const p = EXTRAS.find((plate) => plate.id === drag.name);
        ctx.save();
        ctx.shadowColor = 'rgba(0, 199, 255, 0.8)';
        ctx.shadowBlur = 14;
        drawExtra({ ...p, x: drag.x });
        ctx.restore();
        ctx.strokeStyle = 'rgba(255, 255, 255, 0.85)';
        ctx.lineWidth = 2;
        ctx.beginPath();
        ctx.roundRect(drag.x + 1, 1, EXTRA_WIDTH - 2, H - 2, RADIUS);
        ctx.stroke();
      } else {
        drawRow(EXTRAS);
      }
      for (const [name, { context }] of Object.entries(tiles)) {
        ctx = context;
        ctx.fillStyle = COLOR.background;
        ctx.fillRect(0, 0, EXTRA_WIDTH, H);
        const info = EXTRA_INFO[name];
        drawExtra({ id: name, x: 0, w: EXTRA_WIDTH, icon: name, extra: info }, info.kind === 'toggle');
      }
    } finally {
      ctx = barContext;
    }
  }
  new IntersectionObserver((entries) => {
    builderVisible = entries.some((entry) => entry.isIntersecting);
  }).observe(document.getElementById('build-body'));
  // Phones: the builder is folded away until asked for.
  document.getElementById('build-open').addEventListener('click', (event) => {
    document.getElementById('configure').classList.add('open');
    event.currentTarget.setAttribute('aria-expanded', 'true');
  });
  rebuild();

  applyLevels();
  window.addEventListener('resize', layoutDisplay);
  const fonts = document.fonts ? document.fonts.ready : Promise.resolve();
  const loaded = sprites.complete ? Promise.resolve() : new Promise((resolve) => { sprites.onload = resolve; });
  // For checking the drawing: ?row=function|extras shows that row, ?intro=0.8 freezes the
  // startup animation at 0.8 s, ?lit=vpn lights that toggle.
  const params = new URLSearchParams(location.search);
  if (params.get('row') === 'extras') state.extras = true;
  if (params.get('row') === 'function') state.fnDown = true;
  setTheme(params.get('theme') || 'default');
  for (const name of (params.get('lit') || '').split(',')) {
    if (name === 'vpn') state.lit.vpn = true;
  }
  const frozen = params.has('intro') ? Number(params.get('intro')) : null;

  Promise.all([fonts, loaded]).then(() => {
    drawGallery();
    const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    if (frozen !== null) {
      playIntro();
      intro.running = false;
      if (intro.mask) intro.frozen = frozen;
    } else if (!reduced && !params.has('row')) {
      playIntro();
    }
    requestAnimationFrame(loop);
  });

  // The source links: from the page's fnstrip-repo meta tag, or worked out when served from
  // GitHub Pages (user.github.io/repo/). Without either, the page just says "get the source".
  const host = location.hostname.match(/^([^.]+)\.github\.io$/);
  const repoName = location.pathname.split('/').filter(Boolean)[0];
  const repoUrl = document.querySelector('meta[name="fnstrip-repo"]')?.content
    || (host && repoName ? `https://github.com/${host[1]}/${repoName}` : '');
  if (/^https:\/\/github\.com\/[\w.-]+\/[\w.-]+$/.test(repoUrl)) {
    const repo = document.getElementById('repo');
    repo.href = repoUrl;
    repo.hidden = false;
    for (const link of document.querySelectorAll('.repo-link')) link.href = repoUrl;
    document.querySelector('.repo-foot').hidden = false;
    // Step 2 starts from the source: the clone goes into its commands, so Copy takes it all.
    const clone = document.querySelector('.repo-clone');
    clone.textContent = '';
    clone.append('Get ', Object.assign(document.createElement('a'), { href: repoUrl, textContent: 'the source' }),
      ', build it, and link it in:');
    const commands = clone.closest('li').querySelector('pre code');
    commands.textContent = `git clone ${repoUrl}.git\ncd fnstrip\n${commands.textContent}`;
  }

  // Copy buttons on the install commands.
  for (const button of document.querySelectorAll('.copy-code')) {
    const code = button.closest('.code-head').nextElementSibling;
    button.addEventListener('click', () => copyText(code.textContent, button, code));
  }
})();
