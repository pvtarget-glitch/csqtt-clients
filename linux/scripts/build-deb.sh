#!/usr/bin/env bash
# Собирает .deb для Ubuntu/Debian/Mint: демон, CLI, окно (GUI), ядро,
# окно капчи, юниты.
#
#   scripts/build-deb.sh            # всё собрать и упаковать в dist/
#   scripts/build-deb.sh --no-core  # не пересобирать ядро (взять dist/csqtt-core)
set -euo pipefail

ROOT="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/.." && pwd)"
DIST="$ROOT/dist"
cd "$ROOT"

if [ "${1:-}" != "--no-core" ] || [ ! -x "$DIST/csqtt-core" ]; then
  "$ROOT/scripts/build-core.sh"
fi
cargo build --release --locked -p csqttd -p csqtt

# Окно: интерфейс на Vue собирается и вшивается в бинарник Tauri.
command -v npm >/dev/null || { echo "нужен Node.js с npm (для сборки окна)" >&2; exit 1; }
for lib in webkit2gtk-4.1 gtk+-3.0 ayatana-appindicator3-0.1; do
  pkg-config --exists "$lib" || {
    echo "нет $lib: sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev" >&2
    exit 1
  }
done
(cd gui && npm ci --no-audit --no-fund && npx tauri build --no-bundle)

VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)"
CORE_VERSION="$(cat "$DIST/core-version")"
ARCH="$(dpkg --print-architecture)"
PACKAGE="csqtt-linux_${VERSION}+core${CORE_VERSION}_${ARCH}"
STAGE="$DIST/$PACKAGE"
rm -rf "$STAGE"

install -D -m 755 target/release/csqttd            "$STAGE/usr/sbin/csqttd"
install -D -m 755 target/release/csqtt             "$STAGE/usr/bin/csqtt"
install -D -m 755 target/release/csqtt-gui         "$STAGE/usr/bin/csqtt-gui"
install -D -m 644 packaging/csqtt.desktop          "$STAGE/usr/share/applications/csqtt.desktop"
install -D -m 644 gui/src-tauri/icons/icon.svg     "$STAGE/usr/share/icons/hicolor/scalable/apps/csqtt.svg"
for size in 32 48 64 128 256; do
  mkdir -p "$STAGE/usr/share/icons/hicolor/${size}x${size}/apps"
  rsvg-convert -w "$size" -h "$size" gui/src-tauri/icons/icon.svg \
    -o "$STAGE/usr/share/icons/hicolor/${size}x${size}/apps/csqtt.png"
done
install -D -m 755 "$DIST/csqtt-core"               "$STAGE/usr/lib/csqtt/csqtt-core"
install -D -m 755 helpers/csqtt-captcha            "$STAGE/usr/lib/csqtt/csqtt-captcha"
install -D -m 644 "$DIST/domains.txt"              "$STAGE/usr/share/csqtt/domains.txt"
# Сервер для `csqtt server install` (собирается scripts/build-server.sh).
if [ -x "$DIST/server/csqtt-linux-amd64" ]; then
  for file in deploy.sh version csqtt-linux-amd64 csqtt-linux-arm64 csqtt-linux-armv7; do
    mode=755; case "$file" in deploy.sh|version) mode=644 ;; esac
    install -D -m "$mode" "$DIST/server/$file" "$STAGE/usr/share/csqtt/server/$file"
  done
else
  echo "внимание: нет dist/server — пакет без сервера для установки на VPS (scripts/build-server.sh)" >&2
fi
install -D -m 644 packaging/csqttd.service         "$STAGE/usr/lib/systemd/system/csqttd.service"
install -D -m 644 packaging/csqtt-agent.service    "$STAGE/usr/lib/systemd/user/csqtt-agent.service"
install -D -m 644 packaging/csqttd.toml            "$STAGE/etc/csqtt/csqttd.toml"
install -D -m 644 README.md                        "$STAGE/usr/share/doc/csqtt-linux/README.md"
install -D -m 644 ../LICENSE                       "$STAGE/usr/share/doc/csqtt-linux/copyright"

# Минимальная версия glibc — по символам, которые реально нужны бинарникам.
GLIBC="$(objdump -T "$STAGE/usr/sbin/csqttd" "$STAGE/usr/bin/csqtt" "$STAGE/usr/bin/csqtt-gui" "$STAGE/usr/lib/csqtt/csqtt-core" \
  | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -V | tail -1)"
SIZE="$(du -sk "$STAGE" | cut -f1)"

mkdir -p "$STAGE/DEBIAN"
cat > "$STAGE/DEBIAN/control" <<CONTROL
Package: csqtt-linux
Version: ${VERSION}+core${CORE_VERSION}
Architecture: ${ARCH}
Maintainer: csqtt-linux local build <root@localhost>
Section: net
Priority: optional
Installed-Size: ${SIZE}
Depends: libc6 (>= ${GLIBC}), libgcc-s1, iproute2, libwebkit2gtk-4.1-0, libgtk-3-0, libayatana-appindicator3-1
Recommends: nftables, python3, python3-gi, gir1.2-gtk-3.0, gir1.2-webkit2-4.1, libnotify-bin, curl, pkexec | policykit-1
Homepage: https://github.com/pvtarget-glitch/csqtt-clients
Description: Linux client for the csqtt tunnel
 Daemon (csqttd), desktop window (csqtt-gui), command-line tool (csqtt)
 and manual captcha window for the csqtt TURN/RTP tunnel. Runs the csqtt core
 ${CORE_VERSION} as an unprivileged subprocess and provides the Linux side:
 TUN interface, policy routing, DNS, per-application bypass and process
 supervision.
 .
 Non-commercial use only (PolyForm Noncommercial 1.0.0, as the core).
CONTROL
printf '/etc/csqtt/csqttd.toml\n' > "$STAGE/DEBIAN/conffiles"
install -m 755 packaging/debian/postinst packaging/debian/prerm packaging/debian/postrm "$STAGE/DEBIAN/"

dpkg-deb --root-owner-group --build "$STAGE" "$DIST/$PACKAGE.deb" >/dev/null
rm -rf "$STAGE"
echo "пакет: $DIST/$PACKAGE.deb (glibc >= $GLIBC)"
