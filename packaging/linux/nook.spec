# Fedora package for Nook (the Linux port).
#
# Build it with packaging/linux/build-rpm.sh, which makes the source and
# vendored-crates tarballs this spec expects and runs rpmbuild.

%global nook_repo %{?repo}%{!?repo:Geguchh024/nook}

Name:           nook
Version:        0.2.0
Release:        1%{?dist}
Summary:        A floating notch utility hub
License:        MIT
URL:            https://github.com/%{nook_repo}
Source0:        %{name}-%{version}.tar.gz
# `cargo vendor` output, so the build runs offline (as Koji and mock do).
Source1:        %{name}-%{version}-vendor.tar.gz

ExclusiveArch:  x86_64 aarch64

BuildRequires:  cargo >= 1.88
BuildRequires:  rust >= 1.88
BuildRequires:  gcc
BuildRequires:  pkgconfig(gtk+-3.0)
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib

# Loaded at run time (dlopen), so rpm cannot see them by itself.
Requires:       libX11
Requires:       libXcursor
Requires:       libXi
Requires:       libXrandr
Requires:       libxkbcommon-x11
Requires:       mesa-libEGL
Requires:       mesa-libGL
Requires:       libayatana-appindicator-gtk3
# The overlay is an X11 client on GNOME's Wayland session.
Requires:       xorg-x11-server-Xwayland
# Screen grabs, file choosers and settings go through the desktop portal.
Requires:       xdg-desktop-portal
Requires:       glib2
Requires:       dconf
Requires:       fontconfig
Recommends:     xdg-desktop-portal-gnome
# Tray icon on GNOME.
Recommends:     gnome-shell-extension-appindicator
# Text extractor and font detector.
Recommends:     tesseract
Recommends:     tesseract-langpack-eng
# Voice memos.
Recommends:     pipewire-utils
# Wallpaper and file thumbnails in formats `image` cannot read (JPEG XL).
Recommends:     gdk-pixbuf2
Recommends:     jxl-pixbuf-loader
# Phone link (notifications and mirroring).
Recommends:     android-tools
Suggests:       scrcpy
# Alert sound.
Recommends:     libcanberra-gtk3

%description
Nook is a small glass pill at the top of the screen. Hover it and it opens
into a grid of tools: timer, keep-awake, clipboard history, media controls,
text extractor (OCR), color picker, Lens search, font detector, screenshots,
draw on screen, QR codes, calculator, voice memos, notes, calendar, system
monitor, wallpapers, a file shelf, quick launch and phone link (LocalSend
transfers, scrcpy mirroring, notifications). Plugins add more tools.

%prep
%autosetup -n %{name}-%{version} -a1
mkdir -p .cargo
cat > .cargo/config.toml <<'CARGO'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
CARGO

%build
export NOOK_RELEASES_REPO=%{nook_repo}
export RUSTFLAGS="%{?build_rustflags}"
cargo build --release --locked --offline -p nook-app

%install
install -Dm755 target/release/nook %{buildroot}%{_bindir}/nook
desktop-file-install --dir=%{buildroot}%{_datadir}/applications packaging/linux/nook.desktop
for s in 16 32 48 256 512; do
  install -Dm644 assets/icon/nook-$s.png \
    %{buildroot}%{_datadir}/icons/hicolor/${s}x${s}/apps/nook.png
done
install -Dm644 packaging/linux/io.github.geguchh024.Nook.metainfo.xml \
  %{buildroot}%{_metainfodir}/io.github.geguchh024.Nook.metainfo.xml
# Plugin helpers for plugin authors.
install -Dm644 sdk/python/nook.py %{buildroot}%{_datadir}/nook/sdk/python/nook.py
install -Dm644 sdk/node/nook.js %{buildroot}%{_datadir}/nook/sdk/node/nook.js

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/nook.desktop
appstream-util validate-relax --nonet %{buildroot}%{_metainfodir}/*.metainfo.xml
cargo test --release --locked --offline -p nook-ui -p nook-modules --lib

%files
%license LICENSE
%doc README.md docs/LINUX.md
%{_bindir}/nook
%{_datadir}/applications/nook.desktop
%{_datadir}/icons/hicolor/*/apps/nook.png
%{_metainfodir}/io.github.geguchh024.Nook.metainfo.xml
%{_datadir}/nook/

%changelog
* Thu Oct 29 2026 The Nook contributors - 0.2.0-1
- First Linux (Fedora GNOME) build
