"""Measure actual TCP/UDP recovery in a four-node authenticated overlay diamond."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import secrets
import subprocess
import sys
import time
sys.path.insert(0, '/qualification')
from runtime import Node, run, namespace_command, wait_until


class TrialNode(Node):
    fast_failover = False

    def start(self):
        self.log_handle = Path(f'/state/{self.name}.log').open('w')
        environment = {key: value for key, value in os.environ.items() if not key.startswith('ET_')}
        environment['ET_CREDENTIAL'] = self.credential
        environment['ET_REQUIRE_VERIFIED_MEMBERSHIP'] = '1'
        if self.fast_failover:
            environment['ET_FAST_FAILOVER'] = '1'
        self.process = subprocess.Popen([
            'ip', 'netns', 'exec', self.name, '/engine/easytier-core',
            '--config-file', f'/state/{self.name}.toml', '--console-log-level', 'warn',
        ], stdout=self.log_handle, stderr=subprocess.STDOUT, env=environment)


def relay_bytes(node):
    """Observe actual physical traffic, independently of the generic route display."""
    result = namespace_command(node.name, ['ip', '-s', '-j', 'link', 'show', 'dev', f'peer{node.number}'])
    interface = json.loads(result.stdout)[0]
    statistics = interface.get('stats64', interface.get('stats'))
    return statistics['rx']['bytes'] + statistics['tx']['bytes']


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--fast-failover', action='store_true')
    parser.add_argument('--fault', choices=['blackhole', 'relay_stop'], required=True)
    arguments = parser.parse_args()
    report = {'fault': arguments.fault, 'target_seconds': 2, 'qualified_for_cutover': False, 'fast_failover': arguments.fast_failover,
              'binary_sha256': hashlib.sha256(Path('/engine/easytier-core').read_bytes()).hexdigest()}
    nodes = [TrialNode('source', 1), TrialNode('relay_a', 2), TrialNode('relay_b', 3), TrialNode('destination', 4)]
    workers = []
    try:
        run(['ip', 'link', 'add', 'underlay', 'type', 'bridge'])
        run(['ip', 'link', 'set', 'underlay', 'up'])
        private_keys = {node.name: base64.b64encode(secrets.token_bytes(32)).decode() for node in nodes}
        trust = {}
        for node in nodes:
            node.create()
            private_der = bytes.fromhex('302e020100300506032b656e04220420') + base64.b64decode(private_keys[node.name])
            public_der = subprocess.run(['openssl', 'pkey', '-inform', 'DER', '-pubout', '-outform', 'DER'], input=private_der, check=True, capture_output=True).stdout
            trust[node.name] = {'pubkey': base64.b64encode(public_der[-32:]).decode(), 'groups': [], 'allow_relay': True,
                'allowed_proxy_cidrs': [], 'overlay_addresses': [node.address], 'reusable': False,
                'expiry_unix': int(time.time()) + 300, 'created_at_unix': int(time.time())}
        source, relay_a, relay_b, destination = nodes
        for node in nodes:
            peers = [relay_a, relay_b] if node in [source, destination] else []
            node.configure(credential=private_keys[node.name], peers=peers)
            Path(f'/state/{node.name}-credentials.json').write_text(json.dumps({'format': 'avian-verified-membership-v1', 'credentials': trust}))
            node.require_verified_membership = True
            node.fast_failover = arguments.fast_failover
            node.start()
        if not wait_until(lambda: source.ping(destination) and destination.ping(source), 25):
            raise RuntimeError('diamond baseline not reachable')
        if not wait_until(lambda: all(sum(row.get('cost') == 'p2p' for row in node.command('peer')) >= 2 for node in [source, destination]), 10):
            raise RuntimeError('alternate neighbor connections not established')
        report['source_peers_before_fault'] = source.command('peer')
        report['destination_peers_before_fault'] = destination.command('peer')
        route = next(row for row in source.command('route') if row.get('hostname') == 'destination')
        report['initial_route'] = route
        relays = [relay_a, relay_b]
        idle_start = {node.name: relay_bytes(node) for node in relays}
        idle_at = time.monotonic()
        time.sleep(2)
        idle_duration = time.monotonic() - idle_at
        idle_rates = {node.name: (relay_bytes(node) - idle_start[node.name]) / idle_duration for node in relays}
        for endpoint in [source, destination]:
            workers.append(subprocess.Popen(['ip', 'netns', 'exec', endpoint.name, 'python3', '/qualification/failover/traffic.py', 'serve', endpoint.address], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        time.sleep(0.4)
        streams = []
        for endpoint, peer in [(source, destination), (destination, source)]:
            for protocol in ['tcp', 'udp']:
                name = f'{endpoint.name}-{protocol}'
                path = Path(f'/state/{name}.jsonl')
                streams.append((name, path))
                workers.append(subprocess.Popen(['ip', 'netns', 'exec', endpoint.name, 'python3', '/qualification/failover/traffic.py', protocol, peer.address, '--output', str(path)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        time.sleep(4)
        for name, path in streams:
            if not path.exists() or len(path.read_text().splitlines()) < 10:
                raise RuntimeError(f'persistent baseline stream missing: {name}')
        active_start = {node.name: relay_bytes(node) for node in relays}
        active_at = time.monotonic()
        time.sleep(2)
        active_duration = time.monotonic() - active_at
        before_fault_bytes = {node.name: relay_bytes(node) for node in relays}
        workload_bytes = {
            node.name: max(0, before_fault_bytes[node.name] - active_start[node.name]
                           - idle_rates[node.name] * active_duration)
            for node in relays
        }
        primary = max(relays, key=lambda node: workload_bytes[node.name])
        alternate = next(node for node in relays if node is not primary)
        report['path_witness'] = {
            'idle_control_bytes_per_second': idle_rates,
            'active_interval_seconds': active_duration,
            'excess_workload_bytes': workload_bytes,
            'faulted_relay': primary.name,
            'alternate_relay': alternate.name,
        }
        if workload_bytes[primary.name] < 1024 or workload_bytes[primary.name] <= 2 * workload_bytes[alternate.name]:
            raise RuntimeError('actual workload path is ambiguous; refusing to fault a relay selected only by the generic route display')
        primary_address = primary.address
        fault_time = time.monotonic()
        if arguments.fault == 'relay_stop':
            primary.stop()
        else:
            # Drop packets without changing interface/link state or closing sockets.
            for node in [source, destination]:
                for chain, flag in [('OUTPUT', '-d'), ('INPUT', '-s')]:
                    namespace_command(node.name, ['iptables', '-I', chain, flag, primary.physical_address, '-j', 'DROP'])
        report['fault_at_monotonic'] = fault_time
        deadline = fault_time + 14
        report['generic_route_change_seconds'] = None
        while time.monotonic() < deadline:
            if report['generic_route_change_seconds'] is None:
                routes = source.command('route')
                active = next((row for row in routes if row.get('hostname') == 'destination'), None)
                if active and active.get('next_hop_ipv4', '').split('/')[0] != primary_address:
                    report['generic_route_change_seconds'] = time.monotonic() - fault_time
            time.sleep(0.2)
        report['streams'] = []
        for name, path in streams:
            records = [json.loads(line) for line in path.read_text().splitlines()]
            before = [record for record in records if record['received'] < fault_time]
            after = [record for record in records if record['received'] >= fault_time]
            recovery = after[0]['received'] - fault_time if after else None
            times = [record['received'] for record in records]
            gaps = [right-left for left,right in zip(times,times[1:]) if right >= fault_time]
            if times: gaps.append(time.monotonic()-times[-1])
            gap = max(gaps) if gaps else None
            report['streams'].append({'name': name, 'received_before': len(before), 'received_after': len(after),
                'first_delivery_after_fault_seconds': recovery, 'delivery_gap_seconds': gap,
                'passed': gap is not None and gap <= 2})
        after_fault_duration = time.monotonic() - fault_time
        after_workload = {
            node.name: max(0, relay_bytes(node) - before_fault_bytes[node.name]
                           - idle_rates[node.name] * after_fault_duration)
            for node in relays
        }
        report['path_witness']['post_fault_excess_workload_bytes'] = after_workload
        alternate_observed = after_workload[alternate.name] > 1024 and after_workload[alternate.name] > 2 * after_workload[primary.name]
        report['path_witness']['alternate_carries_workload'] = alternate_observed
        report['status'] = 'pass' if alternate_observed and all(stream['passed'] for stream in report['streams']) else 'fail'
        report['route_after_fault'] = source.command('route')
    except Exception as error:
        report['status'] = 'error'
        report['error'] = str(error)
        report['diagnostics'] = {}
        for node in nodes:
            try:
                report['diagnostics'][node.name] = {'peer': node.command('peer'), 'route': node.command('route')}
            except Exception:
                report['diagnostics'][node.name] = {'process_exit': node.process.poll() if node.process else None}
            log_path = Path('/state') / (node.name + '.log')
            if log_path.exists():
                lines = log_path.read_text().splitlines()
                report['diagnostics'][node.name]['failure_markers'] = [marker for marker in ['panicked', 'pingpong loss too much', 'unknown', 'ConnectionRefused', 'ConnectionReset'] if any(marker in line for line in lines)]
    finally:
        for process in workers:
            process.terminate()
        for process in workers:
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        for node in nodes:
            node.stop()
        Path('/results/runtime.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps(report, indent=2))
    return 0 if report['status'] == 'pass' else 2

if __name__ == '__main__':
    raise SystemExit(main())
