"""Exchange signed records through real overlay links and enforce received trust."""

import base64
import argparse
import copy
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import sys
import time
import tomllib

sys.path.insert(0, '/qualification')
from runtime import Node, STATE, run, wait_until

SERVICES = Path('/services')
DIRECTORY = Path('/qualification/revocation-distribution')
PORT = 9180


def command(*arguments):
    result = run([str(SERVICES / 'avianctl'), *map(str, arguments)])
    return json.loads(result.stdout)


def revision(directory):
    stored = json.loads((directory / 'membership.json').read_text())
    records = stored['revocations']
    return records[-1]['claims']['revision'] if records else 0


def launch_service(node, directory, peer):
    log = (STATE / (node.name + '-distribution.log')).open('a')
    process = subprocess.Popen([
        'ip', 'netns', 'exec', node.name, str(SERVICES / 'avian-revocationd'),
        '--trust-root', str(directory / 'trust-root.json'),
        '--membership', str(directory / 'membership.json'),
        '--engine-trust', str(STATE / (node.name + '-credentials.json')),
        '--listen', f'{node.address}:{PORT}',
        '--peer', peer, '--poll-interval-ms', '200', '--batch-size', '1',
    ], stdout=log, stderr=subprocess.STDOUT)
    log.close()
    return process


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--metered-sync', action='store_true')
    arguments = parser.parse_args()
    nodes = [Node('administrator', 1), Node('relay', 2), Node('source', 3), Node('destination', 4)]
    administrator, relay, source, destination = nodes
    processes = []
    directories = {node.name: STATE / node.name for node in nodes}
    report = {'gates': [], 'metered_sync': arguments.metered_sync, 'qualified_for_cutover': False}

    def record(name, passed, **measurements):
        report['gates'].append({'name': name, 'status': 'pass' if passed else 'fail', **measurements})
        print(name, passed, flush=True)

    def revoke(node_id, name):
        output = STATE / (name + '.json')
        command('--config', directories['administrator'] / 'avian.toml', 'revoke', '--node', node_id, '--output', output)
        return json.loads(output.read_text())

    try:
        STATE.mkdir(mode=0o700, exist_ok=True)
        run(['ip', 'link', 'add', 'underlay', 'type', 'bridge'])
        run(['ip', 'link', 'set', 'underlay', 'up'])
        for node in nodes:
            node.create()
            credential = base64.b64encode(secrets.token_bytes(32)).decode()
            peers = [] if node is administrator else [administrator] if node is relay else [relay]
            node.configure(credential=credential, peers=peers)
            node.require_verified_membership = True
        administrator_directory = directories['administrator']
        command('init', '--state-dir', administrator_directory, '--network', 'distribution-test')
        administrator_config = administrator_directory / 'avian.toml'
        identities = {}
        for node in nodes:
            identity = command('keygen', '--output', STATE / (node.name + '.identity'))
            configuration = tomllib.loads((STATE / (node.name + '.toml')).read_text())
            transport = base64.b64decode(configuration['secure_mode']['local_public_key']).hex()
            result = command('--config', administrator_config, 'enroll',
                             '--identity-public-key', identity['identity_public_key'],
                             '--transport-public-key', transport, '--address', node.address,
                             '--relay', '--output', STATE / (node.name + '-membership.json'))
            identities[node.name] = result['node_id']
        unused_identities = []
        for index in [10, 11, 12, 13]:
            identity = command('keygen', '--output', STATE / f'unused-{index}.identity')
            result = command('--config', administrator_config, 'enroll',
                             '--identity-public-key', identity['identity_public_key'],
                             '--transport-public-key', secrets.token_hex(32), '--address', f'10.201.0.{index}',
                             '--output', STATE / f'unused-{index}.json')
            unused_identities.append(result['node_id'])
        for node in nodes[1:]:
            command('init', '--state-dir', directories[node.name], '--network', 'temporary')
            for filename in ['trust-root.json', 'membership.json']:
                shutil.copyfile(administrator_directory / filename, directories[node.name] / filename)
            (directories[node.name] / 'administrator.key').unlink()
        for node in nodes:
            command('--config', directories[node.name] / 'avian.toml', 'export-engine-trust',
                    '--output', STATE / (node.name + '-credentials.json'))
            if arguments.metered_sync:
                os.environ['AVIAN_TRAFFIC_POLICY'] = json.dumps({
                    'metered_addresses': [node.physical_address], 'control_tcp_ports': [PORT, 9181],
                })
            node.start()
        if not wait_until(lambda: source.ping(destination), 20):
            raise RuntimeError('Initial overlay traffic unavailable')
        services = {}
        for node in nodes:
            peer = administrator if node in [administrator, relay] else relay
            services[node.name] = launch_service(node, directories[node.name], f'http://{peer.address}:{PORT}')
            processes.append(services[node.name])
        time.sleep(1)
        if any(process.poll() is not None for process in services.values()):
            raise RuntimeError('Distribution service exited before testing')
        first = revoke(unused_identities[0], 'revision-one')
        learned = wait_until(lambda: revision(directories['relay']) == 1 and revision(directories['source']) == 1, 10)
        record('signed_update_crosses_relay', learned)
        if not learned:
            raise RuntimeError('Peers did not learn first signed update')
        if arguments.metered_sync:
            ordinary_response = STATE / 'ordinary-response'
            ordinary_response.write_text('ordinary')
            ordinary_process = subprocess.Popen([
                'ip', 'netns', 'exec', 'relay', 'python3', str(DIRECTORY / 'public_response.py'),
                '--port', '9182', '--response', str(ordinary_response),
                '--marker', str(STATE / 'ordinary-request'),
            ])
            processes.append(ordinary_process)
            time.sleep(0.2)
            result = run(['ip', 'netns', 'exec', 'source', 'python3', '-c',
                          f'import urllib.request\nurllib.request.urlopen("http://{relay.address}:9182/",timeout=1).read()'], check=False)
            record('unregistered_http_denied_on_metered_link',
                   result.returncode != 0 and not (STATE / 'ordinary-request').exists()
                   and ordinary_process.poll() is None and source.ping(relay))
            stop(ordinary_process)

        # A signature-invalid public response cannot change durable peer trust.
        stop(services['source'])
        forged = copy.deepcopy(first)
        forged['claims']['previous_revision'] = 1
        forged['claims']['revision'] = 2
        batch = {'version': 1, 'network_id': 'distribution-test', 'after_revision': 1,
                 'next_revision': 2, 'current_revision': 2, 'records': [forged]}
        fake_response = STATE / 'forged-response.json'
        fake_response.write_text(json.dumps(batch))
        server = subprocess.Popen([
            'ip', 'netns', 'exec', 'relay', 'python3', str(DIRECTORY / 'public_response.py'),
            '--port', '9181', '--response', str(fake_response),
            '--marker', str(STATE / 'fake-request'),
        ])
        processes.append(server)
        services['source'] = launch_service(source, directories['source'], f'http://{relay.address}:9181')
        processes.append(services['source'])
        attempted = wait_until(lambda: (STATE / 'fake-request').exists(), 5)
        time.sleep(1)
        record('tampered_signature_rejected', attempted and revision(directories['source']) == 1 and services['source'].poll() is None)
        revoke(unused_identities[1], 'revision-two')
        third = revoke(unused_identities[2], 'revision-three')
        invalid_batches = [
            ('wrong_network_rejected', dict(batch, network_id='another-network', records=[])),
            ('signed_replay_rejected', dict(batch, records=[first])),
            ('missing_signed_revision_rejected', dict(batch, next_revision=3, current_revision=3, records=[third])),
        ]
        for name, invalid_batch in invalid_batches:
            (STATE / 'fake-request').unlink(missing_ok=True)
            fake_response.write_text(json.dumps(invalid_batch))
            attempted = wait_until(lambda: (STATE / 'fake-request').exists(), 5)
            time.sleep(0.5)
            record(name, attempted and revision(directories['source']) == 1 and services['source'].poll() is None)
        stop(services['source'])
        stop(server)
        services['source'] = launch_service(source, directories['source'], f'http://{relay.address}:{PORT}')
        processes.append(services['source'])
        if not wait_until(lambda: revision(directories['source']) == 3, 10):
            raise RuntimeError('Valid paginated history did not recover after rejected updates')

        # During a partition only the administrator receives the newly issued record.
        for direction in ['INPUT', 'OUTPUT']:
            option = '-s' if direction == 'INPUT' else '-d'
            run(['ip', 'netns', 'exec', 'source', 'iptables', '-A', direction,
                 option, relay.physical_address, '-j', 'DROP'])
        revoke(unused_identities[3], 'revision-four')
        relay_learned = wait_until(lambda: revision(directories['relay']) == 4, 10)
        time.sleep(1)
        record('partition_does_not_invent_revision', relay_learned and revision(directories['source']) == 3)
        stop(services['source'])
        for direction in ['INPUT', 'OUTPUT']:
            option = '-s' if direction == 'INPUT' else '-d'
            run(['ip', 'netns', 'exec', 'source', 'iptables', '-D', direction,
                 option, relay.physical_address, '-j', 'DROP'])
        services['source'] = launch_service(source, directories['source'], f'http://{relay.address}:{PORT}')
        processes.append(services['source'])
        caught_up = wait_until(lambda: revision(directories['source']) == 4, 15)
        record('partition_restart_catches_up', caught_up)
        if not caught_up or not wait_until(lambda: source.ping(destination), 15):
            raise RuntimeError('Partition recovery unavailable')

        control_arguments = ['--control'] if arguments.metered_sync else []
        echo = subprocess.Popen(['ip', 'netns', 'exec', 'destination', 'python3', str(DIRECTORY / 'traffic.py'), 'serve', *control_arguments])
        processes.append(echo)
        time.sleep(0.3)
        sample_path = STATE / 'persistent-traffic.jsonl'
        client = subprocess.Popen(['ip', 'netns', 'exec', 'source', 'python3',
                                   str(DIRECTORY / 'traffic.py'), 'sample',
                                   '--destination', destination.address, '--output', str(sample_path), *control_arguments])
        processes.append(client)

        def traffic_ready():
            if not sample_path.exists():
                return False
            samples = [json.loads(line) for line in sample_path.read_text().splitlines()]
            return sum(sample['tcp'] and sample['udp'] for sample in samples) >= 3

        if not wait_until(traffic_ready, 5):
            raise RuntimeError('Persistent TCP and UDP baseline did not become ready')
        record('persistent_tcp_udp_baseline', True)
        time.sleep(0.3)
        issued_at = time.monotonic()
        revoke(identities['destination'], 'revision-five')
        received = wait_until(lambda: revision(directories['source']) == 5 and revision(directories['relay']) == 5, 10)

        def enough_after_deadline_samples():
            samples = [json.loads(line) for line in sample_path.read_text().splitlines()]
            return sum(sample['at'] > issued_at + 2 for sample in samples) >= 3

        wait_until(enough_after_deadline_samples, 5)
        stop(client)
        rows = [json.loads(line) for line in sample_path.read_text().splitlines()]
        before = [row for row in rows if row['at'] < issued_at]
        after = [row for row in rows if row['at'] > issued_at + 2]
        stopped = received and before and after and any(row['tcp'] and row['udp'] for row in before)
        stopped = bool(stopped and not any(row['tcp'] or row['udp'] for row in after))
        record('distributed_revocation_stops_established_traffic', stopped, bound_seconds=2,
               signed_revision_received=received,
               successful_baseline_samples=sum(row['tcp'] and row['udp'] for row in before),
               after_deadline_samples=len(after),
               successful_after_deadline=sum(row['tcp'] or row['udp'] for row in after))
        record('ordinary_peers_have_no_issuer_key', all(not (directories[node.name] / 'administrator.key').exists() for node in nodes[1:]))
    except Exception as problem:
        report['error'] = str(problem)
    finally:
        for process in processes:
            stop(process)
        for node in nodes:
            node.stop()
        Path('/results/distribution.json').write_text(json.dumps(report, indent=2) + '\n')
    expected_gates = 11 if arguments.metered_sync else 10
    return 0 if len(report['gates']) == expected_gates and all(gate['status'] == 'pass' for gate in report['gates']) else 1


if __name__ == '__main__':
    raise SystemExit(main())
