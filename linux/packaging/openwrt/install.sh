#!/bin/sh
# Установка csqtt на роутер с OpenWrt (24.10 с opkg и 25.x с apk — пакетный
# менеджер не нужен). Запуск из распакованного архива: sh install.sh
set -e
cd "$(dirname "$0")"

want="$(cat ARCH)"
arch="$(uname -m)"
case "$want:$arch" in
    aarch64:aarch64 | x86_64:x86_64 | armv7:armv7*) ;;
    *) echo "эта сборка для $want, а у роутера $arch"; exit 1 ;;
esac
[ -f /etc/openwrt_release ] || { echo "это не OpenWrt (нет /etc/openwrt_release)"; exit 1; }

[ -x /etc/init.d/csqttd ] && /etc/init.d/csqttd stop || true

put() { cp "$1" "$3" && chmod "$2" "$3"; }
mkdir -p /usr/sbin /usr/bin /usr/lib/csqtt /usr/share/csqtt /etc/csqtt /etc/init.d
put bin/csqttd 755 /usr/sbin/csqttd
put bin/csqtt 755 /usr/bin/csqtt
put bin/csqtt-core 755 /usr/lib/csqtt/csqtt-core
put share/domains.txt 644 /usr/share/csqtt/domains.txt
put csqttd.init 755 /etc/init.d/csqttd
if [ -f /etc/csqtt/csqttd.toml ]; then
    put csqttd.toml 644 /etc/csqtt/csqttd.toml.new
    echo "конфиг /etc/csqtt/csqttd.toml сохранён, новый образец — csqttd.toml.new"
else
    put csqttd.toml 644 /etc/csqtt/csqttd.toml
fi
# Файлы переживают обновление прошивки (sysupgrade).
for file in /etc/csqtt/ /root/.config/csqtt/ /usr/sbin/csqttd /usr/bin/csqtt /usr/lib/csqtt/ /usr/share/csqtt/ /etc/init.d/csqttd; do
    grep -qxF "$file" /etc/sysupgrade.conf 2>/dev/null || echo "$file" >> /etc/sysupgrade.conf
done

[ -c /dev/net/tun ] || echo "внимание: нет /dev/net/tun — установи kmod-tun (opkg install kmod-tun или apk add kmod-tun)"
command -v ip >/dev/null || echo "внимание: нет утилиты ip — установи ip-full"

/etc/init.d/csqttd enable
/etc/init.d/csqttd start
echo
echo "Дальше:"
echo "  csqtt import 'csqtt://…' --name vps"
echo "  csqtt up vps"
echo "  echo vps > /etc/csqtt/autoconnect   # подключаться при загрузке"
echo "Как направить трафик в туннель — README.md в этом архиве."
