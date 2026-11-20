#!/usr/bin/env bash
# Окно csqtt после установки запускается и не падает в первые секунды.
set -uo pipefail
GUI="/c/Program Files/csqtt/csqtt-gui.exe"
[ -x "$GUI" ] || { echo "нет $GUI"; exit 1; }
"$GUI" > /dev/null 2>&1 &
sleep 10
if tasklist //FI "IMAGENAME eq csqtt-gui.exe" | grep -qi csqtt-gui.exe; then
  echo "окно работает"
  taskkill //F //IM csqtt-gui.exe > /dev/null 2>&1
  exit 0
fi
echo "окно завершилось сразу после запуска"
exit 1
