// fnstrip in the browser: the bar on a 2170x60 canvas (the panel's own size), drawn by the
// renderer's own Rust code compiled to WebAssembly (web/, see tools/build_web.sh): rows,
// themes, live faces and the startup animation. This page adds the browser side: touches and
// the Fn gestures, made-up meter readings (the battery is real where the browser shares it),
// the camera and mic (real, and they stay in this tab), a little synthesized music.
(() => {
  'use strict';

  const W = 2170;
  const H = 60;

  const rgb = (r, g, b) => `rgb(${r},${g},${b})`;
  // ---- Themes: colours for the page, and the live faces' tint (src/theme.rs face_tint) -------

  const THEMES = {
    default: { background: [0, 0, 0], plate: [53, 53, 53], pressed: [76, 76, 76], label: [242, 242, 242], dim: [150, 150, 150],
      lit: [32, 128, 64], litPressed: [58, 164, 92], fill: [110, 110, 110], muted: [72, 72, 72], hues: 'plain' },
    pastel: { background: [26, 24, 32], plate: [205, 205, 205], pressed: [236, 236, 236], label: [46, 40, 60], dim: [120, 112, 132],
      lit: [96, 178, 136], litPressed: [128, 200, 162], fill: [160, 160, 160], muted: [190, 190, 190], hues: 'pastel' },
    mono: { background: [0, 0, 0], plate: [26, 26, 26], pressed: [70, 70, 70], label: [255, 255, 255], dim: [110, 110, 110],
      lit: [118, 118, 118], litPressed: [150, 150, 150], fill: [88, 88, 88], muted: [44, 44, 44], hues: 'grey' },
    rainbow: { background: [0, 0, 0], plate: [66, 66, 66], pressed: [100, 100, 100], label: [242, 242, 242], dim: [150, 150, 150],
      lit: [225, 30, 120], litPressed: [245, 80, 155], fill: [108, 108, 108], muted: [80, 80, 80], hues: 'rainbow' },
  };
  let theme = THEMES.default;
  let fahrenheit = false; // the builder's temperature setting
  let themeName = 'default';
  const COLOR = {};

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

  const RADIUS = 8;
  const SLIDER_ICON = 72;

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
  let keptConfig = null; // the config last kept (page start, copy or download)
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

  // ---- Drawing the rows: fnstrip's own code ---------------------------------------------------
  // Everything on the bar (plates, sliders, keys, icons, themes, the meters, the camera and
  // spectrum faces) and the startup animation is drawn by the renderer's own Rust code,
  // compiled for the browser (web/, embedded by tools/build_web.sh), so this bar matches the
  // real one pixel for pixel. The page feeds it: made-up meter readings, audio, the camera.

  let rust = null; // the WebAssembly exports, once loaded
  const RUST_THEMES = ['default', 'pastel', 'mono', 'rainbow']; // Theme::NAMES
  const RUST_ICONS = ['camera', 'mic', 'vpn', 'cpu', 'mem', 'disk', 'net', 'battery', 'temp', 'fan']; // ExtraIcon::NAMES
  const blankRow = new ImageData(W, H);
  let themePlain = false; // the themes gallery: themes without the builder's colour changes
  // A view straight into the drawing code's frame (no copy): used at once, before the next draw.
  const rustPixels = (pointer) => new ImageData(new Uint8ClampedArray(rust.memory.buffer, pointer, W * H * 4), W, H);

  /** Whether extras plate `p` shows a live face (drawn here, over the plate the row leaves). */
  function liveFace(p, showLit) {
    const lit = state.lit[p.id] || (showLit && p.extra.kind === 'toggle');
    if (p.id === 'camera') return lit && camera.ready;
    if (p.id === 'mic') return lit && mic.ready;
    if (p.id === 'vpn') return lit;
    return p.extra.kind === 'meter';
  }

  /** The touched button, as web/'s `pressed` code. */
  function pressedCode(plates) {
    const id = state.pressed;
    if (!id) return 0;
    const fixed = { esc: 1, previous: 2, playpause: 3, next: 4 }[id];
    if (fixed) return fixed;
    const key = /^F(\d+)$/.exec(id);
    if (key) return 10 + Number(key[1]);
    const index = plates.filter((p) => p.extra).findIndex((p) => p.id === id);
    return index >= 0 ? 100 + index : 0;
  }

  /** Row `plates`, live faces and all, drawn by the renderer's code: RGBA pixels.
   *  Pixels, not a shared canvas: Safari copies canvases at the end of the frame, so a row
   *  drawn later into a shared one would land in every place it was copied to. */
  function renderRow(plates, showLit = false) {
    if (!rust) return blankRow;
    const layer = plates === CONTROLS ? 0 : plates === FUNCTION ? 1 : 2;
    rust.theme(Math.max(0, RUST_THEMES.indexOf(themeName)));
    if (!themePlain) {
      for (const [key, [r, g, b]] of Object.entries(overrides)) rust.color(Object.keys(COLOR_KEYS).indexOf(key), (r << 16) | (g << 8) | b);
    }
    const extras = plates.filter((p) => p.extra);
    let first = 0;
    let rest = 0;
    extras.forEach((p, i) => {
      const icon = RUST_ICONS.indexOf(p.id);
      if (i < 5) first |= icon << (i * 4);
      else rest |= icon << ((i - 5) * 4);
    });
    rust.extras(extras.length, first, rest);
    const playing = layer === 0 && music.playing;
    rust.levels(state.levels.brightness, state.levels.keyboard, state.levels.volume, (state.muted ? 1 : 0) | (playing ? 6 : 0));
    let on = 0;
    let live = 0;
    extras.forEach((p, i) => {
      if (state.lit[p.id] || (showLit && p.extra.kind === 'toggle')) on |= 1 << i;
      if (liveFace(p, showLit)) live |= 1 << i;
    });
    rust.extras_state(on, live);
    rust.pressed(pressedCode(plates));
    return rustPixels(rust.row(layer));
  }

  function drawRow(plates = layout()) {
    ctx.putImageData(renderRow(plates), 0, 0);
  }

  // ---- Meters: made-up readings, fed to the renderer's own meters (src/meters.rs) ----------
  // Each reading in the meter's own units (Meter::push_sample), by ExtraIcon::NAMES index.

  const METER = { vpn: 2, cpu: 3, mem: 4, disk: 5, net: 6, fan: 9 };
  const sim = { busy: 0, load: 0.1, temp: 50, fans: [2400, 2250], mem: 41, disk: 0, net: 60e3, vpn: 20e3 };
  const real = { battery: null };
  const battery = { level: 0.68, charging: true, hours: 0.75 }; // made up, unless the browser shares it
  const jitter = (amount) => (Math.random() - 0.5) * 2 * amount;

  function sample() {
    if (!rust) return;
    sim.busy *= 0.9;
    const target = 0.07 + 0.55 * Math.min(1, sim.busy) + (Math.random() < 0.06 ? 0.3 * Math.random() : 0);
    sim.load += (target - sim.load) * 0.35;
    const load = Math.max(0.02, Math.min(0.98, sim.load + jitter(0.03)));
    rust.meter_sample(METER.cpu, load * 0.72, load * 0.28);
    sim.temp += (46 + load * 48 - sim.temp) * 0.08 + jitter(0.4);
    rust.meter_celsius(sim.temp);
    const heat = Math.max(0, Math.min(1, (sim.temp - 48) / 40));
    sim.fans[0] += (2160 + heat * 3600 - sim.fans[0]) * 0.1 + jitter(25);
    sim.fans[1] += (2000 + heat * 3300 - sim.fans[1]) * 0.1 + jitter(25);
    rust.meter_sample(METER.fan, sim.fans[0], sim.fans[1]);
    sim.mem += (41 + 6 * Math.min(1, sim.busy) - sim.mem) * 0.05 + jitter(0.15);
    rust.meter_sample(METER.mem, sim.mem / 100, (22 + jitter(0.3)) / 100);
    const burst = Math.random() < 0.08;
    rust.meter_sample(METER.disk, burst ? 2e6 * Math.random() : 15e3 * Math.random(),
      Math.random() < 0.12 ? 1.2e6 * Math.random() : 8e3 * Math.random());
    sim.net = Math.max(2e3, sim.net * (0.85 + 0.3 * Math.random()) + (Math.random() < 0.05 ? 400e3 * Math.random() : 0));
    rust.meter_sample(METER.net, sim.net, sim.net * (0.15 + 0.2 * Math.random()));
    // Bursts that rise and settle, like real traffic, rather than noise.
    sim.vpnRaw = Math.max(1e3, (sim.vpnRaw ?? 20e3) * (0.9 + 0.2 * Math.random()) + (Math.random() < 0.04 ? 80e3 * Math.random() : 0));
    sim.vpn = sim.vpn * 0.75 + sim.vpnRaw * 0.25;
    rust.meter_sample(METER.vpn, sim.vpn, sim.vpn * (0.4 + 0.4 * Math.random()));
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
    rust.battery(battery.level, battery.charging ? 1 : 0, Number.isFinite(battery.hours) ? battery.hours : -1);
  }
  /** Once the drawing code is loaded: the fans' ranges (the tested laptop's), a full graph. */
  function startMeters() {
    rust.fan_ranges(2160, 5927, 2000, 5489);
    rust.fahrenheit(fahrenheit ? 1 : 0);
    for (let i = 0; i < 91; i += 1) sample();
    setInterval(sample, 250);
  }
  if (navigator.getBattery) {
    navigator.getBattery().then((b) => { real.battery = b; }).catch(() => {});
  }

  // ---- Camera and mic (real, local only) --------------------------------------------------

  const camera = {
    video: null,
    stream: null,
    ready: false,
    starting: false,
    async start() {
      this.starting = true;
      try {
        await this.open();
      } finally {
        this.starting = false;
      }
    },
    async open() {
      this.stream = await navigator.mediaDevices.getUserMedia({ video: { width: 640, height: 360 }, audio: false });
      this.video = document.createElement('video');
      this.video.muted = true;
      this.video.playsInline = true;
      this.video.srcObject = this.stream;
      // On the page (tiny and nearly invisible): browsers slow a video that isn't to about
      // a frame a second, and the bar copies its frames.
      this.video.className = 'camera-source';
      this.video.setAttribute('aria-hidden', 'true');
      document.body.append(this.video);
      await this.video.play();
      this.ready = true;
    },
    stop() {
      this.stream?.getTracks().forEach((t) => t.stop());
      this.stream = null;
      this.ready = false;
      this.video?.remove();
      rust?.camera_ready(0);
    },
    /** The test screen's camera picture, at the size it asks for (width << 16 | height). */
    testCanvas: document.createElement('canvas'),
    feedTest(size) {
      const width = size >> 16;
      const height = size & 0xffff;
      if (!width || !height) return;
      const canvas = this.testCanvas;
      if (canvas.width !== width || canvas.height !== height) Object.assign(canvas, { width, height });
      const g = canvas.getContext('2d', { willReadFrequently: true });
      this.cover(g, width, height);
      const picture = g.getImageData(0, 0, width, height).data;
      new Uint8Array(rust.memory.buffer, rust.test_camera_buffer(), picture.length).set(picture);
      rust.test_camera_commit();
    },
    /** The video into a width x height canvas: its full width, a band through the middle, mirrored. */
    cover(g, width, height) {
      const v = this.video;
      const vw = v.videoWidth || 640;
      const vh = v.videoHeight || 360;
      const bandHeight = (vw * height) / width;
      g.save();
      g.translate(width, 0);
      g.scale(-1, 1);
      g.drawImage(v, 0, Math.max(0, (vh - bandHeight) / 2), vw, Math.min(vh, bandHeight), 0, 0, width, height);
      g.restore();
    },
    /** The picture for the camera button, 182x60, handed to the drawing code each frame. */
    canvas: Object.assign(document.createElement('canvas'), { width: EXTRA_WIDTH, height: H }),
    update() {
      if (!this.ready || !rust) return;
      const g = this.canvas.getContext('2d', { willReadFrequently: true });
      this.cover(g, EXTRA_WIDTH, H);
      const picture = g.getImageData(0, 0, EXTRA_WIDTH, H).data;
      new Uint8Array(rust.memory.buffer, rust.camera_buffer(), picture.length).set(picture);
      rust.camera_ready(1);
    },
  };

  const mic = {
    stream: null,
    context: null,
    analyser: null,
    ready: false,
    starting: false,
    async start() {
      this.starting = true;
      try {
        await this.open();
      } finally {
        this.starting = false;
      }
    },
    async open() {
      this.stream = await navigator.mediaDevices.getUserMedia({ audio: true, video: false });
      // 48 kHz, as the bar listens, so the spectrum's bands land on the same notes.
      this.context = new AudioContext({ sampleRate: 48000 });
      const source = this.context.createMediaStreamSource(this.stream);
      this.analyser = this.context.createAnalyser();
      this.analyser.fftSize = 2048;
      source.connect(this.analyser);
      this.ready = true;
    },
    stop() {
      this.stream?.getTracks().forEach((t) => t.stop());
      this.context?.close();
      this.stream = null;
      this.ready = false;
    },
    /** The latest samples to the drawing code's spectrum (src/spectrum.rs). */
    update(dt) {
      if (this.ready && rust) feedAudio(0, this.analyser, dt);
    },
  };

  /** The analyser's latest 2048 samples into the drawing code's audio buffer `which`. */
  function feedAudio(which, analyser, dt) {
    analyser.getFloatTimeDomainData(new Float32Array(rust.memory.buffer, rust.audio_buffer(which), 2048));
    rust.audio_update(which, dt);
  }

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
    setup() {
      if (this.context) return;
      this.context = new AudioContext({ sampleRate: 48000 });
      const c = this.context;
      this.bus = c.createGain();
      this.bus.gain.value = 0.5;
      this.analyser = c.createAnalyser();
      this.analyser.fftSize = 2048;
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
    // Follows the page's volume slider and mute; hands what's playing to the spectrum.
    update(dt) {
      if (!this.context) return;
      const target = this.playing && !state.muted ? 0.9 * (state.levels.volume / 100) ** 2 : 0;
      if (Math.abs(target - this.gain) > 0.001) {
        this.output.gain.setTargetAtTime(target, this.context.currentTime, 0.03);
        this.gain = target;
      }
      if (this.playing && rust) feedAudio(1, this.analyser, dt);
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

  // ---- The test screen: the renderer's own (src/screensaver.rs) ---------------------------
  // Triple-tap fn and hold. Its scenes, frame-rate and render-time stats, buffer counts and
  // tap zones are the bar's own code, timed by this browser. The
  // page supplies what a scene asks for: your mic, what's playing (the tune), your camera. The
  // CPU figures read the bar's /proc, so here they show "-".

  const testScreen = {
    running: false,
    usingMic: false,
    usingCamera: false,
    start() {
      if (!rust) return;
      rust.test_start();
      this.running = true;
      say('<b>test screen</b>: tap the left end for the next scene, the stats for the buffer count, anywhere else (or fn) to leave <span class="cmd">(debug_gestures = "on")</span>');
    },
    stop() {
      if (!this.running) return;
      this.running = false;
      rust?.test_stop();
      this.letGo();
    },
    /** Stops the mic or camera if only the test screen had started them. */
    letGo(wants = 0) {
      if (this.usingMic && !(wants & 1)) {
        if (!state.lit.mic) mic.stop();
        this.usingMic = false;
      }
      if (this.usingCamera && !(wants & 4)) {
        if (!state.lit.camera) camera.stop();
        this.usingCamera = false;
      }
    },
    tap(x) {
      const zone = rust.test_zone(Math.max(0, Math.round(x)));
      if (zone === 0) rust.test_next_scene();
      else if (zone === 1) rust.test_next_buffering();
      else this.stop();
    },
    draw(dt) {
      const wants = rust.test_wants();
      this.letGo(wants);
      if (wants & 1 && !mic.ready && !mic.starting) {
        this.usingMic = true;
        mic.start().catch(() => say('<b>mic scene</b>: the browser didn\'t allow the mic'));
      }
      if (wants & 2 && !music.playing) {
        // Nothing playing: silence (tap play first for something to see).
        new Float32Array(rust.memory.buffer, rust.audio_buffer(1), 2048).fill(0);
        rust.audio_update(1, dt);
      }
      if (wants & 4) {
        if (!camera.ready && !camera.starting) {
          this.usingCamera = true;
          camera.start().catch(() => say('<b>camera scene</b>: the browser didn\'t allow the camera'));
        } else if (camera.ready) {
          camera.feedTest(rust.test_camera_size());
        }
      }
      const pointer = rust.test_frame();
      if (pointer) ctx.putImageData(rustPixels(pointer), 0, 0);
      rust.test_tick();
    },
  };

  // ---- The startup animation: the renderer's own (src/intro.rs), over the row ---------------

  let DURATION = 1.8; // seconds; the renderer's own once loaded
  const intro = { running: false, start: 0, frozen: undefined };
  /** The animation at `t` seconds, over the row last drawn. */
  function drawIntro(t) {
    if (rust) ctx.putImageData(rustPixels(rust.intro(Math.round(t * 1000))), 0, 0);
  }

  function playIntro() {
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

  /** No live bar here: recordings and pictures instead (index.html's .fallback-only). */
  function fallBack(error) {
    if (error) console.error(error);
    rust = null;
    document.documentElement.classList.add('no-live');
    for (const video of document.querySelectorAll('video.fallback-only')) {
      video.preload = 'auto';
      video.play().catch(() => {});
    }
  }

  let lastFrame = null;
  let failures = 0;
  function loop(now) {
    requestAnimationFrame(loop); // first, so one bad frame can't stop the bar
    try {
      layoutDisplay();
      const dt = Math.min(0.1, (now - (lastFrame ?? now)) / 1000);
      lastFrame = now;
      mic.update(dt);
      music.update(dt);
      camera.update();
      if (testScreen.running) testScreen.draw(dt);
      else drawRow();
      if (testScreen.running) {
        // The test screen owns the bar.
      } else if (intro.frozen !== undefined) {
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
      // The drawing code itself failing over and over (not other page code): show the
      // recordings instead.
      if (error instanceof WebAssembly.RuntimeError) failures += 1;
      if (failures > 30 && rust) fallBack(error);
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
  // Starting layouts, one click each.
  const STARTERS = {
    default: ['mic', 'cpu', 'mem', 'disk', 'net', 'battery'],
    simple: ['mic', 'cpu', 'mem', 'battery'],
    meters: ['cpu', 'temp', 'fan', 'mem', 'disk', 'net', 'battery'],
  };
  for (const button of document.querySelectorAll('.starter')) {
    button.addEventListener('click', () => {
      builderUsed = true;
      const names = STARTERS[button.dataset.starter];
      chosen.filter((name) => !names.includes(name)).forEach(switchOff);
      chosen = grouped(names.slice());
      rebuild();
      say(`<b>${button.textContent} layout</b>: double-tap fn to see it on the bar`);
    });
  }
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
    themePlain = true;
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
      themePlain = false;
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
    if (announce) say(`<b>theme = "${themeName}"</b> in the config. Double-tap fn (or <kbd>F</kbd>) to see it on your buttons and meters too.`);
  }
  themeSelect.addEventListener('change', () => setTheme(themeSelect.value));
  const unitSelect = document.getElementById('temperature-unit');
  unitSelect.addEventListener('change', () => {
    fahrenheit = unitSelect.value === 'f';
    rust?.fahrenheit(fahrenheit ? 1 : 0);
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
      `debug_gestures = "${debugGestures.checked ? 'on' : 'off'}"   # test screen, theme switching`,
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
  copyButton.addEventListener('click', () => {
    copyText(configText(), copyButton, configOut);
    keptConfig = configText(); // copied: nothing to lose by leaving
  });
  document.getElementById('download').addEventListener('click', (event) => {
    const link = document.createElement('a');
    link.href = URL.createObjectURL(new Blob([configText()], { type: 'application/toml' }));
    link.download = 'config.toml';
    link.click();
    setTimeout(() => URL.revokeObjectURL(link.href), 1000);
    flash(event.currentTarget, 'Saved');
    keptConfig = configText(); // downloaded: nothing to lose by leaving
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

  let tilesDrawn = -Infinity;
  const lifted = Object.assign(document.createElement('canvas'), { width: EXTRA_WIDTH, height: H });
  function drawBuilder() {
    try {
      ctx = preview;
      if (drag) {
        // The row with the dragged button's place empty, then the button lifted, under the finger.
        const p = EXTRAS.find((plate) => plate.id === drag.name);
        const row = renderRow(EXTRAS);
        ctx.putImageData(row, 0, 0);
        ctx.fillStyle = COLOR.background;
        ctx.fillRect(p.x, 0, p.w, H);
        // The lifted button, cut into its own canvas so it can cast a glow.
        lifted.getContext('2d').putImageData(row, -p.x, 0, p.x, 0, p.w, H);
        ctx.save();
        ctx.shadowColor = 'rgba(0, 199, 255, 0.8)';
        ctx.shadowBlur = 14;
        ctx.drawImage(lifted, drag.x, 0);
        ctx.restore();
        ctx.strokeStyle = 'rgba(255, 255, 255, 0.85)';
        ctx.lineWidth = 2;
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(drag.x + 1, 1, EXTRA_WIDTH - 2, H - 2, RADIUS);
        else ctx.rect(drag.x + 1, 1, EXTRA_WIDTH - 2, H - 2);
        ctx.stroke();
      } else {
        drawRow(EXTRAS);
      }
      // The palette's tiles: each button cut from the whole row (toggles lit). Redrawn four
      // times a second (the meters' rate), or at once while the camera or mic is live.
      const now = performance.now();
      if (now - tilesDrawn < 250 && !camera.ready && !mic.ready) return;
      tilesDrawn = now;
      const row = renderRow(SHOWCASE, true);
      for (const [name, { context }] of Object.entries(tiles)) {
        const p = SHOWCASE.find((plate) => plate.id === name);
        context.putImageData(row, -p.x, 0, p.x, 0, EXTRA_WIDTH, H);
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
  // For checking the drawing: ?row=function|extras shows that row, ?intro=0.8 freezes the
  // startup animation at 0.8 s, ?lit=vpn lights that toggle.
  const params = new URLSearchParams(location.search);
  if (params.get('row') === 'extras') state.extras = true;
  if (params.get('row') === 'function') state.fnDown = true;
  setTheme(params.get('theme') || 'default');
  // The config as the page started; leaving with a different one (not yet copied or
  // downloaded) asks first, so a row you've been trying out isn't lost by accident.
  keptConfig = configText();
  window.addEventListener('beforeunload', (event) => {
    if (configText() === keptConfig) return;
    event.preventDefault();
    event.returnValue = '';
  });
  // "Try it on the bar": back up to the bar, showing your extras row.
  for (const button of document.querySelectorAll('.try-it')) {
    button.addEventListener('click', () => {
      builderUsed = true;
      if (testScreen.running) testScreen.stop();
      state.extras = chosen.length > 0;
      display.scrollIntoView({ behavior: 'smooth', block: 'center' });
      say(state.extras
        ? '<b>your extras row</b>: tap your buttons, press fn to go back to the controls'
        : '<b>no extras picked</b>: pick some buttons in the builder below');
    });
  }
  // ?levels=35,50,30 sets brightness, keyboard and volume (for screenshots and tests).
  const levels = (params.get('levels') || '').split(',').map(Number);
  if (levels.length === 3 && levels.every((v) => v >= 0 && v <= 100)) {
    [state.levels.brightness, state.levels.keyboard, state.levels.volume] = levels;
    applyLevels();
  }
  for (const name of (params.get('lit') || '').split(',')) {
    if (name === 'vpn') state.lit.vpn = true;
  }
  const frozen = params.has('intro') ? Number(params.get('intro')) : null;

  // fnstrip's own drawing code, embedded by tools/build_web.sh (works from file:// too).
  // Without it (an older browser, or a failure), the page falls back to recordings.
  const drawing = (typeof WebAssembly === 'object'
    ? WebAssembly.instantiate(Uint8Array.from(atob(window.FNSTRIP_WASM || ''), (c) => c.charCodeAt(0)),
      { env: { now_ms: () => performance.now() } }) // the clock fnstrip's code reads
    : Promise.reject(new Error('no WebAssembly')))
    .then(({ instance }) => {
      rust = instance.exports;
      rust.start();
      DURATION = rust.intro_ms() / 1000;
      startMeters();
    })
    .catch(fallBack);

  Promise.all([fonts, drawing]).then(() => {
    drawGallery();
    const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    if (frozen !== null) {
      playIntro();
      intro.running = false;
      intro.frozen = frozen;
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
    for (const link of document.querySelectorAll('.repo-link')) {
      link.href = repoUrl;
      link.hidden = false;
    }
    document.querySelector('.repo-foot').hidden = false;
    // Step 2 starts from the source: the clone goes into its commands, so Copy takes it all.
    const clone = document.querySelector('.repo-clone');
    clone.textContent = '';
    clone.append('Get ', Object.assign(document.createElement('a'), { href: repoUrl, textContent: 'the source' }),
      ', build it, and link it in:');
    const commands = clone.closest('li').querySelector('pre code');
    commands.textContent = `git clone ${repoUrl}.git\ncd fnstrip\n${commands.textContent}`;
  }

  // The latest release, from the fnstrip-version meta tag (filled in when published).
  const version = document.querySelector('meta[name="fnstrip-version"]')?.content;
  if (/^\d+\.\d+\.\d+$/.test(version || '')) {
    for (const element of document.querySelectorAll('.version')) {
      element.querySelector('.version-number').textContent = version;
      const link = element.querySelector('.version-link');
      link.href = repoUrl ? `${repoUrl}/releases/tag/v${version}` : '#install';
      element.hidden = false;
    }
  }

  // The recordings: each video loads and plays only while on screen (none load up front),
  // and with reduced motion, just shows its first frame.
  const stillOnly = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  const recordings = document.querySelectorAll('.strip video');
  if (stillOnly) {
    recordings.forEach((v) => { v.preload = 'metadata'; });
  } else {
    const watcher = new IntersectionObserver((entries) => {
      for (const { target, isIntersecting } of entries) {
        if (isIntersecting) target.play().catch(() => {});
        else target.pause();
      }
    }, { rootMargin: '120px 0px' });
    recordings.forEach((v) => watcher.observe(v));
  }

  // The reference folds away on phones (each part opens with a tap); open on wider screens.
  if (window.matchMedia('(max-width: 860px)').matches) {
    document.querySelectorAll('details.ref').forEach((d) => { d.open = false; });
  }

  // A link to a folded FAQ answer (#other-macs) opens it.
  function openLinkedAnswer() {
    const target = location.hash && document.querySelector(`details${location.hash}`);
    if (target) target.open = true;
  }
  window.addEventListener('hashchange', openLinkedAnswer);
  openLinkedAnswer();

  // Floating jump arrows: hidden near the top (the page's own links are right there), up
  // and down in between, up only at the bottom.
  {
    const jump = document.querySelector('.jump');
    const up = jump.querySelector('.jump-up');
    const down = jump.querySelector('.jump-down');
    const place = () => {
      const y = window.scrollY;
      const bottom = document.documentElement.scrollHeight - window.innerHeight;
      jump.hidden = y < 400;
      up.hidden = false;
      down.hidden = y >= bottom - 120;
    };
    up.addEventListener('click', () => window.scrollTo({ top: 0 }));
    down.addEventListener('click', () => window.scrollTo({ top: document.documentElement.scrollHeight }));

    // The address follows the reading position, so a bookmark or copied link opens where you
    // are: the section in view (#themes), or the plain page at the top. It replaces the
    // address rather than adding history, and a link to something inside the section in view
    // (#other-macs in the FAQ) is kept.
    const sections = [...document.querySelectorAll('main section[id], body > section[id]')];
    const follow = () => {
      const y = window.scrollY;
      const atBottom = y >= document.documentElement.scrollHeight - window.innerHeight - 4;
      const line = window.innerHeight * 0.3;
      let current = null;
      if (atBottom) {
        current = sections[sections.length - 1];
      } else {
        for (const section of sections) {
          if (section.getBoundingClientRect().top <= line) current = section;
        }
      }
      const linked = location.hash.length > 1 && document.getElementById(location.hash.slice(1));
      if (current && linked && current.contains(linked)) return;
      const wanted = current ? `#${current.id}` : '';
      if (location.hash === wanted) return;
      try {
        history.replaceState(history.state, '', `${location.pathname}${location.search}${wanted}`);
      } catch {
        // Some browsers limit how often the address may change; the next scroll catches up.
      }
    };
    let pending = false;
    const onScroll = () => {
      place();
      if (pending) return;
      pending = true;
      setTimeout(() => {
        pending = false;
        follow();
      }, 150);
    };
    window.addEventListener('scroll', onScroll, { passive: true });
    window.addEventListener('resize', place);
    place();
  }

  // Copy buttons on the install commands.
  for (const button of document.querySelectorAll('.copy-code')) {
    const code = button.closest('.code-head').nextElementSibling;
    button.addEventListener('click', () => copyText(code.textContent, button, code));
  }
})();
