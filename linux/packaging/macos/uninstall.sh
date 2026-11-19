#!/bin/sh
# Удаление csqtt с macOS: sudo /usr/local/share/csqtt/uninstall.sh
# Профили пользователей (~/.config/csqtt) не удаляются.
set -e
[ "$(id -u)" = 0 ] || { echo "запусти через sudo"; exit 1; }
launchctl bootout system /Library/LaunchDaemons/io.github.csqtt.daemon.plist 2>/dev/null || true
rm -f /Library/LaunchDaemons/io.github.csqtt.daemon.plist
rm -rf /Applications/csqtt.app /usr/local/lib/csqtt /usr/local/etc/csqtt /var/run/csqtt
rm -f /usr/local/sbin/csqttd /usr/local/bin/csqtt /var/log/csqttd.log
pkgutil --forget io.github.csqtt >/dev/null 2>&1 || true
rm -rf /usr/local/share/csqtt
echo "csqtt удалён"
