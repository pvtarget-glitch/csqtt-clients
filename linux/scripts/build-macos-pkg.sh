#!/usr/bin/env bash
# Сборка установщика для macOS: окно (csqtt.app), служба csqttd (launchd),
# csqtt, ядро и сервер для установки на VPS (если есть dist/server).
# Результат: dist/csqtt-macos-<версия>+core<версия ядра>-<arm64|x86_64>.pkg
#
#   scripts/build-macos-pkg.sh                         # под архитектуру этого Mac
#   CSQTT_MAC_TARGET=x86_64-apple-darwin scripts/...  # Intel на Apple Silicon
#
# Нужны Xcode Command Line Tools, Rust и Node.js. Подписи нет: при первом
# запуске macOS попросит разрешение в «Настройки → Конфиденциальность».
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CORE="$(cd "$ROOT/.." && pwd)/rust-client"
DIST="$ROOT/dist"
cd "$ROOT"

TARGET="${CSQTT_MAC_TARGET:-}"
TARGET_ARGS=()
OUT_SUBDIR="release"
ARCH="$(uname -m)"
if [ -n "$TARGET" ]; then
  rustup target add "$TARGET"
  TARGET_ARGS=(--target "$TARGET")
  OUT_SUBDIR="$TARGET/release"
  ARCH="${TARGET%%-*}"
fi
[ "$ARCH" = "aarch64" ] && ARCH=arm64
export CARGO_TARGET_DIR_CORE="${CSQTT_CORE_TARGET_DIR:-$ROOT/target/core}"
(cd "$CORE" && CARGO_TARGET_DIR="$CARGO_TARGET_DIR_CORE" cargo build --release --locked --ignore-rust-version ${TARGET_ARGS[@]+"${TARGET_ARGS[@]}"})
cargo build --release --locked -p csqttd -p csqtt ${TARGET_ARGS[@]+"${TARGET_ARGS[@]}"}
(cd gui && npm ci --no-audit --no-fund && npx tauri build --bundles app ${TARGET_ARGS[@]+"${TARGET_ARGS[@]}"} --config '{"bundle":{"active":true}}')

VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)"
CORE_VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$CORE/Cargo.toml" | head -1)"
STAGE="$DIST/macos-root-$ARCH"
rm -rf "$STAGE"
mkdir -p "$STAGE/Applications" "$STAGE/usr/local/bin" "$STAGE/usr/local/sbin" \
  "$STAGE/usr/local/lib/csqtt" "$STAGE/usr/local/share/csqtt" "$STAGE/Library/LaunchDaemons"
APP="$(find "target/$OUT_SUBDIR/bundle/macos" -maxdepth 1 -name '*.app' | head -1)"
[ -n "$APP" ] || { echo "нет собранного .app" >&2; exit 1; }
cp -R "$APP" "$STAGE/Applications/csqtt.app"
install -m 755 "target/$OUT_SUBDIR/csqttd" "$STAGE/usr/local/sbin/csqttd"
install -m 755 "target/$OUT_SUBDIR/csqtt" "$STAGE/usr/local/bin/csqtt"
install -m 755 "$CARGO_TARGET_DIR_CORE/$OUT_SUBDIR/client" "$STAGE/usr/local/lib/csqtt/csqtt-core"
install -m 755 packaging/macos/uninstall.sh "$STAGE/usr/local/share/csqtt/uninstall.sh"
install -m 644 "$ROOT/../app/src/main/assets/domains.txt" "$STAGE/usr/local/share/csqtt/domains.txt"
install -m 644 packaging/macos/csqttd.toml "$STAGE/usr/local/share/csqtt/csqttd.toml"
install -m 644 packaging/macos/io.github.csqtt.daemon.plist "$STAGE/Library/LaunchDaemons/"
if [ -x "$DIST/server/csqtt-linux-amd64" ]; then
  mkdir -p "$STAGE/usr/local/share/csqtt/server"
  cp "$DIST/server/"* "$STAGE/usr/local/share/csqtt/server/"
fi

PKG="$DIST/csqtt-macos-${VERSION}+core${CORE_VERSION}-${ARCH}.pkg"
# Без этого установщик «переносит» приложение туда, где нашёл копию с тем
# же идентификатором (например, в каталог сборки), а не в /Applications.
COMPONENTS="$DIST/components-$ARCH.plist"
pkgbuild --analyze --root "$STAGE" "$COMPONENTS"
count="$(plutil -extract . raw "$COMPONENTS" 2>/dev/null || echo 1)"
for ((i = 0; i < count; i++)); do
  plutil -replace "$i.BundleIsRelocatable" -bool NO "$COMPONENTS"
done
pkgbuild --root "$STAGE" --component-plist "$COMPONENTS" --identifier io.github.csqtt \
  --version "$VERSION" --scripts packaging/macos/scripts --install-location / "$PKG"
rm -f "$COMPONENTS"
rm -rf "$STAGE"
echo "пакет: $PKG"
