#!/usr/bin/env bash
# Проверка службы на macOS с имитатором ядра: utun, пересылка пакетов в
# UDP-режим ядра и обратно, маршруты 0/1 + 128/1, DNS через scutil,
# привязка ядра к физическому интерфейсу, снятие всего при отключении.
#
#   sudo tests/macos-test.sh
#
# Внимание: на время проверки весь трафик машины идёт в туннель имитатора
# (около десяти секунд), как при настоящем подключении.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${CSQTT_BIN_DIR:-$ROOT/target/debug}"
WORK=/tmp/csqtt-mac-it
PASSED=0
FAILED=0
ok() { PASSED=$((PASSED + 1)); echo "  ok   $1"; }
bad() { FAILED=$((FAILED + 1)); echo "  FAIL $1"; [ -n "${2:-}" ] && echo "$2" | head -12 | sed 's/^/       /'; }
contains() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "нет «$2» в: $3"; }
absent() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "лишнее «$2» в: $3"; }
csqtt() { CSQTT_SOCKET="$WORK/run/csqttd.sock" CSQTT_CONFIG_DIR="$WORK/config" "$BIN/csqtt" "$@"; }
field() { csqtt status --json | python3 -c "import json,sys; print(json.load(sys.stdin).get('$1'))"; }

cleanup() {
  [ -n "${PID:-}" ] && kill -TERM "$PID" 2>/dev/null && wait "$PID" 2>/dev/null
  rm -rf "$WORK"
}
trap cleanup EXIT
cleanup

mkdir -p "$WORK/config"
chmod 755 "$WORK"
install -m 755 "$ROOT/tests/fake-core.py" "$WORK/fake-core"
DNS_BEFORE="$(scutil --dns | grep 'nameserver\[' | sort -u)"
UPLINK="$(route -n get default | awk '/interface:/{print $2}')"

cat > "$WORK/csqttd.toml" <<EOF
core_path = "$WORK/fake-core"
core_user = "nobody"
socket_path = "$WORK/run/csqttd.sock"
runtime_dir = "$WORK/run"
control_group = "admin"
domains_template = "/nonexistent"
EOF
cat > "$WORK/config/profiles.toml" <<'EOF'
[[profile]]
name = "ok"
host = "203.0.113.7"
port = 46000
password = "ok"
hashes = ["abcdefghijklmnop1"]

[profile.routing]
ip_mode = "blacklist"
ips = ["192.0.2.0/24"]
EOF

"$BIN/csqttd" --config "$WORK/csqttd.toml" > "$WORK/daemon.log" 2>&1 &
PID=$!
for _ in $(seq 1 50); do [ -S "$WORK/run/csqttd.sock" ] && break; sleep 0.1; done
[ -S "$WORK/run/csqttd.sock" ] || { echo "служба не запустилась"; cat "$WORK/daemon.log"; exit 1; }

echo "== подключение"
csqtt up ok > /dev/null 2>&1
for _ in $(seq 1 50); do [ "$(field state)" = connected ] && break; sleep 0.2; done
STATE="$(field state)"
[ "$STATE" = connected ] && ok "подключено" || bad "подключено" "$(csqtt logs -n 0)"
UTUN="$(field interface)"
contains "интерфейс utun в статусе" "utun" "$UTUN"
contains "адрес туннеля на utun" "10.99.0.2" "$(ifconfig "$UTUN" 2>&1)"
contains "маршрут в туннель" "interface: $UTUN" "$(route -n get 198.51.100.77 2>&1)"
contains "локальный маршрут по умолчанию не тронут" "interface: $UPLINK" "$(route -n get default 2>&1)"
contains "исключение по IP идёт мимо туннеля" "interface: $UPLINK" "$(route -n get 192.0.2.10 2>&1)"
LOG="$(csqtt logs -n 0)"
contains "ядро в UDP-режиме" "--listen-peer=127.0.0.1:" "$LOG"
contains "ядро привязано к физическому интерфейсу" "--bind-interface=$UPLINK" "$LOG"
absent "без передачи дескриптора" "--tun-uds" "$LOG"
contains "DNS туннеля в системе" "1.1.1.1" "$(scutil --dns)"

PING="$(ping -c 2 -t 4 198.51.100.77 2>&1)"
contains "пакет дошёл до ядра через пересылку" "dst=198.51.100.77" "$(csqtt logs -n 0)"
contains "ответ ядра вернулся через utun" "bytes from 198.51.100.77" "$PING"

echo "== отключение"
csqtt down > /dev/null 2>&1
for _ in $(seq 1 30); do [ "$(field state)" = disconnected ] && break; sleep 0.2; done
absent "маршрут в туннель снят" "interface: $UTUN" "$(route -n get 198.51.100.77 2>&1)"
absent "маршрут исключения снят" "destination: 192.0.2.0" "$(route -n get 192.0.2.10 2>&1)"
absent "utun удалён" "10.99.0.2" "$(ifconfig 2>&1)"
[ "$(scutil --dns | grep 'nameserver\[' | sort -u)" = "$DNS_BEFORE" ] && ok "DNS восстановлен" || bad "DNS восстановлен" "$(scutil --dns | grep nameserver)"

echo
echo "итого: пройдено $PASSED, провалено $FAILED"
[ "$FAILED" = 0 ]
