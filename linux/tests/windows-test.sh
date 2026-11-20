#!/usr/bin/env bash
# Проверка службы на Windows с имитатором ядра (Git Bash, от администратора):
# адаптер Wintun, пересылка пакетов в UDP-режим ядра и обратно, маршруты,
# DNS, привязка ядра к физическому интерфейсу, снятие всего при отключении.
#
#   tests/windows-test.sh   (рядом с target/debug/csqttd.exe должен лежать wintun.dll)
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${CSQTT_BIN_DIR:-$ROOT/target/debug}"
WORK="$(cygpath -w "${TEMP:-/tmp}")\\csqtt-win-it"
WORK_UNIX="$(cygpath -u "$WORK")"
PIPE='\\.\pipe\csqttd-it'
PASSED=0
FAILED=0
ok() { PASSED=$((PASSED + 1)); echo "  ok   $1"; }
bad() { FAILED=$((FAILED + 1)); echo "  FAIL $1"; [ -n "${2:-}" ] && echo "$2" | head -15 | sed 's/^/       /'; }
contains() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "нет «$2» в: $3"; }
absent() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "лишнее «$2» в: $3"; }
csqtt() { CSQTT_SOCKET="$PIPE" CSQTT_CONFIG_DIR="$WORK\\config" "$BIN/csqtt.exe" "$@"; }
field() { csqtt status --json | python -c "import json,sys; print(json.load(sys.stdin).get('$1'))"; }

cleanup() {
  [ -n "${PID:-}" ] && kill "$PID" 2>/dev/null
  taskkill //F //IM csqttd.exe > /dev/null 2>&1
  rm -rf "$WORK_UNIX"
}
trap cleanup EXIT
rm -rf "$WORK_UNIX"
mkdir -p "$WORK_UNIX/config"
cp "$ROOT/tests/fake-core.py" "$WORK_UNIX/fake-core.py"
# Служба запускает ядро с системным PATH, поэтому путь к python полный.
PYTHON_WIN="$(cygpath -w "$(command -v python)")"
printf '@"%s" "%%~dp0fake-core.py" %%*\r\n' "$PYTHON_WIN" > "$WORK_UNIX/fake-core.cmd"
cp "$BIN/wintun.dll" "$WORK_UNIX/wintun.dll"

cat > "$WORK_UNIX/csqttd.toml" <<EOF2
core_path = '$WORK\\fake-core.cmd'
socket_path = '$PIPE'
runtime_dir = '$WORK\\run'
domains_template = 'C:\\nonexistent'
EOF2
cat > "$WORK_UNIX/config/profiles.toml" <<'EOF2'
[[profile]]
name = "ok"
host = "203.0.113.7"
port = 46000
password = "ok"
hashes = ["abcdefghijklmnop1"]

[profile.routing]
ip_mode = "blacklist"
ips = ["1.0.0.0/24"]
EOF2

# Физический интерфейс и доступность интернета до подключения.
UPLINK="$(powershell -NoProfile -Command "(Get-NetRoute -DestinationPrefix 0.0.0.0/0 | Sort-Object RouteMetric | Select-Object -First 1).InterfaceIndex" | tr -d '\r')"
probe() { python "$ROOT/tests/bound-probe.py" "$UPLINK" "$1" "$2"; }
BASE_TCP="$(probe tcp 1.1.1.1)"
BASE_DNS="$(probe dns 8.8.8.8)"
echo "физический интерфейс $UPLINK; до подключения: tcp $BASE_TCP, dns $BASE_DNS"
wfp_filters() {
  local count
  rm -f "$WORK_UNIX/filters.xml"
  (cd "$WORK_UNIX" && netsh wfp show filters file=filters.xml > /dev/null 2>&1)
  count="$(grep -c '<name>csqtt</name>' "$WORK_UNIX/filters.xml" 2>/dev/null)"
  echo "${count:-0}"
}

"$BIN/csqttd.exe" --config "$WORK\\csqttd.toml" > "$WORK_UNIX/daemon.log" 2>&1 &
PID=$!
for _ in $(seq 1 50); do csqtt status > /dev/null 2>&1 && break; sleep 0.2; done
csqtt status > /dev/null 2>&1 || { echo "служба не запустилась"; cat "$WORK_UNIX/daemon.log"; exit 1; }

echo "== подключение"
csqtt up ok > /dev/null 2>&1
for _ in $(seq 1 60); do [ "$(field state)" = connected ] && break; sleep 0.25; done
[ "$(field state)" = connected ] && ok "подключено" || bad "подключено" "$(csqtt logs -n 0; cat "$WORK_UNIX/daemon.log")"
contains "адаптер в статусе" "csqtt" "$(field interface)"
contains "адрес туннеля на адаптере" "10.99.0.2" "$(netsh interface ipv4 show addresses csqtt 2>&1)"
contains "маршрут 0/1 в туннель" "0.0.0.0/1" "$(netsh interface ipv4 show route 2>&1)"
LOG="$(csqtt logs -n 0)"
contains "ядро в UDP-режиме" "--listen-peer=127.0.0.1:" "$LOG"
contains "ядро привязано к физическому интерфейсу" "--bind-interface=" "$LOG"
contains "DNS туннеля на адаптере" "1.1.1.1" "$(netsh interface ipv4 show dnsservers csqtt 2>&1)"
PING="$(ping -n 2 -w 3000 198.51.100.77 2>&1)"
contains "пакет дошёл до ядра через пересылку" "dst=198.51.100.77" "$(csqtt logs -n 0)"
contains "ответ ядра вернулся через адаптер" "198.51.100.77: " "$PING"

echo "== исключения по IP и блокировка мимо туннеля (WFP)"
ROUTES="$(netsh interface ipv4 show route 2>&1)"
contains "маршрут исключения через физический интерфейс" "1.0.0.0/24" "$ROUTES"
FILTERS="$(wfp_filters)"
[ "${FILTERS:-0}" -gt 0 ] && ok "фильтры WFP установлены ($FILTERS)" || bad "фильтры WFP установлены" "$(csqtt logs -n 0)"
if [[ "$BASE_TCP" == open* ]]; then
  contains "TCP мимо туннеля заблокирован" "blocked" "$(probe tcp 1.1.1.1)"
  contains "исключение по IP пропускается мимо туннеля" "open" "$(probe tcp 1.0.0.1)"
else
  echo "  --   интернета на машине нет, проверка TCP пропущена"
fi
if [[ "$BASE_DNS" == open* ]]; then
  contains "DNS мимо туннеля заблокирован" "blocked" "$(probe dns 8.8.8.8)"
else
  echo "  --   DNS мимо туннеля недоступен и без csqtt, проверка пропущена"
fi

echo "== отключение"
csqtt down > /dev/null 2>&1
for _ in $(seq 1 30); do [ "$(field state)" = disconnected ] && break; sleep 0.2; done
ROUTES="$(netsh interface ipv4 show route 2>&1)"
absent "маршрут в туннель снят" "0.0.0.0/1" "$ROUTES"
absent "маршрут исключения снят" "1.0.0.0/24" "$ROUTES"
# Служба фильтрации снимает фильтры закрытой сессии не мгновенно.
for _ in $(seq 1 10); do LEFT="$(wfp_filters)"; [ "$LEFT" = 0 ] && break; sleep 0.5; done
[ "$LEFT" = 0 ] && ok "фильтры WFP сняты" || bad "фильтры WFP сняты" "осталось: $LEFT; $(csqtt logs -n 0 | tail -5)"
if [[ "$BASE_TCP" == open* ]]; then
  contains "после отключения сеть снова открыта" "open" "$(probe tcp 1.1.1.1)"
fi

echo
echo "итого: пройдено $PASSED, провалено $FAILED"
[ "$FAILED" = 0 ]
