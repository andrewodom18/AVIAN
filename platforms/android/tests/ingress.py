"""Synthetic emulator ingress to an isolated Docker peer; no physical-radio qualification."""

from collections import deque
import os
import re
import select
import socket
import subprocess
import threading
import time

# Readiness is consumed by the host before this stdout becomes an opaque data
# stream. The backend connection starts only after the first actual client byte.
REMOTE_STREAM = r"""
import socket
import sys
import threading
import select

sys.stdout.buffer.write(b"\x01")
sys.stdout.buffer.flush()
first = sys.stdin.buffer.read1(65536)
if not first:
    raise SystemExit(0)
backend = socket.create_connection(("127.0.0.1", 11010), timeout=5)
backend.settimeout(None)
backend.sendall(first)
input_closed = threading.Event()

def upload():
    try:
        while True:
            data = sys.stdin.buffer.read1(65536)
            if not data:
                break
            backend.sendall(data)
    except OSError:
        pass
    finally:
        input_closed.set()
        try:
            backend.shutdown(socket.SHUT_WR)
        except OSError:
            pass

threading.Thread(target=upload, daemon=True).start()
try:
    while True:
        if not select.select([backend], [], [], 1)[0]:
            if input_closed.is_set():
                break
            continue
        data = backend.recv(65536)
        if not data:
            break
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.flush()
finally:
    backend.close()
"""


class _Worker:
    def __init__(self, container_id):
        self.process = subprocess.Popen(
            [
                "docker",
                "exec",
                "-i",
                container_id,
                "python3",
                "-u",
                "-c",
                REMOTE_STREAM,
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            bufsize=0,
        )
        self.lock = threading.Lock()
        self.closed = False

    def close(self):
        with self.lock:
            if self.closed:
                return
            self.closed = True
            # Termination first also unblocks a writer stalled on the exec pipe.
            if self.process.poll() is None:
                self.process.terminate()
            for pipe in (self.process.stdin, self.process.stdout):
                try:
                    pipe.close()
                except OSError:
                    pass
            try:
                self.process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=1)


class TransparentIngressBroker:
    """Bounded opaque TCP transport to container loopback port 11010.

    This is an emulator test transport. It leaves Docker's internal network
    isolated and does not terminate or inspect the encrypted peer protocol.
    """

    SPARE_WORKERS = 4
    MAXIMUM_ACTIVE_CONNECTIONS = 8
    transport_kind = "synthetic_host_loopback_docker_exec"

    def __init__(self, container_id, *, startup_timeout=25):
        if not isinstance(container_id, str) or not re.fullmatch(
            r"[0-9a-f]{64}", container_id
        ):
            raise ValueError("A complete Docker container ID is required")
        if not 1 <= startup_timeout <= 60:
            raise ValueError("Worker startup timeout must be between 1 and 60 seconds")
        self.container_id = container_id
        self.startup_timeout = startup_timeout
        self.condition = threading.Condition()
        self.stopping = threading.Event()
        self.listener = None
        self.spares = deque()
        self.workers = set()
        self.connections = set()
        self.threads = set()
        self.warming = 0
        self.last_failure = None
        self.retry_after = 0.0

    @property
    def port(self):
        if self.listener is None:
            raise RuntimeError("Ingress broker has not started")
        return self.listener.getsockname()[1]

    def __enter__(self):
        return self.start()

    def __exit__(self, exception_type, exception, traceback):
        self.close()

    def _thread(self, target, *arguments):
        def run():
            try:
                target(*arguments)
            finally:
                with self.condition:
                    self.threads.discard(threading.current_thread())
                    self.condition.notify_all()

        thread = threading.Thread(target=run, daemon=True)
        with self.condition:
            self.threads.add(thread)
        thread.start()

    def start(self):
        if self.listener is not None or self.stopping.is_set():
            raise RuntimeError("Ingress broker cannot be started twice")
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(self.MAXIMUM_ACTIVE_CONNECTIONS)
        self.listener.settimeout(0.2)
        self._thread(self._maintain_spares)
        deadline = time.monotonic() + self.startup_timeout
        with self.condition:
            while len(self.spares) < self.SPARE_WORKERS:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                self.condition.wait(timeout=remaining)
            ready = len(self.spares) == self.SPARE_WORKERS
        if not ready:
            self.close()
            raise RuntimeError(
                self.last_failure or "Docker ingress workers did not become ready"
            )
        self._thread(self._accept)
        return self

    def _maintain_spares(self):
        while not self.stopping.is_set():
            with self.condition:
                if time.monotonic() < self.retry_after:
                    self.condition.wait(timeout=self.retry_after - time.monotonic())
                    continue
                missing = self.SPARE_WORKERS - len(self.spares) - self.warming
                for _ in range(max(0, missing)):
                    self.warming += 1
                    self._thread(self._warm_worker)
                self.condition.wait(timeout=0.25)

    def _warm_worker(self):
        worker = None
        queued = False
        try:
            if self.stopping.is_set():
                return
            worker = _Worker(self.container_id)
            with self.condition:
                self.workers.add(worker)
            deadline = time.monotonic() + self.startup_timeout
            while not self.stopping.is_set() and time.monotonic() < deadline:
                if worker.process.poll() is not None:
                    break
                if select.select([worker.process.stdout], [], [], 0.1)[0]:
                    if os.read(worker.process.stdout.fileno(), 1) == b"\x01":
                        with self.condition:
                            if not self.stopping.is_set():
                                self.spares.append(worker)
                                queued = True
                    break
            if not queued and not self.stopping.is_set():
                self.last_failure = "Docker ingress worker readiness failed"
        except (OSError, ValueError):
            self.last_failure = "Docker ingress worker could not start"
        finally:
            if worker is not None and not queued:
                worker.close()
                with self.condition:
                    self.workers.discard(worker)
            with self.condition:
                self.warming -= 1
                if not queued:
                    self.retry_after = time.monotonic() + 0.25
                self.condition.notify_all()

    def _accept(self):
        while not self.stopping.is_set():
            try:
                connection, _ = self.listener.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            with self.condition:
                if (
                    self.stopping.is_set()
                    or len(self.connections) >= self.MAXIMUM_ACTIVE_CONNECTIONS
                    or not self.spares
                ):
                    connection.close()
                    continue
                worker = self.spares.popleft()
                self.connections.add(connection)
                self.condition.notify_all()
            connection.settimeout(None)
            self._thread(self._forward, connection, worker)

    def _forward(self, connection, worker):
        def upload():
            try:
                while not self.stopping.is_set():
                    if not select.select([connection], [], [], 0.2)[0]:
                        continue
                    data = connection.recv(65536)
                    if not data:
                        break
                    view = memoryview(data)
                    while view:
                        written = os.write(worker.process.stdin.fileno(), view)
                        view = view[written:]
            except (OSError, ValueError):
                pass
            finally:
                try:
                    worker.process.stdin.close()
                except OSError:
                    pass

        self._thread(upload)
        try:
            while not self.stopping.is_set():
                if not select.select([worker.process.stdout], [], [], 0.2)[0]:
                    if worker.process.poll() is not None:
                        break
                    continue
                data = os.read(worker.process.stdout.fileno(), 65536)
                if not data:
                    break
                connection.sendall(data)
        except (OSError, ValueError):
            pass
        finally:
            try:
                connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            connection.close()
            worker.close()
            with self.condition:
                self.connections.discard(connection)
                self.workers.discard(worker)
                self.condition.notify_all()

    def close(self):
        self.stopping.set()
        if self.listener is not None:
            self.listener.close()
        with self.condition:
            connections = list(self.connections)
            workers = list(self.workers)
            self.condition.notify_all()
        for connection in connections:
            try:
                connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            connection.close()
        for worker in workers:
            worker.close()
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            with self.condition:
                threads = list(self.threads)
            if not threads:
                break
            for thread in threads:
                thread.join(timeout=max(0, min(0.2, deadline - time.monotonic())))
        with self.condition:
            if self.threads:
                raise RuntimeError("Ingress worker threads did not stop")
            self.spares.clear()
            self.workers.clear()
            self.connections.clear()
