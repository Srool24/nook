# Nook on Linux (Fedora GNOME)

Nook runs on Fedora Workstation (GNOME, Wayland) with every tool the
Windows build has. The UI, the modules and the plugin system are the same
code; only `crates/platform` (and a few lines in the app) differ, behind
`#[cfg(windows)]` / `#[cfg(target_os = "linux")]`. Each platform module keeps
the same public API, so callers do not know which system they run on.

## Install

### From a release (RPM)

```bash
sudo dnf install ./nook-0.2.0-1.fc43.x86_64.rpm
```

### Build the RPM yourself

```bash
sudo dnf install rpm-build cargo rust gcc gtk3-devel desktop-file-utils libappstream-glib
packaging/linux/build-rpm.sh
sudo dnf install ~/rpmbuild/RPMS/x86_64/nook-*.rpm
```

### Build and install for your user only (no root for Nook itself)

```bash
packaging/linux/install.sh --deps   # once: build tools and runtime helpers (sudo)
packaging/linux/install.sh          # builds and installs into ~/.local
packaging/linux/install.sh --uninstall
```

Or just `cargo run --release` from the checkout.

### Recommended extras

| For | Package |
| --- | --- |
| Tray icon | `gnome-shell-extension-appindicator` (enable it, then log out and in) |
| Text extractor, font detector | `tesseract` and `tesseract-langpack-<lang>` (e.g. `-eng`, `-heb`) |
| Voice memos | `pipewire-utils` (installed on Workstation) |
| Phone notifications | `android-tools` (adb) |
| Phone mirroring | `scrcpy` (the Settings page of Phone link installs it in a terminal) |
| JPEG XL wallpapers (Fedora's own) | `jxl-pixbuf-loader` |

## How each part works

| Feature | Windows | Linux |
| --- | --- | --- |
| Overlay window | Layered Win32 window | X11 window through XWayland: `_NET_WM_STATE_ABOVE`, skip taskbar/pager |
| Click-through outside the shape | Cursor-polling thread toggling `WS_EX_TRANSPARENT` | SHAPE extension *input region* set to the shape; the X server routes clicks, no thread |
| Glass blur | Windows.UI.Composition backdrop | No blur API for clients on GNOME: the glass is painted by egui (the existing fallback) |
| Global shortcuts | `RegisterHotKey` (global-hotkey) | GNOME custom shortcuts that run `nook --hotkey=<id>` (see below); X11 sessions use global-hotkey |
| Screen freeze (snip, lens, OCR, color picker, screenshots, font detector) | GDI `BitBlt` | `org.freedesktop.portal.Screenshot` (GNOME asks once), X11 `GetImage` in an X11 session; Nook's windows are faded out for the grab |
| Snip overlay | GDI window | Software-rendered full-screen X11 window, text via fontconfig + `ab_glyph` |
| Draw on screen | Per-pixel-alpha layered window | 32-bit ARGB X11 window spanning all monitors (`_NET_WM_FULLSCREEN_MONITORS`) |
| OCR | Windows.Media.Ocr | Tesseract (TSV output for word boxes) |
| Clipboard history | `AddClipboardFormatListener` | XFixes selection events; secrets skipped via `x-kde-passwordManagerHint`; writes through arboard |
| Media | SMTC | MPRIS over D-Bus (art from `file://` and `https://`) |
| Keep awake (Coffee) | `SetThreadExecutionState` | `org.gnome.SessionManager.Inhibit` (portal `Inhibit` elsewhere) |
| Voice memos | waveIn / PlaySound | `pw-record` / `pw-play` (`parec` / `paplay` fallback) |
| System monitor | Win32 perf APIs | `/proc` and `/sys` |
| Wallpaper | IDesktopWallpaper | `gsettings` `org.gnome.desktop.background` (light and dark) |
| Accent and theme | DWM / registry | Settings portal `org.freedesktop.appearance` (GNOME 47+ accent colors) |
| File shelf drag-out | `SHDoDragDrop` | XDND source (Mutter bridges it to Wayland apps) |
| Thumbnails and icons | Shell image factory | Images, the freedesktop thumbnail cache, icon themes, GDK-Pixbuf for other formats |
| File and folder pickers | IFileOpenDialog | `org.freedesktop.portal.FileChooser` |
| Phone mirror | scrcpy window owned by Nook | scrcpy as an X11 window, transient for Nook, SHAPE-rounded, Esc via a key grab |
| Single instance, links from a second launch | Named mutex, named pipe | `flock` and a Unix socket in `$XDG_RUNTIME_DIR` |
| `nook://` links | `HKCU\Software\Classes` | A hidden desktop entry + `~/.config/mimeapps.list` |
| Launch at sign-in | `Run` key | `~/.config/autostart/nook.desktop` |
| Settings and data | `%APPDATA%\Nook` | `~/.config/nook` |
| Updates | Downloads and runs Setup | Downloads the release's `.rpm` and installs it with `pkexec dnf` (RPM installs only; other builds open the release page) |
| Usage stats | Daily anonymous ping | Never sent from Linux builds |

## Keyboard shortcuts on GNOME

A Wayland session does not let any app grab keys system-wide. On GNOME,
Nook adds its shortcuts to **Settings > Keyboard > View and Customize
Shortcuts > Custom Shortcuts** as "Nook: ..." entries (Ctrl+Alt+N opens the
panel, Ctrl+Alt+T extracts text, and so on). Each runs `nook --hotkey=<id>`,
which hands the key to the running Nook (or starts it).

You can change a binding there; Nook never overwrites one it already
added, it only adds missing ones and keeps the command pointing at the
installed binary. A shortcut of an uninstalled plugin is removed.

## Known differences

- **Blur.** GNOME offers no backdrop blur to apps, so the glass is tinted
  and nearly opaque, as with the "Solid" material on Windows.
- **Wayland windows are invisible to X11 clients.** In screenshot mode the
  window under the cursor is highlighted for X11 (XWayland) apps only;
  elsewhere a click takes the monitor (F and A work everywhere). "Hide in
  fullscreen" sees X11 games and apps; a focused fullscreen Wayland window
  is stacked above Nook by GNOME anyway.
- **First screenshot.** GNOME may ask once whether Nook may take
  screenshots, and shows its own flash.
- **Fractional scaling.** Nook follows `Xft.dpi`. With GNOME's
  `xwayland-native-scaling` experimental feature on, X11 apps render
  sharply at fractional scales; without it GNOME scales them up.
- **Tray icon** needs the AppIndicator extension; without it, use the
  shortcuts or the gear tile in the grid for settings.

## Development

```bash
cargo run --release
cargo test --workspace          # some tests need a display: xvfb-run -a cargo test
cargo clippy --workspace --all-targets -- -D warnings
NOOK_LOG=debug cargo run        # logs go to ~/.config/nook/nook.log in release builds
```

Linux code lives in `crates/platform/src/linux/` (one file per platform
module, and `sys/` for the X11 connection, D-Bus and portal, settings,
fonts, icons and helper processes) and in `crates/app/src/shortcuts.rs`.
