//! System load for the system monitor: CPU, memory, network traffic,
//! disks, uptime and per-process usage, read from `/proc` and `/sys`. Every
//! call is a cheap snapshot; rates come from the caller diffing two of them.
//!
//! CPU times are reported in 100 ns ticks, like on Windows, so the system
//! monitor's arithmetic is the same on both.

use crate::{PlatformError, Result};
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

/// Cumulative CPU time of the whole machine, in 100 ns ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuTimes {
    pub idle: u64,
    /// All CPU time, idle included.
    pub total: u64,
}

impl CpuTimes {
    /// Busy share (0..1) between `prev` and `self`.
    pub fn usage_since(&self, prev: &CpuTimes) -> f32 {
        let total = self.total.saturating_sub(prev.total);
        let idle = self.idle.saturating_sub(prev.idle);
        if total == 0 {
            return 0.0;
        }
        (1.0 - idle as f64 / total as f64).clamp(0.0, 1.0) as f32
    }
}

/// 100 ns ticks per kernel clock tick (`USER_HZ`, almost always 100).
fn tick_scale() -> u64 {
    static SCALE: OnceLock<u64> = OnceLock::new();
    *SCALE.get_or_init(|| {
        // SAFETY: plain sysconf query.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        let hz = if hz > 0 { hz as u64 } else { 100 };
        10_000_000 / hz
    })
}

fn read(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    std::fs::read_to_string(path)
        .map_err(|e| PlatformError::other(&format!("reading {}", path.display()), e))
}

pub fn cpu_times() -> Result<CpuTimes> {
    parse_cpu_times(&read("/proc/stat")?)
        .ok_or_else(|| PlatformError::Other("no cpu line in /proc/stat".into()))
}

fn parse_cpu_times(stat: &str) -> Option<CpuTimes> {
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|n| n.parse().ok())
        .collect();
    // user nice system idle iowait irq softirq steal (guest time is already
    // part of user time).
    let total: u64 = v.iter().take(8).sum();
    let idle = v.get(3).copied().unwrap_or(0) + v.get(4).copied().unwrap_or(0);
    let s = tick_scale();
    Some(CpuTimes {
        idle: idle * s,
        total: total * s,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Memory {
    pub used: u64,
    pub total: u64,
}

pub fn memory() -> Result<Memory> {
    parse_meminfo(&read("/proc/meminfo")?)
        .ok_or_else(|| PlatformError::Other("unreadable /proc/meminfo".into()))
}

fn parse_meminfo(text: &str) -> Option<Memory> {
    let kb = |key: &str| {
        text.lines().find_map(|l| {
            let rest = l.strip_prefix(key)?.strip_prefix(':')?;
            rest.split_whitespace().next()?.parse::<u64>().ok()
        })
    };
    let total = kb("MemTotal")? * 1024;
    let available = kb("MemAvailable").or_else(|| kb("MemFree"))? * 1024;
    Some(Memory {
        used: total.saturating_sub(available),
        total,
    })
}

/// Bytes received and sent so far by the physical network adapters
/// (loopback, bridges, VPN and container interfaces would count the same
/// traffic twice).
pub fn net_bytes() -> Result<(u64, u64)> {
    let text = read("/proc/net/dev")?;
    let physical = |name: &str| {
        Path::new("/sys/class/net")
            .join(name)
            .join("device")
            .exists()
    };
    Ok(parse_net_dev(&text, physical))
}

fn parse_net_dev(text: &str, physical: impl Fn(&str) -> bool) -> (u64, u64) {
    let (mut down, mut up) = (0u64, 0u64);
    for line in text.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name == "lo" || !physical(name) {
            continue;
        }
        let v: Vec<u64> = rest
            .split_whitespace()
            .filter_map(|n| n.parse().ok())
            .collect();
        down += v.first().copied().unwrap_or(0);
        up += v.get(8).copied().unwrap_or(0);
    }
    (down, up)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disk {
    /// The mount point, e.g. `/` or `/data`.
    pub name: String,
    pub free: u64,
    pub total: u64,
}

/// File systems on local disks, one entry per device (Btrfs subvolumes such
/// as Fedora's `/` and `/home` share one), without boot partitions and
/// removable media.
pub fn disks() -> Vec<Disk> {
    let Ok(mounts) = read("/proc/self/mounts") else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (device, mount) in local_mounts(&mounts) {
        if !seen.insert(device) {
            continue;
        }
        let Ok(c) = std::ffi::CString::new(mount.as_bytes()) else {
            continue;
        };
        // SAFETY: a null-terminated path and a zeroed out struct.
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            continue;
        }
        let frsize = st.f_frsize as u64;
        let total = st.f_blocks as u64 * frsize;
        if total == 0 {
            continue;
        }
        out.push(Disk {
            name: mount,
            free: st.f_bavail as u64 * frsize,
            total,
        });
    }
    out
}

/// `(device, mount point)` of the local disk file systems in a mounts table.
fn local_mounts(text: &str) -> Vec<(String, String)> {
    const FS: [&str; 13] = [
        "ext2", "ext3", "ext4", "xfs", "btrfs", "f2fs", "vfat", "exfat", "ntfs", "ntfs3",
        "fuseblk", "zfs", "bcachefs",
    ];
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (dev, mount, fs) = (f.next()?, f.next()?, f.next()?);
            let mount = unescape_mount(mount);
            let skip = mount.starts_with("/boot")
                || mount == "/efi"
                || mount.starts_with("/run/media/")
                || mount.starts_with("/media/")
                || mount.starts_with("/snap/")
                || mount.starts_with("/var/lib/");
            (FS.contains(&fs) && (dev.starts_with("/dev/") || fs == "zfs") && !skip)
                .then(|| (dev.to_owned(), mount))
        })
        .collect()
}

/// `/proc/mounts` writes spaces and tabs in paths as octal escapes.
fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 4 <= b.len()
            && let Some(v) = std::str::from_utf8(&b[i + 1..i + 4])
                .ok()
                .and_then(|o| u8::from_str_radix(o, 8).ok())
        {
            out.push(v);
            i += 4;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Time since the system started.
pub fn uptime() -> std::time::Duration {
    read("/proc/uptime")
        .ok()
        .and_then(|t| t.split_whitespace().next()?.parse::<f64>().ok())
        .map_or(
            std::time::Duration::ZERO,
            std::time::Duration::from_secs_f64,
        )
}

/// One running process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    /// Executable name, e.g. `firefox`.
    pub name: String,
    /// Kernel + user time so far, 100 ns ticks.
    pub cpu: u64,
    /// Resident memory, bytes.
    pub memory: u64,
}

fn page_size() -> u64 {
    // SAFETY: plain sysconf query.
    let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if p > 0 { p as u64 } else { 4096 }
}

/// Every user-space process (kernel threads are left out, as Windows'
/// "System" process is by the system monitor).
pub fn processes() -> Result<Vec<Process>> {
    let dir = std::fs::read_dir("/proc").map_err(|e| PlatformError::other("reading /proc", e))?;
    let page = page_size();
    let scale = tick_scale();
    let mut out = Vec::with_capacity(256);
    for entry in dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let base = entry.path();
        let Ok(cmdline) = std::fs::read(base.join("cmdline")) else {
            continue;
        };
        if cmdline.is_empty() {
            continue; // kernel thread or zombie
        }
        let Ok(stat) = std::fs::read_to_string(base.join("stat")) else {
            continue;
        };
        let Some((comm, cpu)) = parse_stat(&stat) else {
            continue;
        };
        let rss = std::fs::read_to_string(base.join("statm"))
            .ok()
            .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
            .unwrap_or(0);
        out.push(Process {
            pid,
            name: process_name(&cmdline, &comm),
            cpu: cpu * scale,
            memory: rss * page,
        });
    }
    Ok(out)
}

/// `(comm, utime + stime)` from `/proc/<pid>/stat`. The name sits in
/// parentheses and may itself contain spaces and parentheses.
fn parse_stat(stat: &str) -> Option<(String, u64)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let comm = stat.get(open + 1..close)?.to_owned();
    let fields: Vec<&str> = stat.get(close + 2..)?.split_whitespace().collect();
    // After the name: state(0) ... utime(11) stime(12).
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some((comm, utime + stime))
}

/// The program's file name from its command line (`comm` is cut at 15
/// bytes), falling back to `comm` for interpreters' renamed processes.
fn process_name(cmdline: &[u8], comm: &str) -> String {
    let argv0 = cmdline.split(|&b| b == 0).next().unwrap_or_default();
    let argv0 = String::from_utf8_lossy(argv0);
    // Electron and Chromium helpers: "/opt/app/app --type=renderer".
    let first = argv0.split(' ').next().unwrap_or_default();
    let name = first.rsplit('/').next().unwrap_or_default();
    if name.is_empty() || name.starts_with('-') {
        comm.to_owned()
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_usage_from_two_samples() {
        let a = CpuTimes {
            idle: 100,
            total: 200,
        };
        let b = CpuTimes {
            idle: 130,
            total: 300,
        };
        assert!((b.usage_since(&a) - 0.7).abs() < 1e-6);
        assert_eq!(a.usage_since(&a), 0.0);
    }

    #[test]
    fn parses_proc_files() {
        let t = parse_cpu_times("cpu  10 0 10 70 10 0 0 0 0 0\ncpu0 1 2 3 4\n").unwrap();
        let s = tick_scale();
        assert_eq!((t.idle, t.total), (80 * s, 100 * s));
        let m =
            parse_meminfo("MemTotal:  1000 kB\nMemFree: 100 kB\nMemAvailable:  400 kB\n").unwrap();
        assert_eq!((m.used, m.total), (600 * 1024, 1000 * 1024));
        let dev = "Inter-|   Receive\n face |bytes\n    lo: 500 1 0 0 0 0 0 0 500 1 0 0 0 0 0 0\n wlp2s0: 1000 9 0 0 0 0 0 0 200 3 0 0 0 0 0 0\nvirbr0: 7 0 0 0 0 0 0 0 7 0 0 0 0 0 0 0\n";
        assert_eq!(parse_net_dev(dev, |n| n == "wlp2s0"), (1000, 200));
    }

    #[test]
    fn parses_process_stat_and_names() {
        let stat = "1234 (Web Content (x)) S 1 2 3 4 5 6 7 8 9 10 250 50 0 0";
        assert_eq!(parse_stat(stat), Some(("Web Content (x)".into(), 300)));
        assert_eq!(
            process_name(
                b"/usr/lib64/firefox/firefox\0-contentproc\0",
                "Isolated Web Co"
            ),
            "firefox"
        );
        assert_eq!(process_name(b"-bash\0", "bash"), "bash");
    }

    #[test]
    fn lists_local_disks_once() {
        let mounts = "/dev/nvme0n1p3 / btrfs rw 0 0\n/dev/nvme0n1p3 /home btrfs rw 0 0\n/dev/nvme0n1p2 /boot ext4 rw 0 0\ntmpfs /tmp tmpfs rw 0 0\n/dev/sdb1 /mnt/My\\040Data ext4 rw 0 0\n/dev/sdc1 /run/media/me/USB vfat rw 0 0\n";
        let m = local_mounts(mounts);
        assert_eq!(
            m.iter().map(|(_, p)| p.as_str()).collect::<Vec<_>>(),
            ["/", "/home", "/mnt/My Data"]
        );
    }

    #[test]
    fn snapshots_work() {
        assert!(memory().unwrap().total > 0);
        assert!(cpu_times().unwrap().total > 0);
        assert!(processes().unwrap().len() > 1);
    }
}
