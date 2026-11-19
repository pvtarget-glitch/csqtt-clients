#!/usr/bin/env bash
# Проверка режима «только интерфейс» (роутер с XKeen): служба поднимает
# csqtt0, но не трогает маршруты и DNS; трафик попадает в туннель, только
# если сокет привязан к интерфейсу — так делает Mihomo (interface-name) и
# Xray (sockopt.interface). Запуск: sudo tests/interface-mode.sh
set -uo pipefail
ROOT="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/.." && pwd)"
BIN="${CSQTT_BIN_DIR:-$ROOT/target/debug}"
NS=csqtt-ifm; WORK=/tmp/csqtt-ifm; IFACE=csqtt0
PASSED=0; FAILED=0
ok()   { PASSED=$((PASSED + 1)); echo "  ok   $1"; }
bad()  { FAILED=$((FAILED + 1)); echo "  FAIL $1"; [ -n "${2:-}" ] && echo "       $2"; }
has()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "нет «$2» в: $(echo "$3" | head -4)"; }
hasnt(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "лишнее «$2» в: $(echo "$3" | head -4)"; }
ns()   { ip netns exec "$NS" "$@"; }
cli()  { ns env CSQTT_SOCKET="$WORK/run/csqttd.sock" CSQTT_CONFIG_DIR="$WORK/config" "$BIN/csqtt" "$@"; }

cleanup() {
  [ -n "${PID:-}" ] && kill -TERM "$PID" 2>/dev/null && wait "$PID" 2>/dev/null
  ip netns del "$NS" 2>/dev/null; ip link del ifv0 2>/dev/null
  rm -rf "$WORK" /etc/netns/"$NS"
}
trap cleanup EXIT; cleanup
mkdir -p "$WORK/config" /etc/netns/"$NS"; chmod 755 "$WORK"
install -m 755 "$ROOT/tests/fake-core.py" "$WORK/fake-core"
printf 'nameserver 9.9.9.9\n' > /etc/netns/"$NS"/resolv.conf
ip netns add "$NS"; ip link add ifv0 type veth peer name ifv1; ip link set ifv1 netns "$NS"
ip addr add 10.253.0.1/24 dev ifv0; ip link set ifv0 up
ns sh -c 'ip link set lo up; ip addr add 10.253.0.2/24 dev ifv1; ip link set ifv1 up; ip route add default via 10.253.0.1'
RULES_BEFORE="$(ns ip -4 rule list)"; ROUTES_BEFORE="$(ns ip -4 route show table all | grep -v ' table local')"

cat > "$WORK/csqttd.toml" <<CONF
mode = "interface"
core_user = "root"
core_path = "$WORK/fake-core"
socket_path = "$WORK/run/csqttd.sock"
runtime_dir = "$WORK/run"
domains_template = "/nonexistent"
CONF
cat > "$WORK/config/profiles.toml" <<'CONF'
[[profile]]
name = "router"
host = "203.0.113.7"
port = 46000
password = "ok"
hashes = ["abcdefghijklmnop1"]
CONF

ip netns exec "$NS" "$BIN/csqttd" --config "$WORK/csqttd.toml" >"$WORK/daemon.log" 2>&1 &
PID=$!
for _ in $(seq 50); do [ -S "$WORK/run/csqttd.sock" ] && break; sleep 0.1; done

echo "== режим интерфейса"
UP="$(cli up router 2>&1)"; RC=$?
[ "$RC" = 0 ] && ok "csqtt up подключился" || bad "csqtt up" "$UP"
has "интерфейс с адресом туннеля" "10.99.0.2/32" "$(ns ip -4 addr show dev $IFACE)"
[ "$(ns ip -4 rule list)" = "$RULES_BEFORE" ] && ok "правила ip rule не изменились" || bad "правила изменились" "$(ns ip -4 rule list)"
[ "$(ns ip -4 route show table all | grep -v ' table local')" = "$ROUTES_BEFORE" ] && ok "маршруты не изменились" || bad "маршруты изменились"
has "обычный трафик идёт мимо туннеля" "dev ifv1" "$(ns ip route get 1.1.1.1)"
has "resolv.conf не тронут" "nameserver 9.9.9.9" "$(ns cat /etc/resolv.conf)"
has "ядро работает от root" "uid=0 " "$(cli logs -n 0)"

ns python3 - "$IFACE" <<'PY'
import socket, sys
for port, dst in ((53, "198.51.100.61"),):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE, sys.argv[1].encode())
    s.sendto(b"udp", (dst, port))
t = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
t.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE, sys.argv[1].encode())
t.setblocking(False)
try: t.connect(("198.51.100.62", 443))
except BlockingIOError: pass
u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
u.sendto(b"plain", ("198.51.100.63", 53))
PY
sleep 0.7
LOG="$(cli logs -n 0)"
has "UDP из привязанного сокета дошёл до ядра" "dst=198.51.100.61" "$LOG"
has "TCP из привязанного сокета дошёл до ядра" "dst=198.51.100.62" "$LOG"
hasnt "непривязанный сокет в туннель не попал" "dst=198.51.100.63" "$LOG"

cli down >/dev/null
hasnt "после отключения интерфейса нет" "$IFACE" "$(ns ip link show)"
echo; echo "итого: пройдено $PASSED, провалено $FAILED"; [ "$FAILED" = 0 ]
