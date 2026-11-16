"""Run the traffic-policy candidate in an isolated Linux container."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--engine-directory', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    arguments = parser.parse_args()
    directory = Path(__file__).resolve().parent.parent
    engine = arguments.engine_directory.resolve()
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    for name in ['easytier-core', 'easytier-cli']:
        if not (engine / name).is_file():
            parser.error(f'Missing candidate binary: {name}')
    provenance_path = engine / 'build.json'
    if not provenance_path.is_file():
        parser.error('Candidate must include build.json provenance')
    provenance = json.loads(provenance_path.read_text())
    for name in ['easytier-core', 'easytier-cli']:
        actual = hashlib.sha256((engine / name).read_bytes()).hexdigest()
        if provenance.get('binary_sha256', {}).get(name) != actual:
            parser.error(f'Candidate hash mismatch: {name}')
    (output / 'provenance.json').write_text(json.dumps(provenance, indent=2) + '\n')
    subprocess.run(['docker', 'build', '-t', 'avian-qualification:local', str(directory)], check=True)
    completed = subprocess.run([
        'docker', 'run', '--rm', '--network', 'none', '--privileged',
        '--mount', f'type=bind,source={directory},target=/qualification,readonly',
        '--mount', f'type=bind,source={engine},target=/engine,readonly',
        '--mount', f'type=bind,source={output},target=/results',
        '--tmpfs', '/state:mode=0700', '--entrypoint', 'python3',
        'avian-qualification:local', '/qualification/traffic-policy/runtime.py',
    ])
    return completed.returncode


if __name__ == '__main__':
    raise SystemExit(main())
