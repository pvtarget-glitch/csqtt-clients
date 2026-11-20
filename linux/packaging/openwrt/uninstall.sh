#!/bin/sh
# Удаление csqtt с OpenWrt. Конфиг /etc/csqtt и профили /root/.config/csqtt остаются,
# удалить их: rm -r /etc/csqtt /root/.config/csqtt
[ -x /etc/init.d/csqttd ] && { /etc/init.d/csqttd stop; /etc/init.d/csqttd disable; } || true
rm -f /usr/sbin/csqttd /usr/bin/csqtt /etc/init.d/csqttd
rm -rf /usr/lib/csqtt /usr/share/csqtt
sed -i '\#^/usr/sbin/csqttd$#d; \#^/usr/bin/csqtt$#d; \#^/usr/lib/csqtt/$#d; \#^/usr/share/csqtt/$#d; \#^/etc/init.d/csqttd$#d' /etc/sysupgrade.conf 2>/dev/null || true
echo "csqtt удалён; профили остались в /root/.config/csqtt"
