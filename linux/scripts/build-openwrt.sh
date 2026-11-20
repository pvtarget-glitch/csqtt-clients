#!/usr/bin/env bash
# Сборка для роутеров с OpenWrt: статические бинарники musl через
# cargo-zigbuild, ядро — из rust-client в корне форка. Результат:
# dist/csqtt-openwrt-<версия>+core<версия ядра>-<arch>.tar.gz
#
#   CSQTT_OPENWRT_TARGET=aarch64-unknown-linux-musl (по умолчанию)
#                        x86_64-unknown-linux-musl
#                        armv7-unknown-linux-musleabihf
#
# Нужны rustup, zig и cargo-zigbuild (pip install cargo-zigbuild).
set -euo pipefail

ROOT="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/.." && pwd)"
CORE="$(cd "$ROOT/.." && pwd)/rust-client"
TARGET="${CSQTT_OPENWRT_TARGET:-aarch64-unknown-linux-musl}"
DIST="$ROOT/dist"
cd "$ROOT"

case "$TARGET" in
  aarch64-*) ARCH=aarch64 ;;
  x86_64-*) ARCH=x86_64 ;;
  armv7-*) ARCH=armv7 ;;
  *) echo "неизвестная цель $TARGET" >&2; exit 1 ;;
esac

cargo zigbuild --help >/dev/null 2>&1 || { echo "нужен cargo-zigbuild: pip install cargo-zigbuild" >&2; exit 1; }
rustup target add "$TARGET"

CORE_TARGET_DIR="${CSQTT_CORE_TARGET_DIR:-$ROOT/target/core-router}"
(cd "$CORE" && CARGO_TARGET_DIR="$CORE_TARGET_DIR" cargo zigbuild --release --locked --target "$TARGET" --ignore-rust-version)
cargo zigbuild --release --locked --target "$TARGET" -p csqttd -p csqtt

VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)"
CORE_VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$CORE/Cargo.toml" | head -1)"
NAME="csqtt-openwrt-${VERSION}+core${CORE_VERSION}-${ARCH}"
STAGE="$DIST/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/share"
cp "$CORE_TARGET_DIR/$TARGET/release/client" "$STAGE/bin/csqtt-core"
cp "target/$TARGET/release/csqttd" "target/$TARGET/release/csqtt" "$STAGE/bin/"
cp "$ROOT/../app/src/main/assets/domains.txt" "$STAGE/share/domains.txt"
cp packaging/openwrt/csqttd.init packaging/openwrt/csqttd.toml packaging/openwrt/install.sh packaging/openwrt/uninstall.sh "$STAGE/"
cp docs/openwrt.md "$STAGE/README.md"
cp "$ROOT/../LICENSE" "$STAGE/LICENSE"
echo "$ARCH" > "$STAGE/ARCH"
chmod 755 "$STAGE"/bin/* "$STAGE/csqttd.init" "$STAGE/install.sh" "$STAGE/uninstall.sh"
tar -C "$DIST" -czf "$DIST/$NAME.tar.gz" "$NAME"
rm -rf "$STAGE"
file -b "$CORE_TARGET_DIR/$TARGET/release/client" 2>/dev/null | cut -c1-80 || true
echo "архив: $DIST/$NAME.tar.gz"
