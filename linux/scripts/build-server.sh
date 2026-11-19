#!/usr/bin/env bash
# Собирает сервер csqtt (rust-server из корня форка) под VPS: статические
# бинарники musl для x86_64, aarch64 и armv7 — как build_linux.sh автора,
# но без PowerShell. Результат: dist/server/csqtt-linux-{amd64,arm64,armv7}
# и deploy.sh — их ставит на VPS команда `csqtt server install`.
#
# Нужны rustup, zig и cargo-zigbuild (проще всего: pip install cargo-zigbuild).
#   scripts/build-server.sh          # собрать
#   scripts/build-server.sh --tests  # прогнать тесты сервера
set -euo pipefail

ROOT="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/.." && pwd)"
SERVER="$(cd "$ROOT/.." && pwd)/rust-server"
OUT="$ROOT/dist/server"
[ -f "$SERVER/Cargo.toml" ] || { echo "нет $SERVER — это не форк csqtt?" >&2; exit 1; }
cargo zigbuild --help >/dev/null 2>&1 || { echo "нужен cargo-zigbuild: pip install cargo-zigbuild" >&2; exit 1; }

TOOLCHAIN="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$SERVER/rust-toolchain.toml")"
TARGETS=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl armv7-unknown-linux-musleabihf)
rustup toolchain install "$TOOLCHAIN" --profile minimal
rustup target add "${TARGETS[@]}" --toolchain "$TOOLCHAIN"

mkdir -p "$OUT"
export CARGO_TARGET_DIR="${CSQTT_SERVER_TARGET_DIR:-$ROOT/target/server}"

# Тесты сервера (--tests): под x86_64-musl, C/C++ — через zig, как у автора.
if [ "${1:-}" = "--tests" ]; then
  WRAP="$CARGO_TARGET_DIR/zig-wrappers"
  mkdir -p "$WRAP"
  if command -v zig >/dev/null; then ZIG=(zig); else ZIG=(python3 -m ziglang); fi
  for tool in cc c++; do
    {
      echo '#!/usr/bin/env bash'
      echo 'args=(); for arg in "$@"; do [[ "$arg" == --target=* ]] || args+=("$arg"); done'
      echo "exec ${ZIG[*]} $tool -target x86_64-linux-musl \"\${args[@]}\""
    } > "$WRAP/zig$tool"
    chmod +x "$WRAP/zig$tool"
  done
  printf '#!/usr/bin/env bash\nexec %s ar "$@"\n' "${ZIG[*]}" > "$WRAP/zigar"
  chmod +x "$WRAP/zigar"
  export CC_x86_64_unknown_linux_musl="$WRAP/zigcc" CXX_x86_64_unknown_linux_musl="$WRAP/zigc++" \
    AR_x86_64_unknown_linux_musl="$WRAP/zigar" CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$WRAP/zigcc"
  (cd "$SERVER" && cargo "+$TOOLCHAIN" test --release --locked --target x86_64-unknown-linux-musl)
  exit 0
fi
build() {
  local target="$1" asset="$2"
  (cd "$SERVER" && cargo "+$TOOLCHAIN" zigbuild --release --locked --target "$target")
  install -m 755 "$CARGO_TARGET_DIR/$target/release/csqtt" "$OUT/$asset"
  echo "$asset: $(du -h "$OUT/$asset" | cut -f1)"
}
build x86_64-unknown-linux-musl csqtt-linux-amd64
build aarch64-unknown-linux-musl csqtt-linux-arm64
build armv7-unknown-linux-musleabihf csqtt-linux-armv7
install -m 644 "$ROOT/../app/src/main/assets/deploy.sh" "$OUT/deploy.sh"
VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$SERVER/Cargo.toml" | head -1)"
printf '%s\n' "$VERSION" > "$OUT/version"
echo "сервер csqtt $VERSION → $OUT"
