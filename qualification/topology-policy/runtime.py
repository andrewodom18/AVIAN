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
    report = {'scope': 'Directed remote policy advertisements and per-class next-hop selection', 'gates': [], 'qualified_for_cutover': False}
    nodes = []
    def record(name, passed):
        report['gates'].append({'name': name, 'status': 'pass' if passed else 'fail'})
        print(name, passed, flush=True)
    def start(node, allow_video=False):
        os.environ['AVIAN_TRAFFIC_POLICY'] = json.dumps({'metered_addresses': [node.physical_address] if node.name == 'metered' else [], 'allow_metered_video': allow_video})
        node.start()
    try:
        network.STATE.mkdir(exist_ok=True)
        network.run(['ip', 'link', 'add', 'underlay', 'type', 'bridge'])
        network.run(['ip', 'link', 'set', 'underlay', 'up'])
        source, metered, radio, destination = [network.Node(name, index) for name, index in [('source', 1), ('metered', 2), ('radio', 3), ('destination', 4)]]
        nodes = [source, metered, radio, destination]
        trust = {}
        for node in nodes:
            node.create()
            credential = base64.b64encode(secrets.token_bytes(32)).decode()
            node.configure(credential=credential, peers=[metered, radio] if node in [source, destination] else [])
            private_der = bytes.fromhex('302e020100300506032b656e04220420') + base64.b64decode(credential)
            public_der = subprocess.run(['openssl', 'pkey', '-inform', 'DER', '-pubout', '-outform', 'DER'], input=private_der, check=True, capture_output=True).stdout
            trust[node.name] = {'pubkey': base64.b64encode(public_der[-32:]).decode(), 'groups': [], 'allow_relay': True, 'allowed_proxy_cidrs': [], 'overlay_addresses': [node.address], 'reusable': False, 'expiry_unix': int(time.time()) + 300, 'created_at_unix': int(time.time())}
        network.namespace_command('radio', ['tc', 'qdisc', 'add', 'dev', 'peer3', 'root', 'netem', 'delay', '20ms'])
        for node in nodes:
            (network.STATE / (node.name + '-credentials.json')).write_text(json.dumps({'format': 'avian-verified-membership-v1', 'credentials': trust}))
            node.require_verified_membership = True
            start(node)
        if not network.wait_until(lambda: source.ping(destination), 30): raise RuntimeError('control baseline unavailable')
        time.sleep(6)
        record('video_uses_longer_permitted_remote_path', transfer(source, destination, 8000))
        record('reverse_video_uses_permitted_remote_path', transfer(destination, source, 8001))
        radio.stop()
        time.sleep(4)
        record('control_uses_remaining_metered_path', transfer(source, destination, 8000, 0xa0))
        record('video_denied_without_eligible_remote_path', not transfer(source, destination, 8000))
        record('bulk_denied_without_eligible_remote_path', not transfer(source, destination, 9000))
        metered.stop()
        start(metered, allow_video=True)
        if not network.wait_until(lambda: source.ping(destination), 30): raise RuntimeError('allowance control unavailable')
        time.sleep(3)
        record('remote_explicit_video_allowance', transfer(source, destination, 8000))
        start(radio)
        if not network.wait_until(lambda: transfer(source, destination, 8000), 20): raise RuntimeError('restored path unavailable')
        record('partition_rejoin_restores_delivery', True)
    except Exception as error:
        report['error'] = str(error)
        report['logs'] = {node.name: (network.STATE/(node.name+'.log')).read_text()[-2500:] for node in nodes if (network.STATE/(node.name+'.log')).exists()}
    finally:
        for node in nodes: node.stop()
        Path('/results/topology-policy.json').write_text(json.dumps(report, indent=2)+'\n')
    return 0 if len(report['gates']) == 7 and all(gate['status']=='pass' for gate in report['gates']) else 1

if __name__ == '__main__': raise SystemExit(main())
