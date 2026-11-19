#!/usr/bin/env bash
# Проверка `csqtt server install|remove` против настоящего sshd на 127.0.0.1
# с имитатором deploy.sh: вход по паролю и по ключу, sudo с паролем,
# загрузка файлов, ошибки входа, установщика и смены ключа сервера.
#
#   sudo tests/deploy-test.sh   (нужны openssh-server и openssh-client)
set -uo pipefail

ROOT="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/.." && pwd)"
BIN="${CSQTT_BIN_DIR:-$ROOT/target/debug}"
WORK=/tmp/csqtt-deploy-it
PORT=2299
VPS_USER=csqtt-dt-vps
CLI_USER=csqtt-dt-cli
VPS_PASS="Pa ss'w\"0rd\$x"

pass=0
fail=0
ok() { echo "  ok   $1"; pass=$((pass + 1)); }
bad() { echo "  FAIL $1"; [ -n "${2:-}" ] && echo "$2" | sed 's/^/       /'; fail=$((fail + 1)); }
contains() { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "нет «$2» в: $3" ;; esac; }
absent() { case "$3" in *"$2"*) bad "$1" "есть «$2» в: $3" ;; *) ok "$1" ;; esac; }

cleanup() {
  [ -f "$WORK/sshd.pid" ] && kill "$(cat "$WORK/sshd.pid")" 2>/dev/null
  for user in "$VPS_USER" "$CLI_USER"; do id "$user" >/dev/null 2>&1 && userdel "$user" 2>/dev/null; done
  rm -f /etc/sudoers.d/csqtt-dt /tmp/csqtt-dt.* /tmp/deploy.sh /tmp/.csqtt-upload-*
  rm -rf "$WORK"
  return 0
}
trap cleanup EXIT
cleanup

command -v sshd >/dev/null || [ -x /usr/sbin/sshd ] || { echo "нужен openssh-server"; exit 2; }
mkdir -p "$WORK/assets" "$WORK/cli-home" /run/sshd
chmod 755 "$WORK"

# --- «VPS»: пользователь с паролем и sudo по паролю -----------------------------
useradd --create-home --home-dir "$WORK/vps-home" --shell /bin/bash "$VPS_USER"
printf '%s:%s\n' "$VPS_USER" "$VPS_PASS" | chpasswd
printf '%s ALL=(ALL) ALL\n' "$VPS_USER" > /etc/sudoers.d/csqtt-dt
chmod 440 /etc/sudoers.d/csqtt-dt
useradd --home-dir "$WORK/cli-home" --shell /bin/bash "$CLI_USER"
chown "$CLI_USER" "$WORK/cli-home"

ssh-keygen -q -t ed25519 -N '' -f "$WORK/host_key"
cat > "$WORK/sshd_config" <<EOF
Port $PORT
ListenAddress 127.0.0.1
HostKey $WORK/host_key
PidFile $WORK/sshd.pid
UsePAM no
PasswordAuthentication yes
KbdInteractiveAuthentication no
PubkeyAuthentication yes
AuthorizedKeysFile .ssh/authorized_keys
PermitRootLogin no
EOF
/usr/sbin/sshd -f "$WORK/sshd_config" -E "$WORK/sshd.log"
sleep 0.5

# --- поставка: имитатор deploy.sh и «бинарники» -----------------------------------
cat > "$WORK/assets/deploy.sh" <<'EOF'
#!/bin/bash
echo "CSQTT_PROGRESS|0.10|Пакеты..."
if [ "$(id -u)" != 0 ]; then echo "CSQTT_DEPLOY_ERROR|preflight|не root"; exit 20; fi
case "$1" in
  install)
    if [ -e /tmp/csqtt-dt.fail ]; then
      echo "CSQTT_DEPLOY_ERROR|preflight|UDP-порт $CSQTT_PEER_PORT занят"; exit 20
    fi
    {
      echo "binary=$(cat /tmp/.csqtt-upload-server)"
      echo "env peer=$CSQTT_PEER_PORT web=$CSQTT_WEB_PORT ssh=$CSQTT_SSH_PORT mode=$CSQTT_DEPLOY_MODE ip=$CSQTT_PUBLIC_IP secret=$CSQTT_WEB_SECRET_PATH"
      cat /tmp/.csqtt-upload-web.env /tmp/.csqtt-upload-overrides.json
      echo
      stat -c 'mode=%a' /tmp/.csqtt-upload-overrides.json
    } > /tmp/csqtt-dt.result
    echo "CSQTT_PROGRESS|0.60|Бинарник..."
    printf '\033[0;32m[✓]\033[0m служба запущена\n'
    echo CSQTT_DEPLOY_OK
    ;;
  uninstall)
    echo "peer=$CSQTT_PEER_PORT" > /tmp/csqtt-dt.removed
    echo "[✓] удалено"
    ;;
esac
EOF
for arch in amd64 arm64 armv7; do echo "FAKE-$arch" > "$WORK/assets/csqtt-linux-$arch"; done
chmod -R a+rX "$WORK/assets"

install -m 755 "$BIN/csqtt" "$WORK/csqtt"
run_csqtt() { # run_csqtt [env…] -- аргументы
  local envs=()
  while [ "$1" != "--" ]; do envs+=("$1"); shift; done
  shift
  setpriv --reuid "$CLI_USER" --regid "$CLI_USER" --init-groups \
    env -i PATH=/usr/bin:/bin HOME="$WORK/cli-home" CSQTT_CONFIG_DIR="$WORK/cli-home/config" \
    CSQTT_SERVER_ASSETS="$WORK/assets" "${envs[@]}" "$WORK/csqtt" "$@" 2>&1 < /dev/null
}

echo "== установка: пароль SSH, sudo с паролем"
OUT="$(run_csqtt CSQTT_SSH_PASSWORD="$VPS_PASS" -- server install 127.0.0.1 -p $PORT -u $VPS_USER --name vps)"
contains "установка завершена" "Сервер установлен (x86_64)" "$OUT"
contains "прогресс установщика показан" "Бинарник..." "$OUT"
contains "вывод установщика без цветовых кодов" "       [✓] служба запущена" "$OUT"
contains "адрес панели" "Панель:  https://127.0.0.1:46002/" "$OUT"
RESULT="$(cat /tmp/csqtt-dt.result 2>/dev/null)"
contains "загружен бинарник под архитектуру" "binary=FAKE-amd64" "$RESULT"
contains "переменные установщика" "env peer=46000 web=46002 ssh=$PORT mode=systemd ip=127.0.0.1 secret=" "$RESULT"
contains "файл панели в формате systemd" 'CSQTT_WEB_USER="admin"' "$RESULT"
contains "пароль сервера передан файлом" '{"main_password":"' "$RESULT"
contains "файлы с секретами закрыты" "mode=600" "$RESULT"
PROFILE="$(cat "$WORK/cli-home/config/profiles.toml" 2>/dev/null)"
contains "профиль сохранён" 'name = "vps"' "$PROFILE"
contains "профиль на порт сервера" "port = 46000" "$PROFILE"
MAIN="$(sed -n 's/.*"main_password":"\([^"]*\)".*/\1/p' /tmp/csqtt-dt.result)"
contains "пароль профиля совпадает с паролем сервера" "password = \"$MAIN\"" "$PROFILE"
contains "ключ сервера запомнен (accept-new)" "ssh-ed25519" "$(ssh-keygen -F "[127.0.0.1]:$PORT" -f "$WORK/cli-home/.ssh/known_hosts" 2>/dev/null)"
absent "пароль SSH не попал в командную строку sshd" "$VPS_PASS" "$(cat "$WORK/sshd.log")"

echo "== ошибки"
OUT="$(run_csqtt CSQTT_SSH_PASSWORD="wrong" -- server install 127.0.0.1 -p $PORT -u $VPS_USER)"
contains "неверный пароль" "не принял логин или пароль SSH" "$OUT"
OUT="$(run_csqtt CSQTT_SSH_PASSWORD="$VPS_PASS" -- server install 127.0.0.1 -p 2298 -u $VPS_USER)"
contains "закрытый порт" "не принимает подключения" "$OUT"
touch /tmp/csqtt-dt.fail
OUT="$(run_csqtt CSQTT_SSH_PASSWORD="$VPS_PASS" -- server install 127.0.0.1 -p $PORT -u $VPS_USER)"
contains "ошибка установщика с причиной" "Проверка перед установкой не пройдена; работающий сервер не останавливался. Причина: UDP-порт 46000 занят" "$OUT"
rm -f /tmp/csqtt-dt.fail
OUT="$(run_csqtt CSQTT_SSH_PASSWORD="$VPS_PASS" CSQTT_SUDO_PASSWORD="nope" -- server install 127.0.0.1 -p $PORT -u $VPS_USER)"
contains "неверный пароль sudo" "sudo не принял пароль" "$OUT"
OUT="$(run_csqtt CSQTT_ASKPASS_SECRET=secret -- "Are you sure you want to continue connecting (yes/no)?")"
[ -z "$OUT" ] && ok "askpass не отвечает паролем на чужие вопросы" || bad "askpass не отвечает паролем на чужие вопросы" "$OUT"
contains "askpass отвечает на запрос пароля" "secret" "$(run_csqtt CSQTT_ASKPASS_SECRET=secret -- "user@host's password:")"

echo "== вход по ключу"
mkdir -p "$WORK/vps-home/.ssh"
setpriv --reuid "$CLI_USER" --regid "$CLI_USER" --init-groups ssh-keygen -q -t ed25519 -N 'key pass' -f "$WORK/cli-home/id_test"
cat "$WORK/cli-home/id_test.pub" > "$WORK/vps-home/.ssh/authorized_keys"
chown -R "$VPS_USER" "$WORK/vps-home/.ssh"; chmod 700 "$WORK/vps-home/.ssh"; chmod 600 "$WORK/vps-home/.ssh/authorized_keys"
rm -f /tmp/csqtt-dt.result
OUT="$(run_csqtt CSQTT_SSH_KEY_PASSPHRASE="key pass" CSQTT_SUDO_PASSWORD="$VPS_PASS" -- server install 127.0.0.1 -p $PORT -u $VPS_USER -i "$WORK/cli-home/id_test" --docker --name vps-key)"
contains "установка по ключу с паролем ключа" "Сервер установлен" "$OUT"
contains "режим Docker" "mode=docker" "$(cat /tmp/csqtt-dt.result 2>/dev/null)"

echo "== удаление"
OUT="$(run_csqtt CSQTT_SSH_PASSWORD="$VPS_PASS" -- server remove 127.0.0.1 -p $PORT -u $VPS_USER --peer-port 47000)"
contains "удаление" "Сервер csqtt удалён" "$OUT"
contains "удаление с нужным портом" "peer=47000" "$(cat /tmp/csqtt-dt.removed 2>/dev/null)"

echo "== ключ сервера сменился"
kill "$(cat "$WORK/sshd.pid")"; sleep 0.3
rm -f "$WORK/host_key" "$WORK/host_key.pub"
ssh-keygen -q -t ed25519 -N '' -f "$WORK/host_key"
/usr/sbin/sshd -f "$WORK/sshd_config" -E "$WORK/sshd.log"
sleep 0.5
OUT="$(run_csqtt CSQTT_SSH_PASSWORD="$VPS_PASS" -- server install 127.0.0.1 -p $PORT -u $VPS_USER)"
contains "подмена ключа сервера обнаружена" "ssh-keygen -R '[127.0.0.1]:$PORT'" "$OUT"

echo
echo "итого: пройдено $pass, провалено $fail"
[ "$fail" -eq 0 ]
