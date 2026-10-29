//! Now-playing info and transport controls through MPRIS, the D-Bus
//! interface every Linux player (Spotify, Firefox, Chromium, Rhythmbox,
//! VLC, mpv with its plugin...) implements; GNOME's own media controls use
//! it too.
//!
//! One worker thread listens for `PropertiesChanged` on the players'
//! object path and for players appearing and leaving the bus, so it sleeps
//! until something actually changes (plus a slow safety refresh) and costs
//! nothing while the music is idle. Snapshots go to the UI through a
//! channel, and `on_change` wakes the UI.

use crate::sys::dbus;
use crate::{PlatformError, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use zbus::zvariant::OwnedValue;

/// Album art, decoded and downscaled to at most [`ART_SIZE`] square.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Art {
    pub width: usize,
    pub height: usize,
    /// Unpremultiplied RGBA8.
    pub rgba: Vec<u8>,
}

pub const ART_SIZE: u32 = 96;

/// What is playing right now.
#[derive(Clone, Debug)]
pub struct NowPlaying {
    /// The player's name, e.g. "Spotify" or "Firefox".
    pub source: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub can_previous: bool,
    pub can_next: bool,
    pub can_toggle: bool,
    /// Position and duration in seconds when the app reports a timeline.
    pub timeline: Option<(f64, f64)>,
    /// When `timeline` was sampled; a playing position advances from here.
    pub captured: Instant,
    /// Shared between snapshots of the same track.
    pub art: Option<Arc<Art>>,
}

impl NowPlaying {
    /// Position and duration (seconds) extrapolated to now.
    pub fn position(&self) -> Option<(f64, f64)> {
        let (pos, dur) = self.timeline?;
        let pos = if self.playing {
            pos + self.captured.elapsed().as_secs_f64()
        } else {
            pos
        };
        Some((pos.clamp(0.0, dur), dur))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaCommand {
    PlayPause,
    Next,
    Previous,
}

enum Msg {
    Wake,
    Command(MediaCommand),
}

/// Handle to the worker thread. Dropping it stops the worker.
pub struct MediaWatcher {
    tx: Sender<Msg>,
    rx: Receiver<Option<NowPlaying>>,
}

impl MediaWatcher {
    /// Starts the worker. `on_change` runs (on the worker thread) after each
    /// new snapshot.
    pub fn spawn(on_change: impl Fn() + Send + 'static) -> Result<Self> {
        let (tx, worker_rx) = mpsc::channel();
        let (snap_tx, rx) = mpsc::channel();
        let wake = tx.clone();
        std::thread::Builder::new()
            .name("nook-media".into())
            .spawn(move || {
                if let Err(e) = run(&worker_rx, wake, &snap_tx, &on_change) {
                    tracing::warn!("media controls unavailable: {e}");
                }
            })
            .map_err(|e| PlatformError::other("spawning media thread", e))?;
        Ok(Self { tx, rx })
    }

    /// Latest snapshot since the last call, if any (`Some(None)` means the
    /// player went away).
    pub fn poll(&self) -> Option<Option<NowPlaying>> {
        self.rx.try_iter().last()
    }

    pub fn send(&self, cmd: MediaCommand) {
        let _ = self.tx.send(Msg::Command(cmd));
    }
}

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const PROPS: &str = "org.freedesktop.DBus.Properties";

/// Forwards bus signals that may change what is playing as wake-ups.
fn listen(wake: Sender<Msg>) -> Result<()> {
    let conn = dbus::session()?;
    let rules = [
        zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface(PROPS)
            .and_then(|b| b.member("PropertiesChanged"))
            .and_then(|b| b.path(PATH))
            .map(|b| b.build()),
        zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.freedesktop.DBus")
            .and_then(|b| b.member("NameOwnerChanged"))
            .map(|b| b.build()),
    ];
    for rule in rules {
        let rule = rule.map_err(|e| PlatformError::other("media match rule", e))?;
        let iter = zbus::blocking::MessageIterator::for_match_rule(rule, conn, Some(64))
            .map_err(|e| PlatformError::other("subscribing to players", e))?;
        let wake = wake.clone();
        std::thread::Builder::new()
            .name("nook-media-signals".into())
            .spawn(move || {
                for msg in iter.flatten() {
                    let relevant = match msg.header().member().map(|m| m.as_str()) {
                        Some("NameOwnerChanged") => msg
                            .body()
                            .deserialize::<(String, String, String)>()
                            .is_ok_and(|(name, _, _)| name.starts_with(PREFIX)),
                        _ => true,
                    };
                    if relevant && wake.send(Msg::Wake).is_err() {
                        return;
                    }
                }
            })
            .map_err(|e| PlatformError::other("spawning media listener", e))?;
    }
    Ok(())
}

fn run(
    rx: &Receiver<Msg>,
    wake: Sender<Msg>,
    snaps: &Sender<Option<NowPlaying>>,
    on_change: &dyn Fn(),
) -> Result<()> {
    listen(wake)?;
    let mut state = State::default();
    let mut last_sent: Option<Option<Key>> = None;
    loop {
        let snap = state.snapshot();
        let key = snap.as_ref().map(Key::of);
        if last_sent.as_ref() != Some(&key) {
            last_sent = Some(key);
            if snaps.send(snap).is_err() {
                return Ok(());
            }
            on_change();
        }
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Msg::Command(cmd)) => state.command(cmd),
            Ok(Msg::Wake) => {
                // Players send bursts (metadata, then status, then art).
                std::thread::sleep(Duration::from_millis(60));
                while let Ok(m) = rx.try_recv() {
                    if let Msg::Command(cmd) = m {
                        state.command(cmd);
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

/// What makes two snapshots different for the UI (the position moves on its
/// own between them).
#[derive(Clone, Debug, PartialEq)]
struct Key {
    source: String,
    title: String,
    artist: String,
    playing: bool,
    can: (bool, bool, bool),
    duration: Option<i64>,
    position_s: Option<i64>,
    art: bool,
}

impl Key {
    fn of(n: &NowPlaying) -> Self {
        Key {
            source: n.source.clone(),
            title: n.title.clone(),
            artist: n.artist.clone(),
            playing: n.playing,
            can: (n.can_previous, n.can_next, n.can_toggle),
            duration: n.timeline.map(|(_, d)| d as i64),
            // Paused, or seeked: the reported position jumps.
            position_s: n.timeline.map(|(p, _)| {
                let p = if n.playing {
                    p - n.captured.elapsed().as_secs_f64()
                } else {
                    p
                };
                (p / 2.0) as i64
            }),
            art: n.art.is_some(),
        }
    }
}

#[derive(Default)]
struct State {
    /// The bus name of the player shown.
    current: Option<String>,
    art: Option<(String, Option<Arc<Art>>)>,
}

type Props = HashMap<String, OwnedValue>;

fn players() -> Vec<String> {
    dbus::call::<_, Vec<String>>(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "ListNames",
        &(),
    )
    .unwrap_or_default()
    .into_iter()
    .filter(|n| n.starts_with(PREFIX))
    .collect()
}

fn props(name: &str, iface: &str) -> Option<Props> {
    dbus::call::<_, Props>(name, PATH, PROPS, "GetAll", &(iface,)).ok()
}

fn status(p: &Props) -> &'static str {
    match p
        .get("PlaybackStatus")
        .and_then(|v| dbus::as_str(v))
        .as_deref()
    {
        Some("Playing") => "Playing",
        Some("Paused") => "Paused",
        _ => "Stopped",
    }
}

impl State {
    /// The player to show: one that plays (the current one if it does),
    /// else the current one if it is still there and not stopped, else any
    /// paused one.
    fn pick(&mut self) -> Option<(String, Props)> {
        let mut all: Vec<(String, Props)> = players()
            .into_iter()
            .filter_map(|n| props(&n, PLAYER).map(|p| (n, p)))
            .collect();
        let rank = |(n, p): &(String, Props), current: &Option<String>| {
            let s = status(p);
            let is_current = current.as_deref() == Some(n.as_str());
            match (s, is_current) {
                ("Playing", true) => 0,
                ("Playing", false) => 1,
                ("Paused", true) => 2,
                ("Paused", false) => 3,
                (_, true) => 4,
                _ => 5,
            }
        };
        let current = self.current.clone();
        all.sort_by_key(|e| rank(e, &current));
        let best = all.into_iter().next().filter(|e| rank(e, &current) < 5);
        self.current = best.as_ref().map(|(n, _)| n.clone());
        best
    }

    fn snapshot(&mut self) -> Option<NowPlaying> {
        let (name, p) = self.pick()?;
        let meta = p.get("Metadata");
        let get = |key: &str| meta.and_then(|m| dbus::dict_get(m, key));
        let title = get("xesam:title")
            .and_then(dbus::as_str)
            .unwrap_or_default();
        let artist = get("xesam:artist")
            .map(dbus::as_strs)
            .unwrap_or_default()
            .join(", ");
        let playing = status(&p) == "Playing";
        let flag = |key: &str| p.get(key).and_then(|v| dbus::as_bool(v)).unwrap_or(false);
        let length = get("mpris:length")
            .and_then(dbus::as_i64)
            .filter(|l| *l > 0);
        // Position is not sent with PropertiesChanged; ask for it.
        let position =
            dbus::call::<_, OwnedValue>(&name, PATH, PROPS, "Get", &(PLAYER, "Position"))
                .ok()
                .and_then(|v| dbus::as_i64(&v));
        let timeline = match (position, length) {
            (Some(pos), Some(len)) => Some((pos as f64 / 1e6, len as f64 / 1e6)),
            _ => None,
        };
        let identity = props(&name, "org.mpris.MediaPlayer2")
            .and_then(|i| i.get("Identity").and_then(|v| dbus::as_str(v)));
        let source = identity.unwrap_or_else(|| clean_source(&name));
        let art_url = get("mpris:artUrl")
            .and_then(dbus::as_str)
            .unwrap_or_default();
        let art = self.art_for(&art_url);
        let can_control = p
            .get("CanControl")
            .and_then(|v| dbus::as_bool(v))
            .unwrap_or(true);
        Some(NowPlaying {
            source,
            title,
            artist,
            playing,
            can_previous: can_control && flag("CanGoPrevious"),
            can_next: can_control && flag("CanGoNext"),
            can_toggle: can_control && (flag("CanPause") || flag("CanPlay")),
            timeline,
            captured: Instant::now(),
            art,
        })
    }

    fn art_for(&mut self, url: &str) -> Option<Arc<Art>> {
        if url.is_empty() {
            return None;
        }
        if let Some((u, art)) = &self.art
            && u == url
        {
            return art.clone();
        }
        let art = load_art(url).map(Arc::new);
        self.art = Some((url.to_owned(), art.clone()));
        art
    }

    fn command(&mut self, cmd: MediaCommand) {
        let Some(name) = self.current.clone() else {
            return;
        };
        let method = match cmd {
            MediaCommand::PlayPause => "PlayPause",
            MediaCommand::Next => "Next",
            MediaCommand::Previous => "Previous",
        };
        if let Err(e) = dbus::call_void(&name, PATH, PLAYER, method, &()) {
            tracing::warn!("media {method}: {e}");
        }
    }
}

/// Album art from a `file://` path or an `http(s)://` URL (Spotify and
/// browsers serve it over the network).
fn load_art(url: &str) -> Option<Art> {
    let bytes = if let Some(path) = dbus::uri_to_path(url) {
        std::fs::read(path).ok()?
    } else if url.starts_with("https://") || url.starts_with("http://") {
        let resp = ureq::get(url).timeout(Duration::from_secs(5)).call().ok()?;
        let mut buf = Vec::new();
        use std::io::Read;
        resp.into_reader()
            .take(8 * 1024 * 1024)
            .read_to_end(&mut buf)
            .ok()?;
        buf
    } else {
        return None;
    };
    let img = image::load_from_memory(&bytes).ok()?;
    let img = img.thumbnail(ART_SIZE, ART_SIZE).to_rgba8();
    Some(Art {
        width: img.width() as usize,
        height: img.height() as usize,
        rgba: img.into_raw(),
    })
}

/// "org.mpris.MediaPlayer2.spotify" -> "spotify",
/// "org.mpris.MediaPlayer2.firefox.instance_1_42" -> "firefox". Opaque ids
/// (mostly hex) come back empty rather than as noise.
pub fn clean_source(id: &str) -> String {
    let name = id.strip_prefix(PREFIX).unwrap_or(id);
    let name = name.split(".instance").next().unwrap_or(name);
    let name = name.rsplit(['/', '!']).next().unwrap_or(name);
    let hex = name.chars().filter(char::is_ascii_hexdigit).count();
    if name.len() >= 8 && hex * 3 >= name.len() * 2 {
        return String::new();
    }
    name.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_names() {
        assert_eq!(clean_source("org.mpris.MediaPlayer2.spotify"), "spotify");
        assert_eq!(
            clean_source("org.mpris.MediaPlayer2.firefox.instance_1_42"),
            "firefox"
        );
        assert_eq!(clean_source("org.mpris.MediaPlayer2.F0DC299D809B9700"), "");
    }

    #[test]
    fn paused_position_does_not_advance() {
        let np = NowPlaying {
            source: String::new(),
            title: String::new(),
            artist: String::new(),
            playing: false,
            can_previous: false,
            can_next: false,
            can_toggle: false,
            timeline: Some((30.0, 100.0)),
            captured: Instant::now() - Duration::from_secs(5),
            art: None,
        };
        assert_eq!(np.position(), Some((30.0, 100.0)));
        let playing = NowPlaying {
            playing: true,
            ..np
        };
        let (pos, _) = playing.position().unwrap();
        assert!(pos >= 35.0);
    }
}
