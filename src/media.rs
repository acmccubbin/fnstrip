//! Whether media is playing, for the play/pause button: lit (green, with a spectrum of
//! what the speakers play) while any media player on the session bus reports `Playing`.
//!
//! Read over MPRIS with `busctl`, the same route the bundled desktop provider uses for the
//! media keys. One long-lived `dbus-monitor` listens for players changing state or coming
//! and going, and only then is the state read again, so a quiet desktop costs nothing. If
//! `dbus-monitor` is missing, it falls back to reading every few seconds while the controls
//! row shows.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Without `dbus-monitor`: how often to read while the controls row shows.
const FALLBACK_INTERVAL: Duration = Duration::from_secs(3);
/// With it: a slow safety read, in case a signal was missed.
const SAFETY_INTERVAL: Duration = Duration::from_secs(60);
/// Signals come in bursts (a track change sends several); read once they settle.
const SETTLE: Duration = Duration::from_millis(150);
/// Before restarting a `dbus-monitor` that ended.
const MONITOR_RETRY: Duration = Duration::from_secs(5);
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MATCH_PLAYBACK: &str = "type='signal',interface='org.freedesktop.DBus.Properties',\
    member='PropertiesChanged',path='/org/mpris/MediaPlayer2'";
const MATCH_PLAYERS: &str = "type='signal',interface='org.freedesktop.DBus',\
    member='NameOwnerChanged',arg0namespace='org.mpris.MediaPlayer2'";

#[derive(Default)]
struct Shared {
    watching: AtomicBool,
    playing: AtomicBool,
    /// A signal arrived since the last read.
    changed: AtomicBool,
    /// `dbus-monitor` is running, so reads follow signals.
    monitored: AtomicBool,
}

/// Handle to the playback watcher threads.
pub(crate) struct MediaWatch {
    shared: Arc<Shared>,
    reader: JoinHandle<()>,
}

impl MediaWatch {
    pub(crate) fn start() -> Option<Self> {
        let shared = Arc::new(Shared::default());
        shared.changed.store(true, Ordering::Relaxed);
        let worker = Arc::clone(&shared);
        let reader = thread::Builder::new()
            .name("fnstrip-media".into())
            .spawn(move || read_loop(&worker))
            .ok()?;
        let listener = Arc::clone(&shared);
        let wake = reader.thread().clone();
        thread::Builder::new()
            .name("fnstrip-media-signals".into())
            .spawn(move || listen(&listener, &wake))
            .ok()?;
        Some(Self { shared, reader })
    }

    /// The button shows only while `watching` (the controls row shows). Without
    /// `dbus-monitor`, reads happen only then too.
    pub(crate) fn set_watching(&self, watching: bool) {
        if self.shared.watching.swap(watching, Ordering::Relaxed) != watching
            && watching
            && !self.shared.monitored.load(Ordering::Relaxed)
        {
            self.shared.changed.store(true, Ordering::Relaxed);
            self.reader.thread().unpark();
        }
    }

    pub(crate) fn playing(&self) -> bool {
        self.shared.watching.load(Ordering::Relaxed) && self.shared.playing.load(Ordering::Relaxed)
    }
}

fn read_loop(shared: &Shared) {
    loop {
        let monitored = shared.monitored.load(Ordering::Relaxed);
        let due = shared.changed.swap(false, Ordering::Relaxed)
            || (!monitored && shared.watching.load(Ordering::Relaxed));
        if due {
            thread::sleep(SETTLE);
            shared.changed.store(false, Ordering::Relaxed);
            shared
                .playing
                .store(any_player_playing(), Ordering::Relaxed);
        }
        if shared.changed.load(Ordering::Relaxed) {
            continue;
        }
        thread::park_timeout(if monitored {
            SAFETY_INTERVAL
        } else {
            FALLBACK_INTERVAL
        });
        if shared.monitored.load(Ordering::Relaxed) && !shared.changed.load(Ordering::Relaxed) {
            // Woken by the safety timeout (or spuriously): read anyway, it is rare.
            shared.changed.store(true, Ordering::Relaxed);
        }
    }
}

/// Runs `dbus-monitor` for the MPRIS signals and wakes the reader on each one.
fn listen(shared: &Shared, reader: &thread::Thread) {
    loop {
        let Some(mut monitor) = start_monitor() else {
            shared.monitored.store(false, Ordering::Relaxed);
            return;
        };
        shared.monitored.store(true, Ordering::Relaxed);
        if let Some(stdout) = monitor.stdout.take() {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if line.starts_with("signal ") && !line.contains("member=NameAcquired") {
                    shared.changed.store(true, Ordering::Relaxed);
                    reader.unpark();
                }
            }
        }
        let _ = monitor.kill();
        let _ = monitor.wait();
        shared.monitored.store(false, Ordering::Relaxed);
        reader.unpark();
        thread::sleep(MONITOR_RETRY);
    }
}

fn start_monitor() -> Option<Child> {
    Command::new("dbus-monitor")
        .args(["--session", MATCH_PLAYBACK, MATCH_PLAYERS])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

fn any_player_playing() -> bool {
    let Some(names) = busctl(&["--no-legend", "list"]) else {
        return false;
    };
    names
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| name.starts_with(MPRIS_PREFIX))
        .any(|name| {
            busctl(&[
                "get-property",
                name,
                "/org/mpris/MediaPlayer2",
                "org.mpris.MediaPlayer2.Player",
                "PlaybackStatus",
            ])
            .is_some_and(|status| is_playing(&status))
        })
}

/// `busctl get-property` prints a string property as `s "Playing"`.
fn is_playing(status: &str) -> bool {
    status.trim() == "s \"Playing\""
}

fn busctl(arguments: &[&str]) -> Option<String> {
    let output = Command::new("busctl")
        .args(["--user", "--timeout=1"])
        .args(arguments)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_playing_counts() {
        assert!(is_playing("s \"Playing\"\n"));
        assert!(!is_playing("s \"Paused\"\n"));
        assert!(!is_playing("s \"Stopped\"\n"));
    }
}
