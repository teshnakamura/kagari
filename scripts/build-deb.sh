#!/usr/bin/env bash
#
# Build a Debian/Ubuntu package for kagari using dpkg-deb directly.
# This works on any Rust toolchain (it does not depend on cargo-deb).
#
# Output: dist/kagari_<version>_<arch>.deb
#
set -euo pipefail

cd "$(dirname "$0")/.."

VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
ARCH="$(dpkg --print-architecture)"
DEST="dist"
PKGDIR="$(mktemp -d)"
trap 'rm -rf "$PKGDIR"' EXIT
chmod 755 "$PKGDIR" # mktemp creates 0700; the package root must be world-readable

echo "Building kagari ${VERSION} (${ARCH}) ..."
cargo build --release

# Lay out the package tree.
install -Dm755 target/release/kagari                   "$PKGDIR/usr/bin/kagari"
install -Dm644 assets/info.teshnakamura.Kagari.desktop "$PKGDIR/usr/share/applications/info.teshnakamura.Kagari.desktop"
install -Dm644 assets/kagari.svg                        "$PKGDIR/usr/share/icons/hicolor/scalable/apps/kagari.svg"
install -Dm644 README.md                                "$PKGDIR/usr/share/doc/kagari/README"
install -Dm644 LICENSE                                  "$PKGDIR/usr/share/doc/kagari/copyright"

mkdir -p "$PKGDIR/DEBIAN"

# libgtk-4-1 transitively pulls in glib/cairo/pango/gdk-pixbuf/graphene.
# lm-sensors is needed at runtime for temperature and fan readings.
cat > "$PKGDIR/DEBIAN/control" <<EOF
Package: kagari
Version: ${VERSION}
Architecture: ${ARCH}
Maintainer: Tesh Nakamura <tesh@teshnakamura.com>
Depends: libgtk-4-1, lm-sensors
Section: utils
Priority: optional
Homepage: https://github.com/teshnakamura/kagari
Description: Wayland-native live system metrics graph
 kagari (the kanji 篝, a watchfire) is a Wayland-native live graph of system
 metrics: temperatures, CPU and memory usage, and fan speeds. Unlike psensor
 under XWayland, it keeps updating while the window is visible by separating
 data collection from drawing.
EOF

cat > "$PKGDIR/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi
exit 0
EOF
chmod 755 "$PKGDIR/DEBIAN/postinst"

cat > "$PKGDIR/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi
exit 0
EOF
chmod 755 "$PKGDIR/DEBIAN/postrm"

mkdir -p "$DEST"
DEB="${DEST}/kagari_${VERSION}_${ARCH}.deb"
dpkg-deb --build --root-owner-group "$PKGDIR" "$DEB"

echo "Built: ${DEB}"
