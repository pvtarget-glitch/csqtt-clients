#!/usr/bin/env python3
"""Имитатор ядра csqtt для интеграционных тестов демона.

Говорит с хостом тем же протоколом, что и настоящий rust-client:
события в stdout, журнал в stderr, команды из stdin, TUN-fd через
абстрактный UNIX-сокет (Linux) или пакеты по UDP от службы (macOS:
--listen и --listen-peer). Сценарий выбирается «паролем» (CSQTT_PASSWORD):
ok, captcha, fatal, crash, restart, routing, tunlost, wide (сервер назначает
40 потоков, часть из них не поднимается), deadcalls (все звонки сразу
завершены).

Одноразовый режим Auto API (--vk-api=start|finish) отвечает строкой JSON,
как настоящее ядро, и пишет вызовы в vk-api.log в рабочем каталоге. Токен
vk1.invalid — «токен недействителен», vk1.empty — VK не создаёт звонков,
vk1.slow — звонки создаются 3 секунды.
"""
import array
import base64
import json
import os
import socket
import sys
import threading
import time

ARGS = dict(a[2:].split("=", 1) if "=" in a else (a[2:], "1") for a in sys.argv[1:] if a.startswith("--"))
SCENARIO = os.environ.get("CSQTT_PASSWORD", "ok")
GENERATION = int(ARGS.get("gen", "1"))
out_lock = threading.Lock()
received = []
captcha_results = []


def event(kind, payload=None):
    with out_lock:
        sys.stdout.write("__CSQTT_EVENT__|%s|%s\n" % (kind, json.dumps(payload or {})))
        sys.stdout.flush()


def raw(line):
    with out_lock:
        sys.stdout.write(line + "\n")
        sys.stdout.flush()


def log(message):
    sys.stderr.write("[FAKE] %s\n" % message)
    sys.stderr.flush()


def tun_reader(fd, index):
    while True:
        try:
            packet = os.read(fd, 4096)
        except BlockingIOError:
            time.sleep(0.05)
            continue
        except OSError:
            return
        if packet and packet[0] >> 4 == 4:
            log("TUN#%d packet %d bytes dst=%s src=%s" % (index, len(packet), socket.inet_ntoa(packet[16:20]), socket.inet_ntoa(packet[12:16])))


def fd_listener(name):
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind("\0" + name)
    listener.listen(8)
    while True:
        connection, _ = listener.accept()
        fds = array.array("i")
        _data, ancillary, _flags, _addr = connection.recvmsg(1, socket.CMSG_SPACE(4 * fds.itemsize))
        for level, kind, data in ancillary:
            if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                fds.frombytes(data[: len(data) - (len(data) % fds.itemsize)])
        if not fds:
            connection.close()
            continue
        connection.send(b"\x01")
        connection.close()
        received.append(fds[0])
        log("TUN fd #%d received" % len(received))
        threading.Thread(target=tun_reader, args=(fds[0], len(received)), daemon=True).start()
        if SCENARIO == "tunlost" and len(received) == 1:
            time.sleep(0.3)
            event("TUN_LOST")


def udp_relay(port, peer):
    """macOS: служба пересылает пакеты utun в UDP-порт ядра (--listen)."""
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", port))
    host, peer_port = peer.rsplit(":", 1)
    expected = (host, int(peer_port))
    log("UDP relay on %d, peer %s" % (port, peer))
    while True:
        packet, source = sock.recvfrom(65535)
        if source != expected:
            log("UDP from stranger %s:%d dropped" % source)
            continue
        if not received:
            received.append(-1)
            log("relay first packet")
        if packet and packet[0] >> 4 == 4:
            log("UDP packet %d bytes dst=%s src=%s" % (len(packet), socket.inet_ntoa(packet[16:20]), socket.inet_ntoa(packet[12:16])))
            if packet[9] == 1 and packet[20] == 8:
                # ICMP echo request → ответ, чтобы проверить обратный путь.
                reply = bytearray(packet)
                reply[12:16], reply[16:20] = packet[16:20], packet[12:16]
                reply[20] = 0
                reply[22:24] = b"\0\0"
                checksum = 0
                icmp = bytes(reply[20:])
                if len(icmp) % 2:
                    icmp += b"\0"
                for i in range(0, len(icmp), 2):
                    checksum += (icmp[i] << 8) + icmp[i + 1]
                checksum = (checksum >> 16) + (checksum & 0xFFFF)
                checksum = ~(checksum + (checksum >> 16)) & 0xFFFF
                reply[22:24] = checksum.to_bytes(2, "big")
                sock.sendto(bytes(reply), expected)


def stdin_loop():
    for line in sys.stdin:
        line = line.strip()
        if line == "STOP":
            log("STOP received")
            event("STOPPED")
            os._exit(0)
        elif line.startswith("CAPTCHA_RESULT|"):
            captcha_results.append(line.split("|", 1)[1])
            log("captcha result: %s" % captcha_results[-1])
        elif line:
            log("stdin: %s" % line)
    log("stdin closed")
    os._exit(0)


def wait_captcha(count, timeout=15):
    deadline = time.time() + timeout
    while len(captcha_results) < count and time.time() < deadline:
        time.sleep(0.05)
    return len(captcha_results) >= count


def vk_api():
    token = os.environ.get("CSQTT_VK_TOKEN", "")
    action = ARGS["vk-api"]
    uid = os.getuid() if hasattr(os, "getuid") else 0
    with open("vk-api.log", "a") as record:
        record.write("%s count=%s calls=%s token=%s uid=%d bind=%s argv_token=%s\n" % (
            action, ARGS.get("vk-api-count", ""), ARGS.get("vk-api-calls", ""),
            "yes" if token else "no", uid, ARGS.get("bind-interface", "-"),
            "yes" if token and token in " ".join(sys.argv) else "no"))
    if action == "finish":
        calls = [c for c in ARGS.get("vk-api-calls", "").split(",") if c]
        print(json.dumps({"finished": len(calls), "total": len(calls)}))
        return
    if token == "vk1.invalid":
        print(json.dumps({"calls": [], "requested": 1, "token_invalid": True, "error": "код 5: User authorization failed"}))
        return
    count = int(ARGS.get("vk-api-count", "1"))
    if token == "vk1.slow":
        time.sleep(3)
    if token == "vk1.empty":
        print(json.dumps({"calls": [], "requested": count, "token_invalid": False, "error": "код 9: Flood control"}))
        return
    stamp = "%06d" % (int(time.time() * 1000) % 1000000)
    calls = [{"id": "call%s%d" % (stamp, i), "hash": "apihash%s%d" % (stamp, i)} for i in range(1, count + 1)]
    print(json.dumps({"calls": calls, "requested": count, "token_invalid": False, "error": None}))


def main():
    if "vk-api" in ARGS:
        vk_api()
        return
    event("PROCESS", {"pid": os.getpid()})
    log("uid=%d gen=%d scenario=%s" % (os.getuid() if hasattr(os, "getuid") else 0, GENERATION, SCENARIO))
    log("argv: %s" % " ".join(sys.argv[1:]))
    log("env keys: %s" % ",".join(sorted(os.environ)))
    if "tun-uds" in ARGS:
        threading.Thread(target=fd_listener, args=(ARGS["tun-uds"],), daemon=True).start()
    else:
        port = int(ARGS["listen"].rsplit(":", 1)[1])
        threading.Thread(target=udp_relay, args=(port, ARGS["listen-peer"]), daemon=True).start()
    threading.Thread(target=stdin_loop, daemon=True).start()
    time.sleep(0.3)

    if SCENARIO == "fatal":
        event("ERROR", {"code": "authentication_rejected", "message": "FATAL_AUTH: invalid password", "fatal": True})
        time.sleep(0.2)
        sys.exit(1)
    if SCENARIO == "silent":
        # TURN работает, сервер не отвечает: как в журнале ядра при
        # недоступном или заблокированном сервере.
        for index in range(3):
            log("[TURN] Аллокация активна: relay 155.212.200.%d:40000" % (10 + index))
        for index in range(8):
            time.sleep(0.2)
            log("[СЕССИЯ #%d] DTLS: таймаут рукопожатия за 8000 мс (состояние ждём полёт сервера, входящих 0, повторов 16)" % (index + 1))
        while True:
            time.sleep(1)
    if SCENARIO == "crash":
        log("simulated crash")
        sys.exit(7)
    if SCENARIO == "captcha":
        raw("CAPTCHA_SOLVE|auto|https://id.vk.ru/not_robot_captcha?session_token=abc|abc")
        wait_captcha(1)
        raw("CAPTCHA_SOLVE|manual|https://evil.example/not_robot_captcha?session_token=abc|abc")
        wait_captcha(2)
        raw("CAPTCHA_SOLVE|manual|https://id.vk.ru/not_robot_captcha?session_token=abc|abc")
        if not wait_captcha(3):
            log("captcha timed out")

    extra = "workers=10"
    if SCENARIO == "routing":
        rule = {"mode": "blacklist", "domains": ["example.ru"], "ip_mode": "blacklist", "ips": ["198.51.100.0/24"]}
        extra = base64.b64encode(json.dumps(rule).encode()).decode()
    event("CONFIG", {"config": "TUNCONF:10.99.0.2:1.1.1.1,8.8.8.8:0:stream-v1:%s" % extra})
    event("READY", {"worker": 1})
    if SCENARIO == "wide":
        event("WORKERS", {"workers": 40})
    if SCENARIO == "deadcalls" and GENERATION == 1:
        time.sleep(0.5)
        for hash in [h for h in ARGS.get("vk", "").split(",") if h]:
            event("CALL_UNAVAILABLE", {"hash": hash, "code": 9000})

    started = time.time()
    ticks = 0
    streams_sent = False
    while True:
        time.sleep(0.5)
        ticks += 1
        active = 10 if received else 0
        if SCENARIO == "wide" and received:
            active = min(28, 10 + ticks * 3)
        event("STATS", {"active": active, "bytes_up": 150000 * ticks, "bytes_down": 2400000 * ticks})
        if SCENARIO == "ok" and received and not streams_sent:
            # Новое ядро сообщает потоки: два узла VK, один поток падает.
            streams_sent = True
            for worker in range(1, 5):
                server = "155.212.0.1:3478" if worker <= 3 else "95.142.0.2:443"
                event("STREAM_UP", {"worker": worker, "server": server})
            event("STREAM_DOWN", {"worker": 2, "seconds": 1})
            event("STREAMS", {"streams": [
                {"worker": 1, "server": "155.212.0.1:3478", "seconds": 0},
                {"worker": 3, "server": "155.212.0.1:3478", "seconds": 0},
                {"worker": 4, "server": "95.142.0.2:443", "seconds": 0},
                {"worker": 5, "server": "95.142.0.2:443", "seconds": 120, "bytes_up": 3145728, "bytes_down": 52428800},
            ]})
        if SCENARIO == "restart" and GENERATION == 1 and received and time.time() - started > 1.5:
            log("simulated TUN loss exit")
            sys.exit(3)


if __name__ == "__main__":
    main()
