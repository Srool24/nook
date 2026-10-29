//! System preferences: theme, animations, accent color and wallpaper path
//! from GNOME's settings; clock, locale and battery from the system.

use crate::sys::settings::{self, Setting};
use crate::sys::{cmd, dbus};
use crate::{PlatformError, Result};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const INTERFACE: &str = "org.gnome.desktop.interface";

/// The desktop prefers a light style (GNOME's "Default" is light).
pub fn apps_use_light_theme() -> bool {
    if let Some(v) = dbus::read_setting("org.freedesktop.appearance", "color-scheme")
        && let Some(n) = dbus::as_i64(&v)
    {
        // 0 no preference, 1 prefer dark, 2 prefer light.
        return n != 1;
    }
    settings::get(INTERFACE, "color-scheme")
        .as_ref()
        .and_then(Setting::as_str)
        .is_none_or(|s| s != "prefer-dark")
}

/// There is no system-wide transparency switch on GNOME.
pub fn transparency_enabled() -> bool {
    true
}

/// Pixel size of a tray (AppIndicator) icon.
pub fn small_icon_size() -> u32 {
    22
}

/// "Animations" (Settings > Accessibility > Seeing / Reduce Animation).
pub fn animations_enabled() -> bool {
    settings::get(INTERFACE, "enable-animations")
        .as_ref()
        .and_then(Setting::as_bool)
        .unwrap_or(true)
}

/// Windows build number on Windows; always 0 here (see [`os_version`]).
pub fn windows_build() -> u32 {
    0
}

/// The distribution and version, e.g. "Fedora Linux 43 (Workstation Edition)".
pub fn os_version() -> String {
    let text = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    text.lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim_matches('"').to_owned())
        .unwrap_or_else(|| "Linux".into())
}

/// GNOME's accent color as RGB (Settings > Appearance, GNOME 47 and later).
pub fn system_accent() -> Result<[u8; 3]> {
    if let Some(v) = dbus::read_setting("org.freedesktop.appearance", "accent-color") {
        let f: Vec<f64> = dbus::fields(&v).iter().filter_map(dbus::as_f64).collect();
        // Out of range means "not set".
        if let [r, g, b] = f[..]
            && [r, g, b].iter().all(|c| (0.0..=1.0).contains(c))
        {
            let c = |v: f64| (v * 255.0).round() as u8;
            return Ok([c(r), c(g), c(b)]);
        }
    }
    let name = settings::get(INTERFACE, "accent-color")
        .and_then(|s| s.as_str().map(str::to_owned))
        .ok_or_else(|| PlatformError::Other("no accent color setting".into()))?;
    named_accent(&name).ok_or_else(|| PlatformError::Other(format!("unknown accent {name}")))
}

/// libadwaita's accent palette.
fn named_accent(name: &str) -> Option<[u8; 3]> {
    Some(match name {
        "blue" => [0x35, 0x84, 0xe4],
        "teal" => [0x21, 0x90, 0xa4],
        "green" => [0x3a, 0x94, 0x4a],
        "yellow" => [0xc8, 0x88, 0x00],
        "orange" => [0xed, 0x5b, 0x00],
        "red" => [0xe6, 0x2d, 0x42],
        "pink" => [0xd5, 0x61, 0x99],
        "purple" => [0x91, 0x41, 0xac],
        "slate" => [0x6f, 0x83, 0x96],
        _ => return None,
    })
}

/// Path of the current desktop wallpaper image, if any (the dark-style one
/// when the desktop is dark). For an XML slideshow, its first picture.
pub fn wallpaper_path() -> Option<std::path::PathBuf> {
    const BACKGROUND: &str = "org.gnome.desktop.background";
    let key = if apps_use_light_theme() {
        "picture-uri"
    } else {
        "picture-uri-dark"
    };
    let uri = settings::get(BACKGROUND, key)
        .and_then(|s| s.as_str().map(str::to_owned))
        .filter(|s| !s.is_empty())
        .or_else(|| {
            settings::get(BACKGROUND, "picture-uri").and_then(|s| s.as_str().map(str::to_owned))
        })?;
    let path = dbus::uri_to_path(&uri).unwrap_or_else(|| uri.clone().into());
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("xml"))
    {
        let xml = std::fs::read_to_string(&path).ok()?;
        return first_slideshow_file(&xml);
    }
    path.exists().then_some(path)
}

/// The first picture of a GNOME slideshow or dark/light XML.
fn first_slideshow_file(xml: &str) -> Option<std::path::PathBuf> {
    // `<file>/path</file>`, `<from>/path</from>` or
    // `<size width=".." height="..">/path</size>`: the first tag with text.
    let mut rest = xml;
    while let Some(open) = rest.find('<') {
        let tag_end = rest[open..].find('>')? + open;
        let tag = &rest[open + 1..tag_end];
        let after = &rest[tag_end + 1..];
        let name = tag.split_whitespace().next().unwrap_or("");
        if matches!(name, "file" | "from" | "size") {
            let text = after[..after.find('<').unwrap_or(after.len())].trim();
            if !text.is_empty() {
                return Some(text.into());
            }
        }
        rest = after;
    }
    None
}

/// Local time and date, formatted the way GNOME's top bar does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clock {
    /// e.g. "14:32" or "2:32 PM".
    pub time: String,
    /// Short weekday and day of month, e.g. "Mon 19".
    pub date: String,
    /// Seconds until the minute changes (for scheduling the next repaint).
    pub secs_to_next_minute: u32,
}

/// Local date and time, split into fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    /// 1..=12.
    pub month: u32,
    /// 1..=31.
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

fn tm_now() -> libc::tm {
    // SAFETY: `time` with a null pointer just returns the time; the
    // reentrant `localtime_r` writes into our own struct.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm
    }
}

pub fn local_time() -> LocalTime {
    let t = tm_now();
    LocalTime {
        year: t.tm_year + 1900,
        month: (t.tm_mon + 1) as u32,
        day: t.tm_mday as u32,
        hour: t.tm_hour as u32,
        minute: t.tm_min as u32,
        second: t.tm_sec.clamp(0, 59) as u32,
    }
}

/// Local date and time for file names: `2026-10-20 04-31-12`.
pub fn file_timestamp() -> String {
    let t = local_time();
    format!(
        "{:04}-{:02}-{:02} {:02}-{:02}-{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}

/// The parts of the user's `LC_TIME` locale that Nook shows.
struct TimeLocale {
    /// Sunday first, as the C library lists them.
    abday: Vec<String>,
    am_pm: [String; 2],
    /// 0 = Monday ... 6 = Sunday.
    first_day: u32,
}

fn time_locale() -> &'static TimeLocale {
    static LOCALE: OnceLock<TimeLocale> = OnceLock::new();
    LOCALE.get_or_init(|| {
        let text = cmd::output("locale", &["-k", "LC_TIME"]).unwrap_or_default();
        parse_time_locale(&text)
    })
}

fn parse_time_locale(text: &str) -> TimeLocale {
    let value = |key: &str| {
        text.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().trim_matches('"').to_owned())
        })
    };
    let mut abday: Vec<String> = value("abday")
        .map(|v| v.split(';').map(str::to_owned).collect())
        .unwrap_or_default();
    if abday.len() != 7 {
        abday = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
            .map(String::from)
            .to_vec();
    }
    let am_pm = value("am_pm")
        .and_then(|v| {
            let (a, p) = v.split_once(';')?;
            (!a.is_empty()).then(|| [a.to_owned(), p.to_owned()])
        })
        .unwrap_or_else(|| ["AM".into(), "PM".into()]);
    // week-1stday is a date whose weekday starts the week list (19971130 is
    // a Sunday); first_weekday is 1-based from there.
    let base_sunday = value("week-1stday").is_none_or(|d| d == "19971130");
    let first_weekday: u32 = value("first_weekday")
        .and_then(|v| v.parse().ok())
        .filter(|n| (1..=7).contains(n))
        .unwrap_or(if base_sunday { 2 } else { 1 });
    // Sunday-based index 0..6 (0 = Sunday) of the first day.
    let base = if base_sunday { 0 } else { 1 };
    let sunday_based = (base + first_weekday - 1) % 7;
    TimeLocale {
        abday,
        am_pm,
        first_day: (sunday_based + 6) % 7,
    }
}

/// First day of the week in the user's locale: 0 = Monday ... 6 = Sunday.
pub fn first_day_of_week() -> u32 {
    time_locale().first_day
}

/// GNOME's 12/24-hour setting, re-read at most every few seconds (the pill
/// draws the clock every frame while it animates).
fn uses_12h() -> bool {
    static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, v)) = *c
        && at.elapsed() < Duration::from_secs(10)
    {
        return v;
    }
    let v = settings::get(INTERFACE, "clock-format")
        .as_ref()
        .and_then(Setting::as_str)
        .is_some_and(|f| f == "12h");
    *c = Some((Instant::now(), v));
    v
}

pub fn clock() -> Clock {
    let t = tm_now();
    let loc = time_locale();
    let time = if uses_12h() {
        let h = match t.tm_hour % 12 {
            0 => 12,
            h => h,
        };
        let suffix = &loc.am_pm[usize::from(t.tm_hour >= 12)];
        format!("{h}:{:02} {suffix}", t.tm_min)
    } else {
        format!("{:02}:{:02}", t.tm_hour, t.tm_min)
    };
    let day = loc
        .abday
        .get(t.tm_wday.clamp(0, 6) as usize)
        .cloned()
        .unwrap_or_default();
    Clock {
        time,
        date: format!("{day} {}", t.tm_mday),
        secs_to_next_minute: 60 - (t.tm_sec.clamp(0, 59) as u32),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

/// `None` on machines without a battery. Reads `/sys/class/power_supply`:
/// the system batteries (not a mouse's) and whether a charger is online.
pub fn battery() -> Option<Battery> {
    let dir = std::fs::read_dir("/sys/class/power_supply").ok()?;
    let read = |p: &std::path::Path, f: &str| {
        std::fs::read_to_string(p.join(f))
            .ok()
            .map(|s| s.trim().to_owned())
    };
    let (mut sum, mut n, mut charging, mut mains) = (0u32, 0u32, false, false);
    for entry in dir.flatten() {
        let p = entry.path();
        match read(&p, "type").as_deref() {
            Some("Battery") => {
                if read(&p, "scope").as_deref() == Some("Device") {
                    continue;
                }
                if let Some(c) = read(&p, "capacity").and_then(|c| c.parse::<u32>().ok()) {
                    sum += c.min(100);
                    n += 1;
                }
                charging |= matches!(read(&p, "status").as_deref(), Some("Charging" | "Full"));
            }
            Some("Mains" | "USB") => mains |= read(&p, "online").as_deref() == Some("1"),
            _ => {}
        }
    }
    (n > 0).then(|| Battery {
        percent: (sum / n) as u8,
        charging: charging || mains,
    })
}

/// Plays the desktop's notification sound (the sound theme's
/// `message-new-instant`, as GNOME notifications do). Cosmetic, so a
/// failure is only logged.
pub fn alert_sound() {
    let player_cmd = if cmd::which("canberra-gtk-play").is_some() {
        let mut c = std::process::Command::new("canberra-gtk-play");
        c.args(["-i", "message-new-instant", "-d", "Nook"]);
        c
    } else {
        let file = "/usr/share/sounds/freedesktop/stereo/message-new-instant.oga";
        let player = ["pw-play", "paplay"]
            .into_iter()
            .find(|p| cmd::which(p).is_some());
        let Some(player) = player else {
            tracing::debug!("no sound player for the alert");
            return;
        };
        let mut c = std::process::Command::new(player);
        c.arg(file);
        c
    };
    if let Err(e) = cmd::spawn_detached(player_cmd) {
        tracing::debug!("alert sound: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_week_start_from_the_locale() {
        let us = parse_time_locale(
            "abday=\"Sun;Mon;Tue;Wed;Thu;Fri;Sat\"\nam_pm=\"AM;PM\"\nweek-1stday=19971130\nfirst_weekday=1\n",
        );
        assert_eq!(us.first_day, 6);
        assert_eq!(us.abday[1], "Mon");
        let de = parse_time_locale(
            "abday=\"So;Mo;Di;Mi;Do;Fr;Sa\"\nam_pm=\";\"\nweek-1stday=19971130\nfirst_weekday=2\n",
        );
        assert_eq!(de.first_day, 0);
        assert_eq!(de.am_pm[0], "AM");
        let fallback = parse_time_locale("");
        assert_eq!(fallback.first_day, 0);
    }

    #[test]
    fn finds_the_first_slideshow_picture() {
        let xml =
            "<background><static><file>/usr/share/backgrounds/a.jpg</file></static></background>";
        assert_eq!(
            first_slideshow_file(xml),
            Some("/usr/share/backgrounds/a.jpg".into())
        );
        let sized =
            "<static><file><size width=\"1920\" height=\"1080\">/b.png</size></file></static>";
        assert_eq!(first_slideshow_file(sized), Some("/b.png".into()));
    }

    #[test]
    fn names_adwaita_accents() {
        assert_eq!(named_accent("blue"), Some([0x35, 0x84, 0xe4]));
        assert_eq!(named_accent("chartreuse"), None);
    }
}
