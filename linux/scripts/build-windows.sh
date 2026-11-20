#!/usr/bin/env bash
# Сборка установщика для Windows (Git Bash): окно, служба csqttd, csqtt,
# ядро, wintun.dll и сервер для установки на VPS (если есть dist/server).
# Результат: dist/csqtt-windows-<версия>+core<версия ядра>-x64-setup.exe
#
# Нужны Rust (MSVC), Node.js и wintun.dll в dist/wintun.dll
# (https://www.wintun.net, amd64).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CORE="$(cd "$ROOT/.." && pwd)/rust-client"
DIST="$ROOT/dist"
cd "$ROOT"
[ -f "$DIST/wintun.dll" ] || { echo "нет dist/wintun.dll (https://www.wintun.net)" >&2; exit 1; }

CORE_TARGET="${CSQTT_CORE_TARGET_DIR:-$ROOT/target/core}"
(cd "$CORE" && CARGO_TARGET_DIR="$CORE_TARGET" cargo build --release --locked --ignore-rust-version)
cargo build --release --locked -p csqttd -p csqtt

STAGE="$DIST/windows"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp target/release/csqttd.exe target/release/csqtt.exe "$STAGE/"
cp "$CORE_TARGET/release/client.exe" "$STAGE/csqtt-core.exe"
cp "$DIST/wintun.dll" "$STAGE/"
cp "$ROOT/../app/src/main/assets/domains.txt" "$STAGE/"
RESOURCES='"../../dist/windows/csqttd.exe": "csqttd.exe",
      "../../dist/windows/csqtt.exe": "csqtt.exe",
      "../../dist/windows/csqtt-core.exe": "csqtt-core.exe",
      "../../dist/windows/wintun.dll": "wintun.dll",
      "../../dist/windows/domains.txt": "domains.txt"'
if [ -f "$DIST/server/csqtt-linux-amd64" ]; then
  mkdir -p "$STAGE/server"
  cp "$DIST/server/"* "$STAGE/server/"
  RESOURCES="$RESOURCES,
      \"../../dist/windows/server/\": \"server/\""
fi

cat > "$DIST/tauri.windows.json" <<JSON
{
  "mainBinaryName": "csqtt-gui",
  "bundle": {
    "active": true,
    "targets": ["nsis"],
    "resources": {
      $RESOURCES
    },
    "windows": {
      "nsis": {
        "installMode": "perMachine",
        "installerHooks": "../../packaging/windows/hooks.nsh",
        "languages": ["Russian", "English"]
      }
    }
  }
}
JSON
(cd gui && npm ci --no-audit --no-fund && npx tauri build --config "$DIST/tauri.windows.json")

VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)"
CORE_VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$CORE/Cargo.toml" | head -1)"
SETUP="$(find target/release/bundle/nsis -name '*-setup.exe' | head -1)"
[ -n "$SETUP" ] || { echo "нет собранного установщика" >&2; exit 1; }
OUT="$DIST/csqtt-windows-${VERSION}+core${CORE_VERSION}-x64-setup.exe"
cp "$SETUP" "$OUT"
echo "установщик: $OUT"
