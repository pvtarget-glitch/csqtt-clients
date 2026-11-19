#!/bin/sh
# Установка csqtt-linux на Keenetic с Entware (aarch64). Запуск из
# распакованного архива: sh install.sh
set -e
cd "$(dirname "$0")"

arch="$(uname -m)"
[ "$arch" = "aarch64" ] || { echo "эта сборка для aarch64, а у роутера $arch"; exit 1; }
[ -d /opt/etc/init.d ] || { echo "не найден Entware (/opt/etc/init.d)"; exit 1; }

[ -x /opt/etc/init.d/S99csqttd ] && /opt/etc/init.d/S99csqttd stop || true

# Только cp и chmod: в прошивке может не быть утилиты install.
put() { cp "$1" "$3" && chmod "$2" "$3"; }
mkdir -p /opt/sbin /opt/bin /opt/lib/csqtt /opt/share/csqtt /opt/etc/csqtt /opt/etc/init.d
put bin/csqttd 755 /opt/sbin/csqttd
put bin/csqtt 755 /opt/bin/csqtt
put bin/csqtt-core 755 /opt/lib/csqtt/csqtt-core
put share/domains.txt 644 /opt/share/csqtt/domains.txt
put S99csqttd 755 /opt/etc/init.d/S99csqttd
if [ -f /opt/etc/csqtt/csqttd.toml ]; then
    put csqttd.toml 644 /opt/etc/csqtt/csqttd.toml.new
    echo "конфиг /opt/etc/csqtt/csqttd.toml сохранён, новый образец — csqttd.toml.new"
else
    put csqttd.toml 644 /opt/etc/csqtt/csqttd.toml
fi

command -v ip >/dev/null || echo "внимание: нет утилиты ip — установи: opkg install ip-full"

/opt/etc/init.d/S99csqttd start
echo
echo "Дальше:"
echo "  csqtt import 'csqtt://…' --name vps"
echo "  csqtt up vps"
echo "  echo vps > /opt/etc/csqtt/autoconnect   # подключаться при загрузке"
