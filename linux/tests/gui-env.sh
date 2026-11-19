#!/usr/bin/env bash
# Стенд для ручной и автоматической проверки GUI: настоящий csqttd с
# имитатором ядра в отдельном network namespace. Маршруты основной системы
# не трогаются; окно и CLI подключаются к сокету стенда.
#
#   sudo tests/gui-env.sh up      # поднять стенд, напечатать переменные окружения
#   sudo tests/gui-env.sh down    # убрать
set -uo pipefail

ROOT="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/.." && pwd)"
BIN="${CSQTT_BIN_DIR:-$ROOT/target/debug}"
NS=csqtt-gui
WORK=/tmp/csqtt-gui-it
FAKE=/usr/local/lib/csqtt-gui-it/fake-core

down() {
  [ -f "$WORK/daemon.pid" ] && kill -TERM "$(cat "$WORK/daemon.pid")" 2>/dev/null && sleep 1
  ip netns del "$NS" 2>/dev/null
  ip link del guv0 2>/dev/null
  rm -rf "$WORK" /etc/netns/"$NS" /usr/local/lib/csqtt-gui-it
}

up() {
  down
  id csqtt-core >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin csqtt-core
  mkdir -p "$WORK/config" "$WORK/xdg" /usr/local/lib/csqtt-gui-it /etc/netns/"$NS"
  chmod 755 "$WORK"
  install -m 755 "$ROOT/tests/fake-core.py" "$FAKE"
  printf 'nameserver 9.9.9.9\n' > /etc/netns/"$NS"/resolv.conf
  ip netns add "$NS"
  ip link add guv0 type veth peer name guv1
  ip link set guv1 netns "$NS"
  ip addr add 10.252.0.1/24 dev guv0 && ip link set guv0 up
  ip netns exec "$NS" sh -c 'ip link set lo up; ip addr add 10.252.0.2/24 dev guv1; ip link set guv1 up; ip route add default via 10.252.0.1'
  cat > "$WORK/csqttd.toml" <<CONF
core_path = "$FAKE"
socket_path = "$WORK/run/csqttd.sock"
runtime_dir = "$WORK/run"
interface = "csqttgui0"
table = 51998
rule_priority = 9100
dns = "resolvconf"
domains_template = "/nonexistent"
CONF
  ip netns exec "$NS" "$BIN/csqttd" --config "$WORK/csqttd.toml" >"$WORK/daemon.log" 2>&1 &
  echo $! > "$WORK/daemon.pid"
  for _ in $(seq 1 50); do [ -S "$WORK/run/csqttd.sock" ] && break; sleep 0.1; done
  [ -S "$WORK/run/csqttd.sock" ] || { echo "демон не стартовал"; cat "$WORK/daemon.log"; exit 1; }
  echo "export CSQTT_SOCKET=$WORK/run/csqttd.sock CSQTT_CONFIG_DIR=$WORK/config XDG_CONFIG_HOME=$WORK/xdg"
}

case "${1:-}" in
  up) up ;;
  down) down ;;
  *) echo "использование: $0 up|down"; exit 2 ;;
esac
