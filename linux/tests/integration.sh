#!/usr/bin/env bash
# Интеграционный тест демона csqttd с имитатором ядра (tests/fake-core.py).
#
# Проверяет настоящую сетевую часть — TUN, передачу fd, policy routing, DNS,
# перезапуски — в отдельном network namespace, поэтому маршруты основной
# системы не затрагиваются. Нужен root и собранный проект (cargo build).
#
#   sudo tests/integration.sh
set -uo pipefail

ROOT="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/.." && pwd)"
BIN="${CSQTT_BIN_DIR:-$ROOT/target/debug}"
NS=csqtt-it
WORK=/tmp/csqtt-it
IFACE=csqttit0
TABLE=51999
PRIO=9000
CORE_USER=csqtt-core
IT_USER=csqtt-it-user
FAKE=/usr/local/lib/csqtt-it/fake-core

[ "$(id -u)" = 0 ] || { echo "нужен root"; exit 2; }
[ -x "$BIN/csqttd" ] && [ -x "$BIN/csqtt" ] || { echo "сначала cargo build (нет $BIN/csqttd)"; exit 2; }

FAILED=0
PASSED=0
pass() { PASSED=$((PASSED + 1)); echo "  ok   $1"; }
fail() { FAILED=$((FAILED + 1)); echo "  FAIL $1"; [ -n "${2:-}" ] && echo "       $2"; }
check() { # check "описание" команда...
  local name="$1"; shift
  if "$@" >/dev/null 2>&1; then pass "$name"; else fail "$name" "команда: $*"; fi
}
contains() { # contains "описание" "подстрока" "текст"
  if [[ "$3" == *"$2"* ]]; then pass "$1"; else fail "$1" "нет «$2» в: $(echo "$3" | head -5)"; fi
}
absent() {
  if [[ "$3" != *"$2"* ]]; then pass "$1"; else fail "$1" "лишнее «$2» в: $(echo "$3" | head -5)"; fi
}

ns() { ip netns exec "$NS" "$@"; }
csqtt() { ns env CSQTT_SOCKET="$WORK/run/csqttd.sock" CSQTT_CONFIG_DIR="$WORK/config" "$BIN/csqtt" "$@"; }
state() { csqtt status --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["state"])'; }
field() { csqtt status --json | python3 -c "import json,sys; print(json.load(sys.stdin).get('$1'))"; }
wait_state() { # wait_state состояние секунд
  local i
  for i in $(seq 1 $(( $2 * 5 ))); do
    [ "$(state 2>/dev/null)" = "$1" ] && return 0
    sleep 0.2
  done
  return 1
}
daemon_log() { cat "$WORK/daemon.log"; }
core_log() { csqtt logs -n 500; }

start_daemon() {
  # Без функции-обёртки: $! должен быть PID самого демона (ip делает exec).
  ip netns exec "$NS" "$BIN/csqttd" --config "$WORK/csqttd.toml" >>"$WORK/daemon.log" 2>&1 &
  DAEMON_PID=$!
  local i
  for i in $(seq 1 50); do [ -S "$WORK/run/csqttd.sock" ] && return 0; sleep 0.1; done
  echo "демон не стартовал:"; daemon_log; return 1
}
stop_daemon() {
  [ -n "${DAEMON_PID:-}" ] || return 0
  kill -TERM "$DAEMON_PID" 2>/dev/null
  wait "$DAEMON_PID" 2>/dev/null
  DAEMON_PID=
}

cleanup() {
  stop_daemon
  pkill -f "$FAKE" 2>/dev/null
  ip netns del "$NS" 2>/dev/null
  ip link del itv0 2>/dev/null
  rm -rf "$WORK" /etc/netns/"$NS" /usr/local/lib/csqtt-it
  id "$IT_USER" >/dev/null 2>&1 && userdel "$IT_USER" 2>/dev/null
  [ -n "${CREATED_GROUP:-}" ] && groupdel csqtt 2>/dev/null
  return 0
}
trap cleanup EXIT
cleanup

# --- окружение ---------------------------------------------------------------
id "$CORE_USER" >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin "$CORE_USER"
getent group csqtt >/dev/null || { groupadd --system csqtt; CREATED_GROUP=1; }
useradd --system --no-create-home --shell /usr/sbin/nologin --user-group -G csqtt "$IT_USER"
CORE_UID="$(id -u "$CORE_USER")"
mkdir -p "$WORK/config" /usr/local/lib/csqtt-it /etc/netns/"$NS"
chmod 755 "$WORK"
install -m 755 "$ROOT/tests/fake-core.py" "$FAKE"
printf 'nameserver 9.9.9.9\n' > /etc/netns/"$NS"/resolv.conf

ip netns add "$NS"
ip link add itv0 type veth peer name itv1
ip link set itv1 netns "$NS"
ip addr add 10.250.0.1/24 dev itv0 && ip link set itv0 up
ns ip link set lo up
ns ip addr add 10.250.0.2/24 dev itv1
ns ip link set itv1 up
ns ip route add default via 10.250.0.1
# Строгий rp_filter: демон обязан сам перевести его в loose.
ns sh -c 'echo 1 > /proc/sys/net/ipv4/conf/all/rp_filter'

cat > "$WORK/csqttd.toml" <<EOF
core_path = "$FAKE"
core_user = "$CORE_USER"
socket_path = "$WORK/run/csqttd.sock"
runtime_dir = "$WORK/run"
interface = "$IFACE"
table = $TABLE
rule_priority = $PRIO
dns = "resolvconf"
domains_template = "/nonexistent"
EOF

profile() { # profile имя сценарий [доп. TOML]
  cat >> "$WORK/config/profiles.toml" <<EOF
[[profile]]
name = "$1"
host = "203.0.113.7"
port = 46000
password = "$2"
hashes = ["abcdefghijklmnop1", "abcdefghijklmnop2"]
${3:-}

EOF
}
: > "$WORK/config/profiles.toml"
for scenario in ok captcha fatal crash restart routing tunlost wide silent; do profile "$scenario" "$scenario"; done
profile persistent crash "persistent = true"

cat > "$WORK/captcha-stub" <<'EOF'
#!/bin/sh
echo "$@" > /tmp/csqtt-it/captcha-args
echo "TOKEN tok_stub_42"
EOF
chmod 755 "$WORK/captcha-stub"

start_daemon || exit 1

# --- ссылки и профили --------------------------------------------------------
echo "== ссылки csqtt://"
LINK="$(csqtt export ok)"
contains "export даёт ссылку csqtt://" "csqtt://" "$LINK"
IMPORTED="$(ns env CSQTT_CONFIG_DIR="$WORK/config2" "$BIN/csqtt" import "$LINK" --name imported 2>&1)"
contains "import разбирает свою же ссылку" "203.0.113.7:46000 · хешей: 2" "$IMPORTED"
check "файл профилей закрыт от других (0600)" test "$(stat -c %a "$WORK/config2/profiles.toml")" = 600
SHOWN="$(ns env CSQTT_CONFIG_DIR="$WORK/config2" "$BIN/csqtt" show imported)"
absent "show скрывает пароль" 'password = "ok"' "$SHOWN"

# --- основной сценарий -------------------------------------------------------
echo "== подключение"
UP="$(csqtt up ok 2>&1)"; RC=$?
check "csqtt up завершился успешно" test "$RC" = 0
contains "up печатает статус" "подключено" "$UP"
check "состояние connected" test "$(state)" = connected
check "адрес туннеля в статусе" test "$(field tunnel_ip)" = 10.99.0.2
contains "адрес на интерфейсе" "10.99.0.2/32" "$(ns ip -4 addr show dev $IFACE)"
contains "MTU 1500" "mtu 1500" "$(ns ip link show dev $IFACE)"
RULES="$(ns ip -4 rule list)"
contains "правило UID ядра" "$PRIO:	from all uidrange $CORE_UID-$CORE_UID lookup main" "$RULES"
contains "правило локальных сетей" "$((PRIO + 1)):	from all lookup main suppress_prefixlength 0" "$RULES"
contains "правило таблицы туннеля" "$((PRIO + 2)):	from all lookup $TABLE" "$RULES"
contains "default в таблице туннеля" "default dev $IFACE" "$(ns ip -4 route show table $TABLE)"
contains "обычный трафик уходит в туннель" "dev $IFACE" "$(ns ip route get 1.1.1.1)"
contains "трафик ядра идёт мимо туннеля" "dev itv1" "$(ns ip route get 1.1.1.1 uid "$CORE_UID")"
contains "локальная сеть остаётся локальной" "dev itv1" "$(ns ip route get 10.250.0.1)"
check "rp_filter переведён в loose" test "$(ns cat /proc/sys/net/ipv4/conf/all/rp_filter)" = 2
RESOLV="$(ns cat /etc/resolv.conf)"
contains "DNS туннеля в resolv.conf" "nameserver 1.1.1.1" "$RESOLV"
contains "второй DNS туннеля" "nameserver 8.8.8.8" "$RESOLV"
absent "прежний DNS убран" "9.9.9.9" "$RESOLV"

ns ping -c 1 -W 1 198.51.100.77 >/dev/null 2>&1
sleep 0.5
LOG="$(core_log)"
contains "ядро работает от своего пользователя" "uid=$CORE_UID " "$LOG"
contains "ядро получило TUN fd" "TUN fd #1 received" "$LOG"
contains "пакет дошёл до ядра через переданный fd" "packet 84 bytes dst=198.51.100.77" "$LOG"
contains "argv: peer" "--peer=203.0.113.7:46000" "$LOG"
contains "argv: хеши" "--vk=abcdefghijklmnop1,abcdefghijklmnop2" "$LOG"
contains "argv: режим хешей" "--vk-hash-mode=manual" "$LOG"
absent "пароль не попал в argv" "--password" "$LOG"
# LC_CTYPE добавляет сам интерпретатор Python имитатора (PEP 538).
contains "окружение ядра очищено" "env keys: CSQTT_EVENTS,CSQTT_PASSWORD,HOME,LC_CTYPE,PATH" "$LOG"
check "счётчики трафика обновляются" test "$(field bytes_down)" -gt 0
check "число потоков из профиля" test "$(field workers_target)" = 10
contains "журнал с начала сессии" "ядро запущено (поколение 1" "$(csqtt logs -n 0 | head -1)"
check "csqtt logs | head завершается без падения" bash -c "ip netns exec $NS env CSQTT_SOCKET=$WORK/run/csqttd.sock $BIN/csqtt logs -n 0 | head -1 >/dev/null; test \${PIPESTATUS[0]} != 134"
check "повторный up отклонён" bash -c "! ip netns exec $NS env CSQTT_SOCKET=$WORK/run/csqttd.sock CSQTT_CONFIG_DIR=$WORK/config $BIN/csqtt up ok"

echo "== права доступа к сокету"
# Домашний каталог раннера CI закрыт для других пользователей (0750) —
# кладём клиент туда, откуда его может запустить кто угодно.
install -m 0755 "$BIN/csqtt" "$WORK/csqtt-client"
DENIED="$(ns setpriv --reuid nobody --regid nogroup --clear-groups env CSQTT_SOCKET="$WORK/run/csqttd.sock" "$WORK/csqtt-client" status 2>&1)"
contains "посторонний пользователь не управляет туннелем" "нет прав" "$DENIED"
ALLOWED="$(ns setpriv --reuid "$IT_USER" --regid "$IT_USER" --init-groups env CSQTT_SOCKET="$WORK/run/csqttd.sock" "$WORK/csqtt-client" status 2>&1)"
contains "участник группы csqtt управляет туннелем" "подключено" "$ALLOWED"

echo "== отключение"
csqtt down >/dev/null
check "состояние disconnected" test "$(state)" = disconnected
absent "правила сняты" "lookup $TABLE" "$(ns ip -4 rule list)"
absent "правило UID снято" "uidrange" "$(ns ip -4 rule list)"
check "интерфейс удалён" bash -c "! ip netns exec $NS ip link show dev $IFACE"
contains "resolv.conf восстановлен" "nameserver 9.9.9.9" "$(ns cat /etc/resolv.conf)"
check "rp_filter возвращён" test "$(ns cat /proc/sys/net/ipv4/conf/all/rp_filter)" = 1
contains "ядро остановлено командой STOP" "STOP received" "$(core_log)"

# --- капча -------------------------------------------------------------------
echo "== капча"
UP="$(ns env CSQTT_SOCKET="$WORK/run/csqttd.sock" CSQTT_CONFIG_DIR="$WORK/config" \
  CSQTT_CAPTCHA_HELPER="$WORK/captcha-stub" DISPLAY=:99 "$BIN/csqtt" up captcha 2>&1)"; RC=$?
check "up с капчей подключился" test "$RC" = 0
LOG="$(core_log)"
contains "авто-WebView сразу отклонён" "captcha result: error:auto webview is not available on linux" "$LOG"
contains "чужой адрес капчи отклонён" "captcha result: error:invalid captcha request" "$LOG"
contains "токен ручной капчи передан ядру" "captcha result: tok_stub_42" "$LOG"
contains "окну передан адрес VK" "--url https://id.vk.ru/not_robot_captcha?session_token=abc --timeout 55" "$(cat "$WORK/captcha-args" 2>/dev/null)"
check "капча закрыта в статусе" test "$(field captcha)" = None
csqtt down >/dev/null

# --- фатальная ошибка --------------------------------------------------------
echo "== фатальная ошибка ядра"
UP="$(csqtt up fatal 2>&1)"; RC=$?
check "up вернул ошибку" test "$RC" != 0
contains "причина показана пользователю" "FATAL_AUTH: invalid password" "$UP"
check "состояние failed" test "$(state)" = failed
check "после ошибки сети нет" bash -c "! ip netns exec $NS ip link show dev $IFACE"

echo "== ядро падает при запуске"
UP="$(csqtt up crash 2>&1)"; RC=$?
check "up вернул ошибку" test "$RC" != 0
contains "сообщение о падениях до подключения" "завершилось до подключения (код 7)" "$UP"

# --- перезапуск с удержанием маршрутов ---------------------------------------
echo "== перезапуск ядра"
csqtt up restart >/dev/null 2>&1
HELD=1
for i in $(seq 1 40); do
  # Без конвейера с grep -q: при pipefail ранний выход grep даёт ложный сбой.
  [[ "$(ns ip -4 rule list)" == *"lookup $TABLE"* ]] || { HELD=0; echo "       [$i] нет правила: $(state)"; }
  ns ip link show dev $IFACE >/dev/null 2>&1 || { HELD=0; echo "       [$i] нет интерфейса: $(state)"; }
  [ "$(field restarts)" = 1 ] && [ "$(state)" = connected ] && break
  sleep 0.25
done
check "ядро перезапущено и снова подключено" test "$(field restarts)" = 1 -a "$(state)" = connected
check "маршруты и интерфейс не снимались (kill switch)" test "$HELD" = 1
LOG="$(core_log)"
contains "второе поколение ядра" "gen=2" "$LOG"
contains "причина перезапуска в журнале" "код 3 (TUN потерян)" "$LOG"
ns ping -c 1 -W 1 198.51.100.78 >/dev/null 2>&1; sleep 0.5
contains "новое ядро получает пакеты из того же интерфейса" "dst=198.51.100.78" "$(core_log)"
csqtt down >/dev/null

# --- правила сервера ---------------------------------------------------------
echo "== смена сети"
csqtt up ok >/dev/null 2>&1
ns ip route replace default via 10.250.0.3 dev itv1
for i in $(seq 1 60); do
  [ "$(field restarts)" = 1 ] && [ "$(state)" = connected ] && break
  sleep 0.25
done
LOG="$(csqtt logs -n 0)"
contains "смена шлюза замечена" "сеть на itv1 изменилась" "$LOG"
contains "ядро перезапущено после смены сети" "gen=2" "$LOG"
check "после перезапуска снова подключено" test "$(state)" = connected
contains "интерфейс не снимался" "10.99.0.2/32" "$(ns ip -4 addr show dev $IFACE)"
ns ip route replace default via 10.250.0.1 dev itv1
csqtt down >/dev/null

echo "== сервер молчит"
csqtt up silent >/dev/null 2>&1 &
UP_PID=$!
for i in $(seq 1 40); do [ "$(field diagnosis)" != None ] && break; sleep 0.25; done
contains "служба ставит диагноз «сервер молчит»" "сервер csqtt — нет" "$(field diagnosis)"
contains "диагноз называет порт сервера" "UDP-порт 46000" "$(field diagnosis)"
contains "диагноз виден в csqtt status" "Диагноз:" "$(csqtt status)"
csqtt down >/dev/null; wait $UP_PID 2>/dev/null

echo "== постоянное подключение"
csqtt up persistent >/dev/null 2>&1 &
UP_PID=$!
for i in $(seq 1 80); do [ "$(field restarts)" -ge 3 ] 2>/dev/null && break; sleep 0.25; done
sleep 1
check "после трёх неудач подряд сессия не остановлена" test "$(state)" != failed
check "перезапусков не меньше трёх" test "$(field restarts)" -ge 3
csqtt down >/dev/null; wait $UP_PID 2>/dev/null
check "отключение посреди повторов" test "$(state)" = disconnected

echo "== маршрутизация от сервера"
csqtt up routing >/dev/null 2>&1
contains "throw-маршрут для IP из правил сервера" "throw 198.51.100.0/24" "$(ns ip -4 route show table $TABLE)"
contains "исключённая сеть идёт напрямую" "dev itv1" "$(ns ip route get 198.51.100.5)"
contains "остальное по-прежнему в туннель" "dev $IFACE" "$(ns ip route get 1.1.1.1)"
contains "ядру отправлены правила доменов" "stdin: UPDATE_DOMAINS|enabled=1;ru=0;template=0;whitelist=0;domains=example.ru;ip_enabled=1;ip_whitelist=0;ips=198.51.100.0/24" "$(core_log)"
csqtt down >/dev/null

echo "== сервер назначает число потоков"
csqtt up wide >/dev/null 2>&1
sleep 1
check "число потоков от сервера" test "$(field workers_target)" = 40
csqtt down >/dev/null
absent "новая сессия начинает журнал заново" "scenario=wide" "$(csqtt up tunlost >/dev/null 2>&1; core_log)"
csqtt down >/dev/null

echo "== потеря TUN"
csqtt up tunlost >/dev/null 2>&1
sleep 1.5
contains "fd выдан повторно по TUN_LOST" "TUN fd #2 received" "$(core_log)"
check "состояние осталось connected" test "$(state)" = connected

# --- завершение демона -------------------------------------------------------
echo "== SIGTERM демону при поднятом туннеле"
stop_daemon
absent "правила сняты при остановке" "lookup $TABLE" "$(ns ip -4 rule list)"
check "интерфейс удалён" bash -c "! ip netns exec $NS ip link show dev $IFACE"
contains "resolv.conf восстановлен" "nameserver 9.9.9.9" "$(ns cat /etc/resolv.conf)"
check "сокет удалён" test ! -e "$WORK/run/csqttd.sock"

echo "== SIGKILL демона и --cleanup"
start_daemon || exit 1
csqtt up ok >/dev/null 2>&1
kill -KILL "$DAEMON_PID"; wait "$DAEMON_PID" 2>/dev/null; DAEMON_PID=
sleep 0.5
contains "после SIGKILL правила остались (ожидаемо)" "lookup $TABLE" "$(ns ip -4 rule list)"
ns "$BIN/csqttd" --config "$WORK/csqttd.toml" --cleanup
absent "--cleanup снял правила" "lookup $TABLE" "$(ns ip -4 rule list)"
contains "--cleanup вернул resolv.conf" "nameserver 9.9.9.9" "$(ns cat /etc/resolv.conf)"
check "ядро-сирота завершилось само" bash -c "sleep 3; ! pgrep -u $CORE_USER"

echo
echo "итого: пройдено $PASSED, провалено $FAILED"
[ "$FAILED" = 0 ]
