"""Real UDP traffic through an isolated credential-only relay."""
import base64
import importlib.util
import json
import os
from pathlib import Path
import secrets
import subprocess
import sys
import time
spec = importlib.util.spec_from_file_location('network_runtime', '/qualification/runtime.py')
network = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = network
spec.loader.exec_module(network)

def transfer(source, destination, port, tos=0):
    receiver_code = f"""import socket
connection = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
connection.bind(("0.0.0.0", {port}))
connection.settimeout(2)
print(connection.recv(128).decode())
"""
    receiver = subprocess.Popen(['ip', 'netns', 'exec', destination.name, 'python3', '-c', receiver_code], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    try:
        time.sleep(0.15)
        sender_code = f'import socket\nimport time\nconnection = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)\nconnection.setsockopt(socket.IPPROTO_IP, socket.IP_TOS, {tos})\nfor attempt in range(3):\n    connection.sendto(b"policy-check", ({destination.address!r}, {port}))\n    time.sleep(0.05)\n'
        network.namespace_command(source.name, ['python3', '-c', sender_code])
        output, _ = receiver.communicate(timeout=3)
        return output.strip() == 'policy-check'
    finally:
        if receiver.poll() is None:
            receiver.kill()
            receiver.wait()

def main():
    report = {'scope': 'actual UDP, local per-connection enforcement; no claim of topology-wide optimization', 'gates': [], 'qualified_for_cutover': False}
    nodes = []

    def record(name, passed):
        report['gates'].append({'name': name, 'status': 'pass' if passed else 'fail'})
        print(name, passed, flush=True)

    def start(node, metered=False, allow_video=False):
        os.environ['AVIAN_TRAFFIC_POLICY'] = json.dumps({'metered_addresses': [node.physical_address] if metered else [], 'allow_metered_video': allow_video})
        node.start()
    try:
        network.STATE.mkdir(exist_ok=True)
        network.run(['ip', 'link', 'add', 'underlay', 'type', 'bridge'])
        network.run(['ip', 'link', 'set', 'underlay', 'up'])
        source, relay, destination = [network.Node(name, index) for name, index in [('source', 1), ('relay', 2), ('destination', 3)]]
        nodes = [source, relay, destination]
        trust = {}
        for node in nodes:
            node.create()
            credential = base64.b64encode(secrets.token_bytes(32)).decode()
            node.configure(credential=credential, peers=[] if node is relay else [relay])
            private_der = bytes.fromhex('302e020100300506032b656e04220420') + base64.b64decode(credential)
            public_der = subprocess.run(['openssl', 'pkey', '-inform', 'DER', '-pubout', '-outform', 'DER'], input=private_der, check=True, capture_output=True).stdout
            trust[node.name] = {'pubkey': base64.b64encode(public_der[-32:]).decode(), 'groups': [], 'allow_relay': True, 'allowed_proxy_cidrs': [], 'overlay_addresses': [node.address], 'reusable': False, 'expiry_unix': int(time.time()) + 300, 'created_at_unix': int(time.time())}
        for node in nodes:
            (network.STATE / (node.name + '-credentials.json')).write_text(json.dumps({'format': 'avian-verified-membership-v1', 'credentials': trust}))
            node.require_verified_membership = True
        for node in nodes:
            start(node)
        if not network.wait_until(lambda: source.ping(destination), 20):
            raise RuntimeError('unmetered baseline unavailable')
        record('unmetered_relay_video_delivery', transfer(source, destination, 8000))
        relay.stop()
        start(relay, True)
        if not network.wait_until(lambda: source.ping(destination), 20):
            raise RuntimeError('metered relay control baseline unavailable')
        record('metered_relay_control_delivery', transfer(source, destination, 8000, 160))
        record('metered_relay_video_denied', transfer(source, destination, 8000, 160) and (not transfer(source, destination, 8000)) and transfer(source, destination, 8000, 160))
        record('metered_relay_bulk_denied', transfer(source, destination, 9000, 160) and (not transfer(source, destination, 9000)) and transfer(source, destination, 9000, 160))
        record('metered_relay_reverse_video_denied', transfer(destination, source, 8001, 160) and (not transfer(destination, source, 8001)) and transfer(destination, source, 8001, 160))
        relay.stop()
        start(relay, True, True)
        if not network.wait_until(lambda: source.ping(destination), 20):
            raise RuntimeError('video allowance control baseline unavailable')
        record('explicit_metered_video_allowance', transfer(source, destination, 8000))
        source.stop()
        start(source, True)
        if not network.wait_until(lambda: source.ping(relay), 20):
            raise RuntimeError('direct control baseline unavailable')
        record('direct_metered_video_denied', transfer(source, relay, 8000, 160) and (not transfer(source, relay, 8000)) and transfer(source, relay, 8000, 160))
        record('direct_metered_control_delivery', transfer(source, relay, 8000, 160))
    except Exception as error:
        report['runner_error'] = str(error)
    finally:
        for node in nodes:
            node.stop()
        Path('/results/traffic-policy.json').write_text(json.dumps(report, indent=2) + '\n')
    return 0 if len(report['gates']) == 8 and all((g['status'] == 'pass' for g in report['gates'])) else 1
if __name__ == '__main__':
    raise SystemExit(main())
