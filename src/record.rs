//! Debug recording: while `$XDG_RUNTIME_DIR/fnstrip-record` exists, every input frame,
//! action, desktop report, light change, submitted frame and release is logged with a
//! wall-clock timestamp to `~/.cache/fnstrip-record/events.log`, and each submitted frame's
//! pixels are appended to `frames.raw` (XRGB8888, one whole frame each, in submit order).
//! `tools/record_video.py` turns the two into a video with the touches drawn on.
//!
//! Off (the flag file absent) it costs one atomic load per call, plus a file check every
//! half second.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const FLAG: &str = "fnstrip-record";
const CHECK_INTERVAL: Duration = Duration::from_millis(500);

static ACTIVE: AtomicBool = AtomicBool::new(false);
static FILES: Mutex<Option<Files>> = Mutex::new(None);
static CHECKED: Mutex<Option<Instant>> = Mutex::new(None);

struct Files {
    events: File,
    frames: File,
}

fn flag() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join(FLAG))
}

fn directory() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".cache/fnstrip-record"))
}

/// Starts or stops recording as the flag file appears or goes (checked twice a second).
pub(crate) fn poll() {
    {
        let mut checked = CHECKED.lock().unwrap_or_else(PoisonError::into_inner);
        if checked.is_some_and(|at| at.elapsed() < CHECK_INTERVAL) {
            return;
        }
        *checked = Some(Instant::now());
    }
    let wanted = flag().is_some_and(|flag| flag.exists());
    if wanted == ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let mut files = FILES.lock().unwrap_or_else(PoisonError::into_inner);
    if wanted {
        let Some(directory) = directory() else { return };
        let _ = fs::create_dir_all(&directory);
        let open = |name: &str| {
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(directory.join(name))
        };
        if let (Ok(events), Ok(frames)) = (open("events.log"), open("frames.raw")) {
            *files = Some(Files { events, frames });
            ACTIVE.store(true, Ordering::Relaxed);
        }
    } else {
        *files = None;
        ACTIVE.store(false, Ordering::Relaxed);
    }
}

#[must_use]
pub(crate) fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.0
}

/// Logs one event line (callers build `text` only when [`active`]).
pub(crate) fn event(text: &str) {
    if !active() {
        return;
    }
    if let Some(files) = FILES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
    {
        let _ = writeln!(files.events, "{:.3} {text}", now_ms());
    }
}

/// Logs a submitted frame and appends its pixels.
pub(crate) fn frame(frame_id: u64, damage: &str, pixels: &[u8]) {
    if !active() {
        return;
    }
    if let Some(files) = FILES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
    {
        let _ = writeln!(files.events, "{:.3} submit {frame_id} {damage}", now_ms());
        let _ = files.frames.write_all(pixels);
    }
}
