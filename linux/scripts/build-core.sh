#!/usr/bin/env bash
# Собирает ядро csqtt (../rust-client — код amurcanov в корне форка) под
# Linux без единой правки. Результат: dist/csqtt-core и dist/domains.txt.
#
# Новая версия ядра приходит в форк кнопкой «Sync fork» на GitHub
# (или: git fetch upstream && git merge upstream/main), затем:
#   scripts/build-core.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/.." && pwd)"
CORE="$(cd "$ROOT/.." && pwd)"
DIST="$ROOT/dist"

[ -f "$CORE/rust-client/Cargo.toml" ] || { echo "нет $CORE/rust-client — папка linux/ должна лежать в корне форка csqtt" >&2; exit 1; }

command -v cargo >/dev/null || { echo "нужен Rust: https://rustup.rs" >&2; exit 1; }
# aws-lc-sys собирается из C-исходников.
for tool in cc cmake; do
  command -v "$tool" >/dev/null || { echo "нужен $tool: sudo apt install build-essential cmake" >&2; exit 1; }
done

REQUIRED="$(sed -n 's/^rust-version *= *"\(.*\)"/\1/p' "$CORE/rust-client/Cargo.toml")"
HAVE="$(rustc --version | awk '{print $2}')"
FLAGS=(--release --locked)
if [ -n "$REQUIRED" ] && [ "$(printf '%s\n%s\n' "$REQUIRED" "$HAVE" | sort -V | head -1)" != "$REQUIRED" ]; then
  echo "внимание: ядро просит Rust $REQUIRED, установлен $HAVE — собираю с --ignore-rust-version" >&2
  echo "          (надёжнее обновить: rustup update)" >&2
  FLAGS+=(--ignore-rust-version)
fi

# Отдельный target-каталог: код ядра остаётся чистым.
export CARGO_TARGET_DIR="${CSQTT_CORE_TARGET_DIR:-$ROOT/target/core}"
(cd "$CORE/rust-client" && cargo build "${FLAGS[@]}")

mkdir -p "$DIST"
install -m 755 "$CARGO_TARGET_DIR/release/client" "$DIST/csqtt-core"
install -m 644 "$CORE/app/src/main/assets/domains.txt" "$DIST/domains.txt"
VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$CORE/rust-client/Cargo.toml" | head -1)"
COMMIT="$(git -C "$CORE" rev-parse --short HEAD)"
printf '%s\n' "$VERSION" > "$DIST/core-version"
echo "ядро csqtt $VERSION ($COMMIT) → $DIST/csqtt-core"
