"""Соединение через заданный физический интерфейс мимо маршрутов (IP_UNICAST_IF).

    python windows-probe.py ИНДЕКС tcp АДРЕС   -> open | blocked
    python windows-probe.py ИНДЕКС dns АДРЕС   -> open | blocked
"""
import socket
import struct
import sys

IP_UNICAST_IF = 31
index, kind, address = int(sys.argv[1]), sys.argv[2], sys.argv[3]
QUERY = (b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00"
         b"\x07example\x03com\x00\x00\x01\x00\x01")
try:
    if kind == "tcp":
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.settimeout(5)
        sock.setsockopt(socket.IPPROTO_IP, IP_UNICAST_IF, struct.pack("!I", index))
        sock.connect((address, 443))
    else:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.settimeout(5)
        sock.setsockopt(socket.IPPROTO_IP, IP_UNICAST_IF, struct.pack("!I", index))
        sock.sendto(QUERY, (address, 53))
        sock.recvfrom(512)
    print("open")
except OSError as error:
    print("blocked", error)
