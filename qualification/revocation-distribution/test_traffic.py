"""Socket regressions for framing, delayed receipts, and concurrent sample reads."""

import json
from pathlib import Path
import socket
import sys
import tempfile
import threading
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from competing import observations
from traffic import receive_tcp, receive_udp


class TrafficSamples(unittest.TestCase):
    def test_split_and_coalesced_tcp_frames(self):
        reader, writer = socket.socketpair()
        with reader, writer:
            writer.sendall(b"0\n1")
            first, pending = receive_tcp(reader, b"", 2, 0.1)
            self.assertEqual([receipt["sequence"] for receipt in first], [0])
            self.assertEqual(pending, b"1")
            completed_at = time.monotonic()
            writer.sendall(b"\n2\n")
            remaining, pending = receive_tcp(reader, pending, 2, 0.1)
            self.assertEqual([receipt["sequence"] for receipt in remaining], [1, 2])
            self.assertEqual(pending, b"")
            self.assertTrue(
                all(receipt["received_at"] >= completed_at for receipt in remaining)
            )

    def test_late_tcp_previous_sequence_is_delivery(self):
        reader, writer = socket.socketpair()
        with reader, writer:
            writer.sendall(b"0")
            first, pending = receive_tcp(reader, b"", 0, 0.02)
            self.assertEqual(first, [])
            self.assertEqual(pending, b"0")
            deadline = time.monotonic()
            writer.sendall(b"\n")
            delayed, pending = receive_tcp(reader, pending, 1, 0.1)
            self.assertEqual(delayed[0]["sequence"], 0)
            self.assertGreaterEqual(delayed[0]["received_at"], deadline)

    def test_late_udp_previous_sequence_uses_receipt_time(self):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as reader, socket.socket(
            socket.AF_INET, socket.SOCK_DGRAM
        ) as writer:
            reader.bind(("127.0.0.1", 0))
            deadline = time.monotonic()

            def deliver():
                time.sleep(0.03)
                writer.sendto(b"0", reader.getsockname())

            sender = threading.Thread(target=deliver)
            sender.start()
            delayed = receive_udp(reader, 1, 0.2)
            sender.join()
            self.assertEqual(delayed[0]["sequence"], 0)
            self.assertGreater(delayed[0]["received_at"], deadline + 0.02)

    def test_partial_final_sample_does_not_break_reader(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "samples.jsonl"
            first = {"at": 1, "tcp": True}
            path.write_text(json.dumps(first) + '\n{"at":')
            self.assertEqual(observations(path), [first])


if __name__ == "__main__":
    unittest.main()
