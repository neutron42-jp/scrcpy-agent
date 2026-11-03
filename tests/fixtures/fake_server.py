#!/usr/bin/env python3
"""Fake scrcpy-server for tests: speaks the 5.x handshake over localhost."""
import socket
import struct
import sys

port = int(sys.argv[1])
h264_path = sys.argv[2]
control_log = sys.argv[3]

with open(h264_path, "rb") as handle:
    payload = handle.read()

video = socket.create_connection(("127.0.0.1", port))
control = socket.create_connection(("127.0.0.1", port))

video.sendall(b"FakePhone".ljust(64, b"\x00"))
video.sendall(struct.pack(">I", 0x68323634))
video.sendall(bytes([0x80, 0, 0, 0]) + struct.pack(">II", 160, 120))
video.sendall(struct.pack(">Q", (1 << 61) | 1) + struct.pack(">I", len(payload)) + payload)

with open(control_log, "ab") as log:
    while True:
        data = control.recv(4096)
        if not data:
            break
        log.write(data)
        log.flush()
