#!/usr/bin/env bash
# Builds the Fedora package: makes the source tarball and the vendored
# crates tarball nook.spec expects, then runs rpmbuild.
#
#   sudo dnf install rpm-build cargo rust gcc gtk3-devel desktop-file-utils libappstream-glib
#   packaging/linux/build-rpm.sh
#   sudo dnf install ~/rpmbuild/RPMS/x86_64/nook-*.rpm
#
# NOOK_RELEASES_REPO=owner/name points the in-app update check at a fork
# that publishes these packages.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
top=${RPM_TOPDIR:-$HOME/rpmbuild}
mkdir -p "$top"/{SOURCES,SPECS,BUILD,RPMS,SRPMS}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

echo "Packing the sources (nook $version)..."
tar -C "$root" \
    --exclude=./target --exclude=./.git --exclude=./vendor --exclude=./.cargo \
    --transform "s,^\.,nook-$version," \
    -czf "$top/SOURCES/nook-$version.tar.gz" .

echo "Vendoring crates..."
(cd "$root" && cargo vendor --locked --versioned-dirs "$work/vendor" >/dev/null)
tar -C "$work" -czf "$top/SOURCES/nook-$version-vendor.tar.gz" vendor

cp "$here/nook.spec" "$top/SPECS/nook.spec"
args=(--define "_topdir $top")
if [ -n "${NOOK_RELEASES_REPO:-}" ]; then
    args+=(--define "repo $NOOK_RELEASES_REPO")
fi
rpmbuild -ba "${args[@]}" "$top/SPECS/nook.spec"
echo
echo "Built:"
find "$top/RPMS" "$top/SRPMS" -name "nook-$version*.rpm" -newer "$top/SPECS/nook.spec" -print
