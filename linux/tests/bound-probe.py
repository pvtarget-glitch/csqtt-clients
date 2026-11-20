"""Соединение через заданный физический интерфейс мимо маршрутов:
IP_UNICAST_IF на Windows, IP_BOUND_IF на macOS.

    python bound-probe.py ИНДЕКС tcp АДРЕС   -> open | blocked
    python bound-probe.py ИНДЕКС dns АДРЕС   -> open | blocked
"""
import socket
import struct
import sys

index, kind, address = int(sys.argv[1]), sys.argv[2], sys.argv[3]
if sys.platform == "win32":
    OPTION, VALUE = 31, struct.pack("!I", index)  # IP_UNICAST_IF, порядок сети
else:
    OPTION, VALUE = 25, struct.pack("@I", index)  # IP_BOUND_IF
QUERY = (b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00"
         b"\x07example\x03com\x00\x00\x01\x00\x01")
try:
    if kind == "tcp":
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.settimeout(5)
        sock.setsockopt(socket.IPPROTO_IP, OPTION, VALUE)
        sock.connect((address, 443))
    else:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.settimeout(5)
        sock.setsockopt(socket.IPPROTO_IP, OPTION, VALUE)
        sock.sendto(QUERY, (address, 53))
        sock.recvfrom(512)
    print("open")
except OSError as error:
    print("blocked", error)
