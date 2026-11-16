"""Persistent TCP and sequenced UDP echo traffic for isolated namespace tests."""
import argparse
import json
from pathlib import Path
import socket
import threading
import time


def serve(address):
    def udp():
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as listener:
            listener.bind((address, 18000))
            while True:
                packet, peer = listener.recvfrom(4096)
                listener.sendto(packet, peer)
    threading.Thread(target=udp, daemon=True).start()
    with socket.socket() as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind((address, 18001))
        listener.listen()
        while True:
            connection, _ = listener.accept()
            def echo(connection):
                with connection:
                    while packet := connection.recv(4096):
                        connection.sendall(packet)
            threading.Thread(target=echo, args=(connection,), daemon=True).start()


def collect(peer, protocol, output):
    kind = socket.SOCK_STREAM if protocol == 'tcp' else socket.SOCK_DGRAM
    with socket.socket(socket.AF_INET, kind) as connection, Path(output).open('w', buffering=1) as log:
        connection.settimeout(0.2)
        connection.connect((peer, 18001 if protocol == 'tcp' else 18000))
        if protocol == 'tcp':
            connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        def receive():
            pending = b''
            while True:
                try:
                    packet = connection.recv(4096)
                    if not packet:
                        return
                    pending += packet
                    while b'\n' in pending:
                        line, pending = pending.split(b'\n', 1)
                        log.write(json.dumps({'sequence': int(line), 'received': time.monotonic()}) + '\n')
                except socket.timeout:
                    continue
                except OSError:
                    return
        threading.Thread(target=receive, daemon=True).start()
        sequence = 0
        while True:
            try:
                connection.sendall(f'{sequence}\n'.encode())
            except socket.timeout:
                pass
            except OSError:
                return
            sequence += 1
            time.sleep(0.05)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('mode', choices=['serve', 'tcp', 'udp'])
    parser.add_argument('address')
    parser.add_argument('--output')
    arguments = parser.parse_args()
    if arguments.mode == 'serve':
        serve(arguments.address)
    else:
        collect(arguments.address, arguments.mode, arguments.output)
