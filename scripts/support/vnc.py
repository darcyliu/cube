"""Minimal RFB transport shared by display checks."""
import socket
import struct


def read(sock, size):
    result = b""
    while len(result) < size:
        data = sock.recv(size - len(result))
        if not data:
            raise EOFError("VNC disconnected")
        result += data
    return result


def connect(host, port):
    sock = socket.create_connection((host, port), timeout=10)
    assert read(sock, 12) == b"RFB 003.008\n"
    sock.sendall(b"RFB 003.008\n")
    assert read(sock, 2) == b"\x01\x01"
    sock.sendall(b"\x01")
    assert read(sock, 4) == bytes(4)
    sock.sendall(b"\x01")
    init = read(sock, 24)
    width, height = struct.unpack_from("!HH", init)
    read(sock, struct.unpack_from("!I", init, 20)[0])
    return sock, (width, height)


def key(sock, symbol, down):
    sock.sendall(struct.pack("!BBHI", 4, down, 0, symbol))


def pointer(sock, mask, x, y):
    sock.sendall(struct.pack("!BBHH", 5, mask, x, y))


