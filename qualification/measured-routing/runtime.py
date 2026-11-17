"""Observe packet paths selected by live quality and bounded capacity evidence."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

sys.path.insert(0, '/qualification')
from load.runtime import (LoadNode, credential_record, physical_bytes, publish_trust,
                          namespace_command, run, stop_worker, wait_until, worker)


class MeasuredNode(LoadNode):
    def start(self):
        self.log_handle = Path(f'/state/{self.name}.log').open('w')
        environment = {key: value for key, value in os.environ.items()
                       if not key.startswith('ET_') and not key.startswith('AVIAN_')}
        environment.update({'ET_CREDENTIAL': self.credential, 'ET_REQUIRE_VERIFIED_MEMBERSHIP': '1',
            'ET_FAST_FAILOVER': '1', 'ET_PRIORITY_FORWARDING': '1', 'ET_MEASURED_ROUTING': '1',
            'AVIAN_TRAFFIC_POLICY_FILE': f'/state/{self.name}-policy.json'})
        self.process = subprocess.Popen(['ip', 'netns', 'exec', self.name, '/engine/easytier-core',
            '--config-file', f'/state/{self.name}.toml', '--console-log-level', 'warn'],
            env=environment, stdout=self.log_handle, stderr=subprocess.STDOUT)

    @property
    def stable_node_id(self):
        return hashlib.sha256(('fixture:' + self.name).encode()).hexdigest()

    def command(self, *arguments):
        response = super().command(*arguments)
        if arguments in [('route', 'measured'), ('route', 'links')] and isinstance(response, dict) and 'items' in response:
            items = response['items']
            cursor = response.get('next_cursor')
            visited = set()
            while cursor:
                if cursor in visited:
                    raise RuntimeError('measurement page cursor did not advance')
                visited.add(cursor)
                response = super().command(*arguments, '--after', cursor)
                items.extend(response['items'])
                cursor = response.get('next_cursor')
            return items
        return response

    def policy(self, **values):
        path = Path(f'/state/{self.name}-policy.json')
        temporary = path.with_suffix('.tmp')
        temporary.write_text(json.dumps(values))
        temporary.chmod(0o600)
        temporary.replace(path)


def main():
    report = {'scope': 'Actual isolated Linux UDP/TCP forwarding with authenticated synthetic membership.',
              'qualified_for_cutover': False, 'gates': [], 'probes': []}
    nodes = []
    workers = []

    def gate(name, passed, evidence=None):
        report['gates'].append({'name': name, 'status': 'pass' if passed else 'fail', 'evidence': evidence})
        print(name, 'pass' if passed else 'fail', flush=True)

    try:
        run(['ip', 'link', 'add', 'underlay', 'type', 'bridge'])
        run(['ip', 'link', 'set', 'underlay', 'up'])
        source, narrow, wide, destination = [MeasuredNode(name, number) for number, name in
                                             enumerate(['source', 'narrow', 'wide', 'destination'], 1)]
        nodes = [source, narrow, wide, destination]
        relays = [narrow, wide]
        trust = {}
        for node in nodes:
            node.create()
            key, trust[node.stable_node_id] = credential_record(node)
            node.configure(credential=key, peers=relays if node in [source, destination] else [])
            configuration = Path('/state') / (node.name + '.toml')
            if node is narrow:
                configuration.write_text(configuration.read_text().replace('tcp://0.0.0.0:11010',
                                                                           'udp://' + node.physical_address + ':11010'))
            elif node is wide:
                configuration.write_text(configuration.read_text().replace('listeners = ["tcp://0.0.0.0:11010"]',
                    'listeners = ["tcp://' + node.physical_address + ':11010", "udp://' + node.physical_address + ':11010"]'))
            if node in [source, destination]:
                configuration.write_text(configuration.read_text().replace('tcp://' + narrow.physical_address + ':11010',
                                                                           'udp://' + narrow.physical_address + ':11010'))
                if node is source:
                    configuration.write_text(configuration.read_text() + '\n[[peer]]\nuri = "udp://' + wide.physical_address + ':11010"\n')
                else:
                    configuration.write_text(configuration.read_text().replace('tcp://' + wide.physical_address + ':11010',
                                                                               'udp://' + wide.physical_address + ':11010'))
            if node is destination:
                configuration.write_text(configuration.read_text().replace('hostname = "destination"', 'hostname = "display-only"'))
            node.policy(metered_addresses=[])
        publish_trust(nodes, trust)
        for relay, rate in [(narrow, 2_000_000), (wide, 8_000_000)]:
            namespace_command(relay.name, ['tc', 'qdisc', 'add', 'dev', f'peer{relay.number}', 'root',
                'handle', '1:', 'tbf', 'rate', str(rate) + 'bit', 'burst', '16384', 'latency', '100ms'])
        namespace_command(wide.name, ['tc', 'qdisc', 'add', 'dev', f'peer{wide.number}', 'parent', '1:1',
                                     'handle', '10:', 'netem', 'delay', '20ms', 'limit', '128'])
        for node in nodes:
            node.start()
        if not wait_until(lambda: source.ping(destination), 30):
            raise RuntimeError('measured diamond did not become reachable')
        time.sleep(3)
        peers = {node.name: next(row['id'] for row in node.command('peer') if row['cost'] == 'Local') for node in nodes}
        destination_id = int(peers[destination.name])

        def decision(traffic_class):
            return next((row for row in source.command('route', 'measured')
                         if row['destination_peer_id'] == destination_id and row['traffic_class'] == traffic_class), None)

        received_bytes_path = Path('/state/received-bytes.json')
        workers.append(worker(destination, 'serve', destination.address, '--output', str(received_bytes_path)))
        control_received_path = Path('/state/control-path-received.json')
        workers.append(subprocess.Popen(['ip', 'netns', 'exec', destination.name, 'python3',
            '/qualification/measured-routing/control_path.py', 'serve', destination.address, '--output', str(control_received_path)]))
        time.sleep(0.2)

        def path_witness(mode, expected):
            before = {node.name: physical_bytes(node) for node in relays}
            started = time.monotonic()
            time.sleep(1)
            duration = time.monotonic() - started
            idle_rates = {node.name: (physical_bytes(node) - before[node.name]) / duration for node in relays}
            before = {node.name: physical_bytes(node) for node in relays}
            started = time.monotonic()
            output = Path('/state') / f'witness-{mode}-{time.monotonic_ns()}.jsonl'
            arguments = ['--duration', '2', '--output', str(output)]
            if mode == 'control':
                before_received = json.loads(control_received_path.read_text())['received_payload_bytes']
                process = subprocess.Popen(['ip', 'netns', 'exec', source.name, 'python3',
                    '/qualification/measured-routing/control_path.py', 'send', destination.address, *arguments])
            else:
                before_received = json.loads(received_bytes_path.read_text())['8000']
                arguments.extend(['--offered-bps', '1000000'])
                process = worker(source, mode, destination.address, *arguments)
            workers.append(process)
            process.wait(timeout=6)
            duration = time.monotonic() - started
            excess = {node.name: max(0, physical_bytes(node) - before[node.name] - idle_rates[node.name] * duration)
                      for node in relays}
            other = next(node for node in relays if node is not expected)
            time.sleep(0.2)
            after_received = (json.loads(control_received_path.read_text())['received_payload_bytes'] if mode == 'control'
                              else json.loads(received_bytes_path.read_text())['8000'])
            return {'received_payload_bytes': after_received - before_received, 'passed': process.returncode == 0 and after_received > before_received and excess[expected.name] > 8192
                    and excess[expected.name] > 2 * excess[other.name], 'excess_underlay_bytes': excess}

        pages = []
        cursor = None
        while True:
            arguments = ['route', 'measured', '--limit', '2']
            if cursor:
                arguments.extend(['--after', cursor])
            page = source.command(*arguments)
            if not isinstance(page, dict):
                break
            pages.append(page)
            cursor = page.get('next_cursor')
            if not cursor:
                break
        if pages:
            identifiers = [(item['destination_peer_id'], item['traffic_class']) for page in pages for item in page['items']]
            gate('actual_decisions_are_bounded_and_pageable', len(identifiers) == len(set(identifiers))
                 and all(len(page['items']) <= 2 for page in pages) and len(pages) > 1,
                 {'pages': len(pages), 'items': len(identifiers)})
            destination_items = [item for page in pages for item in page['items'] if item['destination_peer_id'] == destination_id]
            gate('stable_identity_comes_from_verified_cache', bool(destination_items) and all(
                item.get('destination_node_id') == destination.stable_node_id for item in destination_items))
        unknown = decision('video')
        gate('unknown_capacity_is_usable_degraded_fallback', unknown is not None
             and unknown['route']['degraded'] and unknown['route']['bottleneck_capacity_bps'] is None, unknown)
        initial_path = path_witness('video', narrow)
        gate('unknown_video_uses_observed_low_latency_path', initial_path['passed'], initial_path)
        # Measure the two directed candidate paths, targeting exact native sessions.
        def probe_link(node, peer, scheme=None):
            links = [row for row in node.command('route', 'links') if row['peer_id'] == int(peers[peer.name])
                     and (scheme is None or row['remote_address'].startswith(scheme + ':'))]
            if not links:
                raise RuntimeError('requested direct session is unavailable for probe')
            link = links[0]
            for attempt in range(4):
                try:
                    evidence = node.command('route', 'probe', '--peer-id', str(link['peer_id']),
                        '--connection-id', link['connection_id'], '--duration-ms', '500', '--offered-bps', '8000000')
                    report['probes'].append({'source': node.name, 'destination': peer.name, 'evidence': evidence})
                    return link
                except subprocess.CalledProcessError as error:
                    if attempt == 3 or not any(reason in (error.stderr or '') for reason in ['queued traffic', 'link loss', 'link jitter']):
                        raise
                    time.sleep(3)
        freshness_samples = []
        freshness_started = time.monotonic()
        while time.monotonic() - freshness_started < 12:
            current = decision('control')
            freshness_samples.append({'elapsed_seconds': time.monotonic() - freshness_started,
                'decision': current})
            time.sleep(0.25)
        quality_remained_fresh = all(sample['decision'] is not None and 'quality_unknown' not in
            sample['decision']['route']['degraded_reasons'] for sample in freshness_samples)
        gate('healthy_remote_quality_stays_fresh_across_synchronization_cycles',
             quality_remained_fresh, freshness_samples)
        if not quality_remained_fresh:
            raise RuntimeError('Healthy remote quality expired between synchronization cycles')
        source_links = source.command('route', 'links')
        parallel = [row for row in source_links if row['peer_id'] == int(peers['wide'])]
        gate('all_unused_parallel_connections_are_observable', len(parallel) >= 2
             and len({row['connection_id'] for row in parallel}) == len(parallel), parallel)
        probe_link(source, narrow, 'udp')
        probe_link(narrow, destination, 'udp')
        probe_link(source, wide, 'tcp')
        probe_link(wide, destination, 'udp')
        selected = wait_until(lambda: (decision('video') or {}).get('next_hop_peer_id') == int(peers['wide']), 10)
        gate('video_prefers_measured_capacity_over_lower_latency', selected, decision('video'))
        video_path = path_witness('video', wide)
        gate('video_capacity_decision_controls_packets', video_path['passed'], video_path)
        upgraded = probe_link(source, wide, 'udp')
        gate('previously_unmeasured_parallel_capacity_is_applied', wait_until(lambda:
            (decision('video') or {}).get('connection_id') == upgraded['connection_id'], 10), decision('video'))
        before_parallel = {row['connection_id']: row['tx_bytes'] for row in source.command('route', 'links')}
        parallel_path = path_witness('video', wide)
        after_parallel = {row['connection_id']: row['tx_bytes'] for row in source.command('route', 'links')}
        increments = {identifier: after_parallel.get(identifier, count) - count for identifier, count in before_parallel.items()}
        selected_bytes = increments.get(upgraded['connection_id'], 0)
        other_parallel = sum(increments.get(row['connection_id'], 0) for row in parallel if row['connection_id'] != upgraded['connection_id'])
        gate('parallel_capacity_choice_carries_actual_video', parallel_path['passed'] and selected_bytes > 2 * other_parallel,
             {'connection_tx_bytes': increments, 'path': parallel_path})
        selected = (decision('control') or {}).get('next_hop_peer_id') == int(peers['narrow'])
        control_path = path_witness('control', narrow)
        gate('control_prefers_low_latency_while_video_prefers_capacity', selected and control_path['passed'],
             {'decision': decision('control'), 'path': control_path})
        trust[narrow.stable_node_id]['allow_relay'] = False
        role_changed_at = time.monotonic()
        publish_trust(nodes, trust)
        endpoint_only = wait_until(lambda: (decision('control') or {}).get('next_hop_peer_id') == int(peers['wide']), 2)
        gate('verified_endpoint_only_grant_removes_transit_immediately', endpoint_only and source.ping(narrow),
             {'selection_seconds': time.monotonic() - role_changed_at, 'decision': decision('control')})
        endpoint_path = path_witness('control', wide)
        gate('endpoint_only_peer_is_not_used_for_actual_transit', endpoint_path['passed'], endpoint_path)
        trust[narrow.stable_node_id]['allow_relay'] = True
        publish_trust(nodes, trust)
        grant_restore_started = time.monotonic()
        grant_restored = wait_until(lambda: (decision('control') or {}).get('next_hop_peer_id') == int(peers['narrow']), 30)
        gate('restored_verified_relay_grant_recovers_healthy_path', grant_restored,
             {'selection_seconds': time.monotonic() - grant_restore_started, 'decision': decision('control')})
        if not grant_restored:
            report['grant_restore_diagnostics'] = {node.name: {
                'local_links': node.command('route', 'links'),
                'topology_dump': namespace_command(node.name, ['/engine/easytier-cli', 'route', 'dump']).stdout[-100000:]
            } for node in nodes}
            raise RuntimeError('restored relay grant did not become a healthy eligible path')
        # Impair an actual physical TCP underlay and observe fresh quality, not injected planner input.
        namespace_command(narrow.name, ['tc', 'qdisc', 'add', 'dev', f'peer{narrow.number}', 'parent', '1:1',
                                       'handle', '10:', 'netem', 'loss', '35%', 'delay', '1ms', 'limit', '128'])
        loss_windows = []
        def observed_loss():
            rows = source.command('route', 'links')
            row = next((row for row in rows if row['peer_id'] == int(peers['narrow'])), None)
            if row:
                loss_windows.append(row)
            return row is not None and row['quality_fresh'] and (row['loss_fraction'] or 0) > 0
        gate('physical_loss_is_collected_from_live_session', wait_until(observed_loss, 15), loss_windows)
        healthy = wait_until(lambda: (decision('control') or {}).get('next_hop_peer_id') == int(peers['wide']), 12)
        gate('control_avoids_impaired_physical_path', healthy, decision('control'))
        loss_path = path_witness('control', wide)
        gate('loss_aware_decision_controls_actual_packets', loss_path['passed'], loss_path)
        namespace_command(narrow.name, ['tc', 'qdisc', 'replace', 'dev', f'peer{narrow.number}', 'parent', '1:1',
                                       'handle', '10:', 'netem', 'delay', '1ms', 'limit', '128'])
        # Clear residual TCP loss backoff with a clean fixture relay reconnection.
        narrow.stop()
        narrow.start()
        if not wait_until(narrow.ready, 10):
            raise RuntimeError('recovered relay management endpoint did not start')
        peers['narrow'] = next(row['id'] for row in narrow.command('peer') if row['cost'] == 'Local')
        restored_at = time.monotonic()
        restored = wait_until(lambda: (decision('control') or {}).get('next_hop_peer_id') == int(peers['narrow']), 15)
        hold = time.monotonic() - restored_at
        gate('healthy_improvement_waits_five_seconds', restored and ((decision('control') or {}).get('improvement_hold_ms') or 0) >= 5000,
             {'selection_after_recovery_seconds': hold, 'decision': decision('control')})
        if not restored:
            report['recovery_diagnostics'] = {node.name: {
                'local_links': node.command('route', 'links'),
                'topology_dump': namespace_command(node.name, ['/engine/easytier-cli', 'route', 'dump']).stdout[-100000:]
            } for node in nodes}
            if os.environ.get('AVIAN_DIAGNOSE_RECOVERY') == '1':
                raise RuntimeError('recovery diagnostic completed before remaining fixture stages')
        # Similar low-noise delays should not churn a healthy current route.
        namespace_command(narrow.name, ['tc', 'qdisc', 'replace', 'dev', f'peer{narrow.number}', 'parent', '1:1',
                                       'handle', '10:', 'netem', 'delay', '20ms', '100us', 'limit', '128'])
        noise_selections = []
        for _ in range(8):
            time.sleep(1)
            noise_selections.append(decision('control'))
        gate('small_physical_delay_noise_preserves_current_route', all(
            row and row['next_hop_peer_id'] == int(peers['narrow']) for row in noise_selections), noise_selections)
        namespace_command(narrow.name, ['tc', 'qdisc', 'replace', 'dev', f'peer{narrow.number}', 'parent', '1:1',
                                       'handle', '10:', 'netem', 'delay', '1ms', 'limit', '128'])
        gate('expired_capacity_returns_to_degraded_healthy_fallback', wait_until(lambda:
            (decision('video') or {}).get('route', {}).get('bottleneck_capacity_bps') is None
            and (decision('video') or {}).get('next_hop_peer_id') == int(peers['narrow']), 40), decision('video'))
        stale_path = path_witness('video', narrow)
        gate('stale_capacity_fallback_controls_actual_video', stale_path['passed'], stale_path)
        # A missing policy file is a live unknown-cost transition and must not retain authorization.
        Path('/state/source-policy.json').unlink()
        time.sleep(0.3)
        before_denied = json.loads(received_bytes_path.read_text())['8000']
        denied = worker(source, 'video', destination.address, '--duration', '1', '--offered-bps', '1000000',
                        '--output', '/state/denied-video.json')
        workers.append(denied)
        denied.wait(timeout=3)
        after_denied = json.loads(received_bytes_path.read_text())['8000']
        gate('missing_live_cost_policy_blocks_actual_video', before_denied == after_denied,
             {'before': before_denied, 'after': after_denied})
        gate('missing_live_cost_policy_removes_media_route', wait_until(lambda: decision('video') is None, 3))
        control_log = Path('/state/unknown-cost-control.jsonl')
        control = worker(source, 'control', destination.address, '--duration', '1', '--output', str(control_log))
        workers.append(control)
        control.wait(timeout=4)
        gate('control_survives_unknown_cost_policy', control.returncode == 0 and any(
            'received' in json.loads(line) for line in control_log.read_text().splitlines()))
        source.policy(metered_addresses=[])
        gate('restored_policy_recalculates_actual_route', wait_until(lambda: decision('video') is not None, 5), decision('video'))
        # Explicit metering restricts the egress immediately; no announcement can authorize it.
        source.policy(metered_addresses=[source.physical_address])
        gate('live_metering_prohibits_video', wait_until(lambda: decision('video') is None, 3))
        probe = namespace_command(source.name, ['/engine/easytier-cli', '-o', 'json', 'route', 'probe',
            '--peer-id', peers['wide']], check=False)
        gate('metered_capacity_probe_rejected', probe.returncode != 0, probe.stderr[-300:])
        source.policy(metered_addresses=[])
        wait_until(lambda: decision('video') is not None, 5)
        # Revoke the measured active relay; a failed or prohibited path bypasses improvement hold.
        active = (decision('control') or {}).get('next_hop_peer_id') == int(peers['narrow'])
        revocation_path = Path('/state/revocation-control.jsonl')
        revocation_control = worker(source, 'control', destination.address, '--duration', '6', '--output', str(revocation_path))
        workers.append(revocation_control)
        if not wait_until(lambda: revocation_path.exists() and len(revocation_path.read_text().splitlines()) >= 3, 3):
            raise RuntimeError('control baseline missing before relay revocation')
        trust.pop(narrow.stable_node_id)
        revoked_at = time.monotonic()
        publish_trust(nodes, trust)
        changed = wait_until(lambda: (decision('control') or {}).get('next_hop_peer_id') == int(peers['wide']), 2)
        changed_after = time.monotonic() - revoked_at
        revocation_control.wait(timeout=8)
        records = [json.loads(line) for line in revocation_path.read_text().splitlines()]
        deliveries = [record['received'] for record in records if 'received' in record]
        gaps = [right-left for left, right in zip(deliveries, deliveries[1:])]
        completed = any(record.get('completed') for record in records)
        gate('relay_revocation_bypasses_healthy_improvement_hold', active and changed and changed_after <= 2
             and completed and bool(gaps) and max(gaps) <= 2,
             {'route_change_seconds': changed_after, 'maximum_control_gap_seconds': max(gaps) if gaps else None})
        revoked_path = path_witness('control', wide)
        gate('revoked_relay_replaced_on_actual_packet_path', revoked_path['passed'], revoked_path)
        endpoint_log = Path('/state/revoked-endpoint-control.jsonl')
        endpoint_control = worker(source, 'control', destination.address, '--duration', '4', '--output', str(endpoint_log))
        workers.append(endpoint_control)
        if not wait_until(lambda: endpoint_log.exists() and len(endpoint_log.read_text().splitlines()) >= 3, 3):
            raise RuntimeError('endpoint revocation baseline missing')
        trust.pop(destination.stable_node_id)
        endpoint_revoked_at = time.monotonic()
        publish_trust(nodes, trust)
        endpoint_removed = wait_until(lambda: decision('control') is None and decision('video') is None, 2)
        endpoint_control.wait(timeout=7)
        endpoint_records = [json.loads(line) for line in endpoint_log.read_text().splitlines()]
        late = [record for record in endpoint_records if record.get('received', 0) > endpoint_revoked_at + 2]
        complete = any(record.get('completed') and record.get('total_sent', 0) >= 64 for record in endpoint_records)
        gate('revoked_endpoint_is_removed_from_actual_decisions_and_packets', endpoint_removed and complete and not late,
             {'late_deliveries': len(late), 'generator_completed': complete})
        report['status'] = 'pass' if all(item['status'] == 'pass' for item in report['gates']) else 'fail'
    except Exception as error:
        report['status'] = 'error'
        report['error'] = str(error)
        report['diagnostics'] = {}
        for node in nodes:
            path = Path(f'/state/{node.name}.log')
            report['diagnostics'][node.name] = path.read_text()[-2000:] if path.exists() else ''
    finally:
        for process in workers:
            stop_worker(process)
        for node in nodes:
            node.stop()
        report['binary_sha256'] = hashlib.sha256(Path('/engine/easytier-core').read_bytes()).hexdigest()
        Path('/results/runtime.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps(report, indent=2))
    return 0 if report['status'] == 'pass' else 2


if __name__ == '__main__':
    raise SystemExit(main())
