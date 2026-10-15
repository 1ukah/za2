#!/bin/sh
# Installs za2 system-wide, or removes it with --uninstall.
# Run it as root from the release archive, or from a source folder after `cargo build --release`.
set -eu

PREFIX="${PREFIX:-/usr/local}"
BINS="za2 za2-capture za2-memory za2-replay"
HERE="$(cd "$(dirname "$0")" && pwd)"

if [ "$(id -u)" -ne 0 ]; then
    echo "Run this script as root: sudo $0 $*" >&2
    exit 1
fi

refresh() {
    command -v update-desktop-database >/dev/null && update-desktop-database -q "$PREFIX/share/applications" || true
    command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" || true
    command -v kbuildsycoca6 >/dev/null && kbuildsycoca6 >/dev/null 2>&1 || true
}

if [ "${1:-}" = "--uninstall" ]; then
    for b in $BINS; do rm -f "$PREFIX/bin/$b"; done
    rm -rf "$PREFIX/share/za2"
    rm -f "$PREFIX/share/applications/za2.desktop" "$PREFIX/share/icons/hicolor/256x256/apps/za2.png"
    refresh
    echo "za2 is removed."
    exit 0
fi

if [ -d "$HERE/bin" ]; then
    BIN_DIR="$HERE/bin"
    ICON="$HERE/za2.png"
else
    BIN_DIR="$HERE/target/release"
    ICON="$HERE/za2/ui/brand/icon-256.png"
fi
for b in $BINS; do
    if [ ! -f "$BIN_DIR/$b" ]; then
        echo "$BIN_DIR/$b is missing. Build za2 first: cargo build --release" >&2
        exit 1
    fi
done

for b in $BINS; do install -Dm755 "$BIN_DIR/$b" "$PREFIX/bin/$b"; done
rm -rf "$PREFIX/share/za2/assets"
mkdir -p "$PREFIX/share/za2"
cp -r "$HERE/assets" "$PREFIX/share/za2/assets"
install -Dm644 "$ICON" "$PREFIX/share/icons/hicolor/256x256/apps/za2.png"
mkdir -p "$PREFIX/share/applications"
cat > "$PREFIX/share/applications/za2.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=za2
GenericName=zodiak for Aion 2
Comment=DPS meter and map for Aion 2
Exec=$PREFIX/bin/za2
Icon=za2
Terminal=false
Categories=Game;Utility;
StartupWMClass=za2
EOF

setcap cap_net_raw,cap_net_admin=eip "$PREFIX/bin/za2-capture"
setcap cap_sys_ptrace=eip "$PREFIX/bin/za2-memory"
refresh

echo "za2 is installed. Start it from the application menu, or run: za2"
