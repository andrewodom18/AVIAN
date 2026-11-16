"""Exercise established TCP and UDP traffic while local trust changes."""
import base64
import copy
import json
import os
from pathlib import Path
import secrets
import subprocess
import sys
import time
sys.path.insert(0, '/qualification')
from runtime import Node, STATE, run, wait_until
ECHO = "import socket,threading\n\ndef connection(c):\n try:\n  while True:\n   b=c.recv(100)\n   if not b:break\n   c.sendall(b)\n except OSError:pass\n c.close()\ndef tcp():\n s=socket.socket();s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);s.bind(('0.0.0.0',9000));s.listen()\n while True:\n  c,_=s.accept();threading.Thread(target=connection,args=(c,),daemon=True).start()\nthreading.Thread(target=tcp,daemon=True).start()\ns=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind(('0.0.0.0',9001))\nwhile True:\n b,a=s.recvfrom(100);s.sendto(b,a)\n"
CLIENT = "import socket,time,json,sys\ns=socket.create_connection((sys.argv[1],9000),timeout=2);s.settimeout(.15)\nu=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);u.settimeout(.15)\nend=time.monotonic()+8\nwith open(sys.argv[2],'w',buffering=1) as f:\n while time.monotonic()<end:\n  result={'time':time.time(),'tcp':False,'udp':False}\n  try:s.sendall(b't');result['tcp']=s.recv(1)==b't'\n  except OSError:pass\n  try:u.sendto(b'u',(sys.argv[1],9001));result['udp']=u.recvfrom(1)[0]==b'u'\n  except OSError:pass\n  f.write(json.dumps(result)+'\\n');time.sleep(.05)\n"

def publish(nodes, trust):
    for node in nodes:
        cache_path = STATE / (node.name + '-credentials.json')
        temporary_path = cache_path.with_suffix('.tmp')
        temporary_path.write_text(json.dumps({'format': 'avian-verified-membership-v1', 'credentials': trust}))
        temporary_path.chmod(384)
        temporary_path.replace(cache_path)

def main():
    report = {'gates': [], 'qualified_for_cutover': False}
    nodes = [Node('relay', 2), Node('source', 3), Node('destination', 4)]
    processes = []
    try:
        run(['ip', 'link', 'add', 'underlay', 'type', 'bridge'])
        run(['ip', 'link', 'set', 'underlay', 'up'])
        trust = {}
        for node in nodes:
            node.create()
            key = base64.b64encode(secrets.token_bytes(32)).decode()
            node.configure(credential=key, peers=[] if node.name == 'relay' else [nodes[0]])
            import tomllib
            public = tomllib.loads((STATE / (node.name + '.toml')).read_text())['secure_mode']['local_public_key']
            trust[node.name] = {'pubkey': public, 'groups': [], 'allow_relay': True, 'allowed_proxy_cidrs': [], 'overlay_addresses': [node.address], 'reusable': False, 'created_at_unix': int(time.time()), 'expiry_unix': int(time.time()) + 300}
            node.require_verified_membership = True
        publish(nodes, trust)
        for node in nodes:
            node.start()
        relay, source, destination = nodes
        if not wait_until(lambda: source.ping(destination), 20):
            raise RuntimeError('baseline traffic unavailable')
        server = STATE / 'echo.py'
        server.write_text(ECHO)
        processes.append(subprocess.Popen(['ip', 'netns', 'exec', 'destination', 'python3', str(server)]))
        time.sleep(0.3)
        client = STATE / 'client.py'
        client.write_text(CLIENT)
        for name, changed in [('removed_member', {device_name: entry for device_name, entry in trust.items() if device_name != 'destination'}), ('relay_only_revocation', {device_name: entry for device_name, entry in trust.items() if device_name != 'destination'}), ('source_only_revocation', {device_name: entry for device_name, entry in trust.items() if device_name != 'destination'}), ('empty_snapshot', {}), ('malformed_cache', {}), ('lease_refresh', trust), ('expired_lease', {device_name: dict(entry, expiry_unix=int(time.time()) - 1) if device_name == 'destination' else entry for device_name, entry in trust.items()})]:
            for node in nodes:
                node.stop()
            scenario_trust = copy.deepcopy(trust)
            if name in ['malformed_cache', 'lease_refresh']:
                for entry in scenario_trust.values():
                    entry['expiry_unix'] = int(time.time()) + 5
            publish(nodes, scenario_trust)
            lease_deadline = max((entry['expiry_unix'] for entry in scenario_trust.values()))
            for node in nodes:
                node.start()
            if not wait_until(lambda: source.ping(destination), 20):
                raise RuntimeError('independent scenario baseline unavailable')
            output = STATE / (name + '.jsonl')
            process = subprocess.Popen(['ip', 'netns', 'exec', 'source', 'python3', str(client), destination.address, str(output)])
            processes.append(process)
            time.sleep(1.5)
            updated = time.time()
            if name == 'malformed_cache':
                for node in nodes:
                    cache_path = STATE / (node.name + '-credentials.json')
                    temporary_path = cache_path.with_suffix('.tmp')
                    temporary_path.write_text('{invalid')
                    temporary_path.replace(cache_path)
            elif name == 'lease_refresh':
                while process.poll() is None:
                    for entry in scenario_trust.values():
                        entry['expiry_unix'] = int(time.time()) + 5
                    publish(nodes, scenario_trust)
                    time.sleep(1)
            else:
                publish([relay] if name == 'relay_only_revocation' else [source] if name == 'source_only_revocation' else nodes, changed)
            process.wait(timeout=12)
            rows = [json.loads(line) for line in output.read_text().splitlines()]
            before = [sample for sample in rows if sample['time'] < updated]
            after = [sample for sample in rows if sample['time'] > (lease_deadline + 1 if name == 'malformed_cache' else updated + 1)]
            passed = bool(before and after and any((sample['tcp'] and sample['udp'] for sample in before)) and (all((sample['tcp'] and sample['udp'] for sample in after)) if name == 'lease_refresh' else not any((sample['tcp'] or sample['udp'] for sample in after))))
            report['gates'].append({'name': name, 'status': 'pass' if passed else 'fail', 'tcp_udp_stopped_within_seconds': None if name == 'lease_refresh' else 1, 'established_tcp': True, 'sample_count': len(rows)})
            print(name, passed, flush=True)
    except Exception as error:
        report['error'] = str(error)
    finally:
        for process in processes:
            if process.poll() is None:
                process.terminate()
                process.wait()
        for node in nodes:
            node.stop()
        Path('/results/revocation.json').write_text(json.dumps(report, indent=2) + '\n')
    return 0 if len(report['gates']) == 7 and all((g['status'] == 'pass' for g in report['gates'])) else 1
if __name__ == '__main__':
    raise SystemExit(main())
