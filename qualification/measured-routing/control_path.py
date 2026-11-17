"""Unidirectional CS5 traffic for observing a single forwarding direction."""
import argparse
import json
import os
from pathlib import Path
import socket
import time


def publish(path, value):
    destination = Path(path)
    temporary = destination.with_suffix('.tmp')
    temporary.write_text(json.dumps(value))
    temporary.replace(destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['serve', 'send'])
    parser.add_argument('address')
    parser.add_argument('--output', required=True)
    parser.add_argument('--duration', type=float, default=2)
    arguments = parser.parse_args()
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as connection:
        if arguments.mode == 'serve':
            connection.bind((arguments.address, 19002))
            connection.settimeout(0.1)
            received = 0
            while True:
                try:
                    received += len(connection.recv(65535))
                except socket.timeout:
                    pass
                publish(arguments.output, {'received_payload_bytes': received})
        else:
            connection.setsockopt(socket.IPPROTO_IP, socket.IP_TOS, 0xa0)
            connection.connect((arguments.address, 19002))
            payload = os.urandom(1100)
            started = time.monotonic()
            deadline = started + arguments.duration
            sent = 0
            next_send = started
            while time.monotonic() < deadline:
                connection.send(payload)
                sent += len(payload)
                next_send += len(payload) * 8 / 1_000_000
                time.sleep(max(0, next_send - time.monotonic()))
            publish(arguments.output, {'sent_bytes': sent, 'completed': True,
                                       'duration_seconds': time.monotonic() - started})
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
