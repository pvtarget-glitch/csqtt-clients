# csqtt на Keenetic с XKeen

Сборка для роутеров Keenetic на ARM64 (aarch64) с Entware, например Giga KN-1012. Туннель csqtt становится на роутере сетевым интерфейсом `csqtt0`, а какой трафик в него отправлять, решает Mihomo или Xray из XKeen.

## Как это работает

Служба `csqttd` поднимает интерфейс `csqtt0` и больше ничего не трогает: ни маршрутов, ни DNS, ни правил iptables. В туннель попадает только трафик, который Mihomo или Xray отправили через этот интерфейс. Всё остальное идёт как раньше, включая соединения самого ядра csqtt с серверами VK.

```text
клиенты сети → XKeen (Mihomo/Xray) ─┬─ правило «csqtt» → csqtt0 → ядро csqtt → VK TURN → сервер csqtt
                                     └─ остальное        → как настроено в XKeen
```

## Что нужно на роутере

- Entware (он уже есть, если стоит XKeen).
- `uname -m` выдаёт `aarch64`.
- Есть `/dev/net/tun`. Если файла нет, установи компонент KeeneticOS с модулем tun — обычно его тянет «Клиент OpenVPN».
- Утилита `ip`: `opkg install ip-full`, если её нет.

## Установка

Архив `csqtt-router-…-aarch64.tar.gz` лежит в разделе [Releases](https://github.com/pvtarget-glitch/csqtt-clients/releases) репозитория. Скопируй его на роутер. SSH в Entware обычно слушает порт 222:

```sh
scp -P 222 csqtt-router-*-aarch64.tar.gz root@192.168.1.1:/opt/tmp/
```

На роутере:

```sh
cd /opt/tmp
tar xzf csqtt-router-*-aarch64.tar.gz
sh csqtt-router-*-aarch64/install.sh
```

Установщик кладёт файлы в `/opt`, ставит службу `/opt/etc/init.d/S99csqttd` и запускает её. Существующий `/opt/etc/csqtt/csqttd.toml` не перезаписывается.

Профиль и подключение:

```sh
csqtt import 'csqtt://…' --name vps
csqtt up vps
csqtt status
```

Подключаться при загрузке роутера:

```sh
echo vps > /opt/etc/csqtt/autoconnect
```

Управление службой: `/opt/etc/init.d/S99csqttd start|stop|restart|status`. Журнал службы — `/opt/var/log/csqttd.log`, журнал ядра — `csqtt logs -n 0`.

## Mihomo

В `/opt/etc/mihomo/config.yaml` добавь прокси, привязанный к интерфейсу:

```yaml
proxies:
  - name: csqtt
    type: direct
    interface-name: csqtt0
    ip-version: ipv4
    udp: true
```

И правила — что отправлять в туннель:

```yaml
rules:
  - DOMAIN-SUFFIX,youtube.com,csqtt
  - DOMAIN-SUFFIX,googlevideo.com,csqtt
  - MATCH,DIRECT
```

`csqtt` можно добавить и в `proxy-groups` рядом с другими прокси: например, в группу `fallback` как запасной канал.

`ip-version: ipv4` обязателен: туннель передаёт только IPv4.

DNS-запросы Mihomo делает по своим настройкам `dns:`, а не через туннель. Если провайдер подменяет ответы, направь DNS через туннель суффиксом с именем прокси:

```yaml
dns:
  nameserver:
    - "https://1.1.1.1/dns-query#csqtt"
```

После правки: `xkeen -restart`.

## Xray

Исходящее соединение в `/opt/etc/xray/configs/04_outbounds.json`:

```json
{
  "tag": "csqtt",
  "protocol": "freedom",
  "settings": { "domainStrategy": "UseIPv4" },
  "streamSettings": { "sockopt": { "interface": "csqtt0" } }
}
```

Правило в `05_routing.json`:

```json
{ "type": "field", "domain": ["geosite:youtube"], "outboundTag": "csqtt" }
```

## Капча VK

На роутере некому показать окно проверки. Ядро сначала решает проверку само. Если VK требует ручную, запрос ждёт и по тайм-ауту получает отказ.

Пока окна для роутера нет, поставь в профиле `captcha_mode = "rjs"`: ядро не будет ждать ручного решения. Профиль правится в `/opt/root/.config/csqtt/profiles.toml`, затем `csqtt down && csqtt up vps`. Если проверки появляются часто, вариант — подключиться один раз с компьютера, где окно есть, и затем вернуться на роутер.

## Если что-то не работает

| Признак | Что проверить |
|---|---|
| `нет /dev/net/tun` при запуске | Компонент KeeneticOS с модулем tun. |
| `csqtt status` — «подключено», но через Mihomo сайты не открываются | `ip addr show csqtt0` — есть ли адрес; `ip-version: ipv4` в прокси; журнал Mihomo. |
| Подключение не доходит до «подключено» | `csqtt logs -n 0`; типичная причина — сервер не отвечает (см. основной README). |
| Потоки рвутся каждые 10–15 минут | На KN-1012 сообщали, что аппаратное ускорение PPE рвёт долгие UDP-сессии. Трафик самого роутера обычно идёт мимо PPE, но если симптом есть — проверь с выключенным аппаратным ускорением. |
| После перезагрузки нет подключения | Есть ли `/opt/etc/csqtt/autoconnect` с именем профиля; `/opt/var/log/csqttd.log`. |

## Что проверено

- Режим «только интерфейс» на Linux: интерфейс поднимается, маршруты, правила и DNS не меняются, UDP и TCP из сокетов, привязанных к `csqtt0`, доходят до ядра, остальной трафик идёт мимо (`sudo tests/interface-mode.sh`).
- Установщик и скрипт службы — на имитации Entware: установка, автоподключение при запуске, остановка со снятием интерфейса.

- Сборка под aarch64 (статические бинарники musl) — в GitHub Actions, вместе с тестом режима.

Не проверено: работа на самом роутере, Mihomo и Xray с этой схемой, скорость и расход памяти. Сколько потоков вытянет MT7981, неизвестно; если упрётся в процессор — уменьши `workers` в профиле.
