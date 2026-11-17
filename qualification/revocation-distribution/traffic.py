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
                listener.setsockopt(socket.IPPROTO_IP, socket.IP_TOS, 0xA0)
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(("0.0.0.0", 9000))
            listener.listen()
            while True:
                connection, _ = listener.accept()
                threading.Thread(
                    target=echo_connection, args=(connection,), daemon=True
                ).start()

    threading.Thread(target=tcp_server, daemon=True).start()
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver:
        if control:
            receiver.setsockopt(socket.IPPROTO_IP, socket.IP_TOS, 0xA0)
        receiver.bind(("0.0.0.0", 9001))
        while True:
            payload, address = receiver.recvfrom(1024)
            receiver.sendto(payload, address)


def valid_sequence(payload, last_sent_sequence):
    if not payload.isdigit():
        return None
    sequence = int(payload)
    return sequence if sequence <= last_sent_sequence else None


def receive_tcp(connection, pending, last_sent_sequence, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        connection.settimeout(max(0.001, deadline - time.monotonic()))
        try:
            chunk = connection.recv(1024)
        except socket.timeout:
            return [], pending
        if not chunk:
            return [], pending
        received_at = time.monotonic()
        pending += chunk
        receipts = []
        while b"\n" in pending:
            response, pending = pending.split(b"\n", 1)
            sequence = valid_sequence(response, last_sent_sequence)
            if sequence is not None:
                receipts.append({"sequence": sequence, "received_at": received_at})
        if receipts:
            return receipts, pending
    return [], pending


def receive_udp(datagram, last_sent_sequence, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        datagram.settimeout(max(0.001, deadline - time.monotonic()))
        try:
            payload = datagram.recvfrom(1024)[0]
        except socket.timeout:
            return []
        received_at = time.monotonic()
        sequence = valid_sequence(payload, last_sent_sequence)
        if sequence is not None:
            return [{"sequence": sequence, "received_at": received_at}]
    return []


def sample(destination, output, control, response_timeout=0.15):
    with socket.socket() as connection, socket.socket(
        socket.AF_INET, socket.SOCK_DGRAM
    ) as datagram, open(output, "w", buffering=1) as observations:
        if control:
            for transport in (connection, datagram):
                transport.setsockopt(socket.IPPROTO_IP, socket.IP_TOS, 0xA0)
        connection.settimeout(3)
        connection.connect((destination, 9000))
        pending = b""
        sequence = 0
        while True:
            payload = str(sequence).encode()
            observation = {
                "attempted_at": time.monotonic(),
                "tcp": False,
                "udp": False,
                "tcp_receipts": [],
                "udp_receipts": [],
            }
            try:
                connection.settimeout(response_timeout)
                connection.sendall(payload + b"\n")
                observation["tcp_receipts"], pending = receive_tcp(
                    connection, pending, sequence, response_timeout
                )
            except OSError:
                pass
            try:
                datagram.sendto(payload, (destination, 9001))
                observation["udp_receipts"] = receive_udp(
                    datagram, sequence, response_timeout
                )
            except OSError:
                pass
            observation["tcp"] = bool(observation["tcp_receipts"])
            observation["udp"] = bool(observation["udp_receipts"])
            observation["at"] = time.monotonic()
            observations.write(json.dumps(observation) + "\n")
            sequence += 1
            time.sleep(0.05)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=("serve", "sample"))
    parser.add_argument("--destination")
    parser.add_argument("--output")
    parser.add_argument("--control", action="store_true")
    arguments = parser.parse_args()
    if arguments.mode == "serve":
        serve(arguments.control)
    else:
        sample(arguments.destination, arguments.output, arguments.control)
