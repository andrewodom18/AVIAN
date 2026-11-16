"""Run signed revocation distribution through an isolated mesh."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def hashes(directory, names):
    return {name: hashlib.sha256((directory / name).read_bytes()).hexdigest() for name in names}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--engine-directory', type=Path, required=True)
    parser.add_argument('--service-directory', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--metered-sync', action='store_true')
    arguments = parser.parse_args()
    qualification = Path(__file__).resolve().parents[1]
    engine = arguments.engine_directory.resolve()
    services = arguments.service_directory.resolve()
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    provenance = {
        'engine_binary_sha256': hashes(engine, ['easytier-core', 'easytier-cli']),
        'service_binary_sha256': hashes(services, ['avianctl', 'avian-revocationd']),
    }
    if (engine / 'build.json').exists():
        build = json.loads((engine / 'build.json').read_text())
        if build['binary_sha256'] != provenance['engine_binary_sha256']:
            parser.error('Engine binaries differ from build provenance')
        provenance['engine_build'] = build
    (output / 'provenance.json').write_text(json.dumps(provenance, indent=2) + '\n')
    subprocess.run(['docker', 'build', '-t', 'avian-qualification:local', str(qualification)], check=True)
    return subprocess.run([
        'docker', 'run', '--rm', '--network', 'none', '--privileged',
        '--mount', f'type=bind,source={qualification},target=/qualification,readonly',
        '--mount', f'type=bind,source={engine},target=/engine,readonly',
        '--mount', f'type=bind,source={services},target=/services,readonly',
        '--mount', f'type=bind,source={output},target=/results',
        '--tmpfs', '/state:mode=0700', '--entrypoint', 'python3',
        'avian-qualification:local', '/qualification/revocation-distribution/runtime.py',
        *(['--metered-sync'] if arguments.metered_sync else []),
    ]).returncode


if __name__ == '__main__':
    raise SystemExit(main())
