"""Control echo and competing UDP loads for isolated network measurements."""

import argparse
import json
from pathlib import Path
import socket
import threading
import time


def serve(address, output):
    counters = {19000: 0, 8000: 0, 19001: 0}

    def publish_counters():
        while True:
            destination = Path(output)
            temporary = destination.with_suffix(".tmp")
            temporary.write_text(json.dumps(counters))
            temporary.replace(destination)
            time.sleep(0.1)

    def receive(port):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as listener:
            if port == 19000:
                listener.setsockopt(socket.IPPROTO_IP, socket.IP_TOS, 0xA0)
            listener.bind((address, port))
            while True:
                packet, peer = listener.recvfrom(65535)
                counters[port] += len(packet)
                if port == 19000:
                    try:
                        listener.sendto(packet, peer)
                    except OSError:
                        pass

    if output:
        threading.Thread(target=publish_counters, daemon=True).start()
    for port in [8000, 19001]:
        threading.Thread(target=receive, args=(port,), daemon=True).start()
    receive(19000)


def control(address, duration, output, interval_seconds, payload_bytes):
    started_at = time.monotonic()
    deadline = started_at + duration
    sent = 0
    send_errors = 0
    receive_errors = 0
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as connection, Path(
        output
    ).open("w", buffering=1) as log:
        connection.setsockopt(socket.IPPROTO_IP, socket.IP_TOS, 0xA0)
        connection.settimeout(0.03)
        connection.connect((address, 19000))

        def receive_reply():
            nonlocal receive_errors
            try:
                packet = json.loads(connection.recv(4096))
                received = time.monotonic()
                log.write(
                    json.dumps(
                        {
                            "sequence": packet["sequence"],
                            "sent": packet["sent"],
                            "received": received,
                            "round_trip_seconds": received - packet["sent"],
                        }
                    )
                    + "\n"
                )
                return True
            except socket.timeout:
                return False
            except OSError:
                receive_errors += 1
                return False

        while time.monotonic() < deadline:
            started = time.monotonic()
            payload = (
                json.dumps({"sequence": sent, "sent": started})
                .encode()
                .ljust(payload_bytes, b" ")
            )
            sent += 1
            try:
                connection.send(payload)
            except OSError:
                send_errors += 1
            receive_reply()
            time.sleep(max(0, interval_seconds - (time.monotonic() - started)))
        sending_finished = time.monotonic()
        # Drain already-sent probes before calculating loss; in-flight replies are not loss.
        drain_deadline = time.monotonic() + 2
        while time.monotonic() < drain_deadline:
            if not receive_reply() and time.monotonic() - deadline >= 1:
                break
        log.write(
            json.dumps(
                {
                    "total_sent": sent,
                    "sending_finished": sending_finished,
                    "started": started_at,
                    "requested_duration_seconds": duration,
                    "completed": True,
                    "send_errors": send_errors,
                    "receive_errors": receive_errors,
                }
            )
            + "\n"
        )


def flood(address, port, duration, bits_per_second, output):
    payload = b"x" * 1100
    interval = len(payload) * 8 / bits_per_second
    started = time.monotonic()
    deadline = started + duration
    attempted = 0
    sent = 0
    errors = 0
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as connection:
        connection.connect((address, port))
        next_send = time.monotonic()
        while time.monotonic() < deadline:
            attempted += 1
            try:
                connection.send(payload)
                sent += 1
            except OSError:
                errors += 1
            next_send += interval
            time.sleep(max(0, next_send - time.monotonic()))
    if output:
        Path(output).write_text(
            json.dumps(
                {
                    "started": started,
                    "finished": time.monotonic(),
                    "requested_duration_seconds": duration,
                    "offered_bps": bits_per_second,
                    "attempted_datagrams": attempted,
                    "sent_datagrams": sent,
                    "sent_bytes": sent * len(payload),
                    "send_errors": errors,
                    "completed": True,
                }
            )
            + "\n"
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["serve", "control", "video", "bulk"])
    parser.add_argument("address")
    parser.add_argument("--duration", type=float, default=10)
    parser.add_argument("--offered-bps", type=int, default=4000000)
    parser.add_argument("--output")
    parser.add_argument("--interval-seconds", type=float, default=0.05)
    parser.add_argument("--payload-bytes", type=int, default=64)
    arguments = parser.parse_args()
    if arguments.mode == "serve":
        serve(arguments.address, arguments.output)
    elif arguments.mode == "control":
        control(
            arguments.address,
            arguments.duration,
            arguments.output,
            arguments.interval_seconds,
            arguments.payload_bytes,
        )
    else:
        flood(
            arguments.address,
            8000 if arguments.mode == "video" else 19001,
            arguments.duration,
            arguments.offered_bps,
            arguments.output,
        )
