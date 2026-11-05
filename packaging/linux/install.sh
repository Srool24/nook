#!/usr/bin/env bash
# Builds Nook from this checkout and installs it for the current user
# (~/.local), without root. `--deps` first installs what Fedora needs to
# build and run it (asks for sudo). `--uninstall` removes it again.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
prefix=${PREFIX:-$HOME/.local}
apps="$prefix/share/applications"
icons="$prefix/share/icons/hicolor"

if [ "${1:-}" = "--uninstall" ]; then
    pkill -x nook 2>/dev/null || true
    rm -f "$prefix/bin/nook" "$apps/nook.desktop" "$HOME/.config/autostart/nook.desktop"
    rm -f "$apps"/nook-nook-handler.desktop
    for s in 16 32 48 256 512; do rm -f "$icons/${s}x${s}/apps/nook.png"; done
    command -v update-desktop-database >/dev/null && update-desktop-database "$apps" || true
    echo "Nook removed. Settings stay in ~/.config/nook (delete it to forget them)."
    echo "GNOME shortcuts Nook added are under Settings > Keyboard > Custom Shortcuts."
    exit 0
fi

if [ "${1:-}" = "--deps" ]; then
    sudo dnf install -y cargo rust gcc gtk3-devel \
        libayatana-appindicator-gtk3 gnome-shell-extension-appindicator \
        tesseract tesseract-langpack-eng pipewire-utils android-tools \
        gdk-pixbuf2 libcanberra-gtk3 xdg-desktop-portal-gnome
fi

for tool in cargo cc pkg-config; do
    command -v "$tool" >/dev/null || {
        echo "Missing $tool. Run: $0 --deps" >&2
        exit 1
    }
done

cd "$root"
cargo build --release -p nook-app

install -Dm755 target/release/nook "$prefix/bin/nook"
mkdir -p "$apps"
sed "s|^Exec=nook |Exec=$prefix/bin/nook |" packaging/linux/nook.desktop > "$apps/nook.desktop"
for s in 16 32 48 256 512; do
    install -Dm644 "assets/icon/nook-$s.png" "$icons/${s}x${s}/apps/nook.png"
done
command -v update-desktop-database >/dev/null && update-desktop-database "$apps" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$icons" || true

echo
echo "Nook is installed: open it from Activities, or run $prefix/bin/nook"
if ! gnome-extensions list --enabled 2>/dev/null | grep -q appindicator; then
    echo "Tip: for the tray icon, enable \"AppIndicator and KStatusNotifierItem Support\""
    echo "     (sudo dnf install gnome-shell-extension-appindicator, then log out and in)."
fi
