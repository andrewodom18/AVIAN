"""Persistent TCP and sequenced UDP traffic for distribution checks."""

import argparse
import json
import socket
import threading
import time


def echo_connection(connection):
    with connection:
        try:
            while payload := connection.recv(1024):
                connection.sendall(payload)
        except OSError:
            return


def serve(control):
    def tcp_server():
        with socket.socket() as listener:
            if control:
                listener.setsockopt(socket.IPPROTO_IP, socket.IP_TOS, 0xa0)
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(('0.0.0.0', 9000))
            listener.listen()
            while True:
                connection, _ = listener.accept()
                threading.Thread(target=echo_connection, args=(connection,), daemon=True).start()

    threading.Thread(target=tcp_server, daemon=True).start()
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver:
        if control:
            receiver.setsockopt(socket.IPPROTO_IP, socket.IP_TOS, 0xa0)
        receiver.bind(('0.0.0.0', 9001))
        while True:
            payload, address = receiver.recvfrom(1024)
            receiver.sendto(payload, address)


def sample(destination, output, control):
    connection = socket.socket()
    if control:
        connection.setsockopt(socket.IPPROTO_IP, socket.IP_TOS, 0xa0)
    connection.settimeout(3)
    connection.connect((destination, 9000))
    connection.settimeout(0.15)
    datagram = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    if control:
        datagram.setsockopt(socket.IPPROTO_IP, socket.IP_TOS, 0xa0)
    datagram.settimeout(0.15)
    with connection, datagram, open(output, 'w', buffering=1) as observations:
        sequence = 0
        while True:
            payload = str(sequence).encode()
            observation = {'at': time.monotonic(), 'tcp': False, 'udp': False}
            try:
                connection.sendall(payload)
                observation['tcp'] = connection.recv(1024) == payload
            except OSError:
                pass
            try:
                datagram.sendto(payload, (destination, 9001))
                observation['udp'] = datagram.recvfrom(1024)[0] == payload
            except OSError:
                pass
            observations.write(json.dumps(observation) + '\n')
            sequence += 1
            time.sleep(0.05)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('mode', choices=['serve', 'sample'])
    parser.add_argument('--destination')
    parser.add_argument('--output')
    parser.add_argument('--control', action='store_true')
    arguments = parser.parse_args()
    if arguments.mode == 'serve':
        serve(arguments.control)
    else:
        sample(arguments.destination, arguments.output, arguments.control)
