#!/usr/bin/env python3
"""Minimal dependency-free WebSocket text probe for riz's e2e smoke harness.

Usage:  ws_probe.py <ws://host:port/path> <message>

Opens a WebSocket, performs the RFC 6455 handshake, sends one masked text frame
containing <message>, then prints the payload of the FIRST text frame the server
sends back (skipping ping/pong control frames) and exits 0. Any protocol,
handshake, or connection error prints to stderr and exits non-zero — so a broken
WebSocket handler makes the smoke harness FAIL rather than pass by silent skip.

Stdlib only (socket / base64 / hashlib / os / struct) — no `websockets` pip
dependency — so it runs in CI wherever python3 is present, no websocat needed.
"""

import base64
import hashlib
import os
import socket
import struct
import sys
from urllib.parse import urlparse

GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"  # RFC 6455 magic
TIMEOUT = 15


def fail(msg):
    print(f"ws_probe: {msg}", file=sys.stderr)
    return 1


def run(url, message):
    u = urlparse(url)
    if u.scheme != "ws":
        return fail(f"unsupported scheme {u.scheme!r} (only ws:// is supported)")
    host = u.hostname or "127.0.0.1"
    port = u.port or 80
    path = u.path or "/"
    if u.query:
        path += "?" + u.query

    key = base64.b64encode(os.urandom(16)).decode()
    handshake = (
        f"GET {path} HTTP/1.1\r\n"
        f"Host: {host}:{port}\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key}\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        "\r\n"
    )

    sock = socket.create_connection((host, port), timeout=TIMEOUT)
    sock.settimeout(TIMEOUT)
    sock.sendall(handshake.encode())

    # Read the HTTP 101 handshake response up to the blank line.
    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = sock.recv(4096)
        if not chunk:
            return fail("server closed the connection during the handshake")
        buf += chunk
    header, _, rest = buf.partition(b"\r\n\r\n")
    header_text = header.decode("latin1")
    status_line = header_text.split("\r\n", 1)[0]
    if "101" not in status_line:
        return fail(f"handshake did not upgrade: {status_line!r}")
    expected_accept = base64.b64encode(
        hashlib.sha1((key + GUID).encode()).digest()
    ).decode()
    if expected_accept.lower() not in header_text.lower():
        return fail("Sec-WebSocket-Accept mismatch — not a valid WebSocket peer")

    # Send one masked text frame (client→server frames MUST be masked).
    payload = message.encode("utf-8")
    mask = os.urandom(4)
    masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
    frame = bytearray([0x81])  # FIN + opcode 0x1 (text)
    n = len(payload)
    if n < 126:
        frame.append(0x80 | n)
    elif n < 65536:
        frame.append(0x80 | 126)
        frame += struct.pack(">H", n)
    else:
        frame.append(0x80 | 127)
        frame += struct.pack(">Q", n)
    frame += mask + masked
    sock.sendall(frame)

    # Read frames until the first TEXT frame; carry any bytes already buffered
    # from the handshake read.
    inbuf = bytearray(rest)

    def need(num):
        # extend() mutates in place — `inbuf += chunk` would rebind the name and
        # make it a local in this closure (UnboundLocalError on the read above).
        while len(inbuf) < num:
            chunk = sock.recv(4096)
            if not chunk:
                raise ConnectionError("server closed before sending a text frame")
            inbuf.extend(chunk)

    while True:
        need(2)
        b0, b1 = inbuf[0], inbuf[1]
        opcode = b0 & 0x0F
        is_masked = bool(b1 & 0x80)
        length = b1 & 0x7F
        offset = 2
        if length == 126:
            need(4)
            length = struct.unpack(">H", bytes(inbuf[2:4]))[0]
            offset = 4
        elif length == 127:
            need(10)
            length = struct.unpack(">Q", bytes(inbuf[2:10]))[0]
            offset = 10
        mask_len = 4 if is_masked else 0
        need(offset + mask_len + length)
        mkey = bytes(inbuf[offset : offset + mask_len])
        data = bytes(inbuf[offset + mask_len : offset + mask_len + length])
        if is_masked:
            data = bytes(b ^ mkey[i % 4] for i, b in enumerate(data))
        del inbuf[: offset + mask_len + length]

        if opcode == 0x1:  # text
            sys.stdout.write(data.decode("utf-8", "replace"))
            sys.stdout.flush()
            return 0
        if opcode == 0x8:  # close
            return fail("server closed before sending a text frame")
        # 0x9 ping / 0xA pong / 0x0 continuation / 0x2 binary → keep reading.


def main():
    if len(sys.argv) != 3:
        return fail("usage: ws_probe.py <ws-url> <message>")
    try:
        return run(sys.argv[1], sys.argv[2])
    except (OSError, ConnectionError) as exc:
        return fail(str(exc))


if __name__ == "__main__":
    sys.exit(main())
