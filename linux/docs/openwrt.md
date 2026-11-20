# csqtt на OpenWrt

Сборка для роутеров с OpenWrt 24.10 и новее, включая 25.x с менеджером пакетов `apk`. Архитектуры: aarch64 (MT7981/MT7986, Raspberry Pi 4/5 и другие ARM64), armv7 (IPQ40xx, MT7623 и другие ARMv7) и x86_64. Для роутеров на MIPS (`mipsel_24kc` и подобные) сборки нет: ядро csqtt под MIPS не собирается.

Туннель становится на роутере сетевым интерфейсом `csqtt0`. Какой трафик в него отправлять, решаешь ты: весь трафик отдельных устройств, всей локальной сети или выбранных сайтов.

## Как это работает

Служба `csqttd` поднимает интерфейс `csqtt0` и больше ничего не трогает: ни маршрутов, ни DNS, ни брандмауэра. Соединения самого ядра csqtt с серверами VK идут через обычный интернет роутера.

```text
устройства сети → правило pbr «через csqtt» → csqtt0 → ядро csqtt → VK TURN → сервер csqtt
                → остальное                 → как раньше
```

## Что нужно на роутере

- `uname -m` выдаёт `aarch64`, `armv7l` или `x86_64`.
- Модуль `kmod-tun` (`/dev/net/tun`): `opkg install kmod-tun` или `apk add kmod-tun`.
- Утилита `ip` из пакета `ip-full`.
- Около 25 МБ свободного места.

## Установка

Скачай архив своей архитектуры `csqtt-openwrt-…-<arch>.tar.gz` из [Releases](https://github.com/pvtarget-glitch/csqtt-clients/releases) и скопируй на роутер:

```sh
scp -O csqtt-openwrt-*-aarch64.tar.gz root@192.168.1.1:/tmp/
```

На роутере:

```sh
cd /tmp
tar xzf csqtt-openwrt-*-aarch64.tar.gz
sh csqtt-openwrt-*-aarch64/install.sh
```

Установщик кладёт файлы в `/usr`, ставит и включает службу `/etc/init.d/csqttd` (procd). Он же добавляет файлы csqtt в `/etc/sysupgrade.conf`, чтобы они пережили обновление прошивки. Существующий `/etc/csqtt/csqttd.toml` не перезаписывается.

Профиль и подключение:

```sh
csqtt import 'csqtt://…' --name vps
csqtt up vps
csqtt status
```

Подключаться при загрузке роутера:

```sh
echo vps > /etc/csqtt/autoconnect
```

Управление службой: `/etc/init.d/csqttd start|stop|restart`. Журнал службы — `logread -e csqttd`, журнал ядра — `csqtt logs -n 0`.

Удаление: `sh uninstall.sh` из распакованного архива. Профили в `/root/.config/csqtt` и конфиг `/etc/csqtt` остаются.

## Интерфейс и брандмауэр

Чтобы OpenWrt знал про `csqtt0`, заведи для него сетевой интерфейс и зону брандмауэра с маскарадингом:

```sh
uci set network.csqtt=interface
uci set network.csqtt.proto='none'
uci set network.csqtt.device='csqtt0'

uci add firewall zone
uci set firewall.@zone[-1].name='csqtt'
uci set firewall.@zone[-1].network='csqtt'
uci set firewall.@zone[-1].input='REJECT'
uci set firewall.@zone[-1].output='ACCEPT'
uci set firewall.@zone[-1].forward='REJECT'
uci set firewall.@zone[-1].masq='1'
uci set firewall.@zone[-1].mtu_fix='1'

uci add firewall forwarding
uci set firewall.@forwarding[-1].src='lan'
uci set firewall.@forwarding[-1].dest='csqtt'

uci commit network
uci commit firewall
/etc/init.d/network reload
/etc/init.d/firewall reload
```

Маршрут по умолчанию через `csqtt0` не ставь: тогда в туннель пошёл бы и трафик самого ядра csqtt, и связь пропала бы.

## Что отправлять в туннель: pbr

Проще всего выбирать трафик пакетом [pbr](https://docs.openwrt.melmac.ca/pbr/) (policy-based routing): `opkg install pbr luci-app-pbr` или `apk add pbr luci-app-pbr`. Правила задаются в LuCI («Службы → Policy Routing») или через uci.

Весь трафик одного устройства:

```sh
uci add pbr policy
uci set pbr.@policy[-1].name='Ноутбук через csqtt'
uci set pbr.@policy[-1].src_addr='192.168.1.50'
uci set pbr.@policy[-1].interface='csqtt'
uci commit pbr
/etc/init.d/pbr restart
```

Вся локальная сеть: `src_addr='192.168.1.0/24'`.

Отдельные сайты: `dest_addr='youtube.com googlevideo.com'` без `src_addr`. Для правил по доменам pbr нужен `dnsmasq-full` (наборы адресов nftset). Подробности — в документации pbr.

DNS: устройства, которые идут через туннель, по-прежнему спрашивают DNS у роутера, а роутер — у провайдера. Если это важно, направь в csqtt и DNS-запросы роутера: например, через `https-dns-proxy` с правилом pbr для его адресов.

## Mihomo или Xray

Если на роутере уже стоит Mihomo (например, OpenClash, nikki) или Xray, `csqtt0` подключается к нему так же, как на Keenetic: прокси типа `direct` с `interface-name: csqtt0` и `ip-version: ipv4`. Пример — в [docs/keenetic.md](keenetic.md#mihomo).

## Ограничения

- Туннель передаёт только IPv4.
- Окна проверки VK на роутере нет. Ядро сначала решает проверку само, а если VK требует ручную, запрос ждёт и по тайм-ауту получает отказ. Поставь в профиле `captcha_mode = "rjs"`, чтобы ядро не ждало ручного решения (`/root/.config/csqtt/profiles.toml`, затем `csqtt down && csqtt up vps`). Если проверки появляются часто, подключись один раз с компьютера, где окно есть, и затем вернись на роутер.
- Профили и токены хранятся в `/root/.config/csqtt` с правами только для root.
