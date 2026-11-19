#!/usr/bin/env bash
# Сборка для роутера Keenetic с Entware (aarch64: Giga KN-1012 и другие
# модели на MT7981/MT7986). Статические бинарники musl через cargo-zigbuild,
# ядро — из rust-client в корне форка. Результат:
# dist/csqtt-router-<версия>+core<версия ядра>-aarch64.tar.gz
#
# Нужны rustup, zig и cargo-zigbuild (pip install cargo-zigbuild).
set -euo pipefail

ROOT="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/.." && pwd)"
CORE="$(cd "$ROOT/.." && pwd)/rust-client"
TARGET="${CSQTT_ROUTER_TARGET:-aarch64-unknown-linux-musl}"
DIST="$ROOT/dist"
cd "$ROOT"

cargo zigbuild --help >/dev/null 2>&1 || { echo "нужен cargo-zigbuild: pip install cargo-zigbuild" >&2; exit 1; }
rustup target add "$TARGET"

CORE_FLAGS=(--release --locked --target "$TARGET" --ignore-rust-version)
CORE_TARGET_DIR="${CSQTT_CORE_TARGET_DIR:-$ROOT/target/core-router}"
(cd "$CORE" && CARGO_TARGET_DIR="$CORE_TARGET_DIR" cargo zigbuild "${CORE_FLAGS[@]}")
cargo zigbuild --release --locked --target "$TARGET" -p csqttd -p csqtt

VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)"
CORE_VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$CORE/Cargo.toml" | head -1)"
NAME="csqtt-router-${VERSION}+core${CORE_VERSION}-${TARGET%%-*}"
STAGE="$DIST/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/share"
cp "$CORE_TARGET_DIR/$TARGET/release/client" "$STAGE/bin/csqtt-core"
cp "target/$TARGET/release/csqttd" "target/$TARGET/release/csqtt" "$STAGE/bin/"
cp "$ROOT/../app/src/main/assets/domains.txt" "$STAGE/share/domains.txt"
cp packaging/keenetic/S99csqttd packaging/keenetic/csqttd.toml packaging/keenetic/install.sh "$STAGE/"
cp docs/keenetic.md "$STAGE/README.md"
cp "$ROOT/../LICENSE" "$STAGE/LICENSE"
chmod 755 "$STAGE"/bin/* "$STAGE/S99csqttd" "$STAGE/install.sh"
tar -C "$DIST" -czf "$DIST/$NAME.tar.gz" "$NAME"
rm -rf "$STAGE"
file -b "$CORE_TARGET_DIR/$TARGET/release/client" 2>/dev/null | cut -c1-80 || true
echo "архив: $DIST/$NAME.tar.gz"
