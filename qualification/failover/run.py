"""Run a namespace failover trial with exact candidate binaries."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--engine-directory', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--fast-failover', action='store_true')
    parser.add_argument('--fault', choices=['blackhole', 'relay_stop'], required=True)
    arguments = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    engine = arguments.engine_directory.resolve()
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    provenance = {'binary_sha256': {name: hashlib.sha256((engine/name).read_bytes()).hexdigest() for name in ['easytier-core', 'easytier-cli']}}
    if (engine/'build.json').exists():
        provenance['build'] = json.loads((engine/'build.json').read_text())
        if provenance['build'].get('binary_sha256') != provenance['binary_sha256']:
            raise SystemExit('Candidate binaries do not match recorded build provenance')
    (output/'provenance.json').write_text(json.dumps(provenance, indent=2)+'\n')
    subprocess.run(['docker', 'build', '-t', 'avian-qualification:local', str(root)], check=True)
    return subprocess.run(['docker', 'run', '--rm', '--network', 'none', '--privileged',
        '--mount', f'type=bind,source={root},target=/qualification,readonly',
        '--mount', f'type=bind,source={engine},target=/engine,readonly',
        '--mount', f'type=bind,source={output},target=/results',
        '--tmpfs', '/state:mode=0700', '--entrypoint', 'python3', 'avian-qualification:local',
        '/qualification/failover/runtime.py', '--fault', arguments.fault, *(['--fast-failover'] if arguments.fast_failover else [])]).returncode

if __name__ == '__main__':
    raise SystemExit(main())
