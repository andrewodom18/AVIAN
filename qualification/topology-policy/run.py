"""Test remote policy-aware routing inside isolated Linux network namespaces."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--engine-directory', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    arguments = parser.parse_args()
    qualification = Path(__file__).resolve().parents[1]
    engine = arguments.engine_directory.resolve()
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    provenance = json.loads((engine / 'build.json').read_text())
    for name in ['easytier-core', 'easytier-cli']:
        actual = hashlib.sha256((engine / name).read_bytes()).hexdigest()
        if provenance.get('binary_sha256', {}).get(name) != actual:
            parser.error(f'Candidate hash mismatch: {name}')
    (output / 'provenance.json').write_text(json.dumps(provenance, indent=2) + '\n')
    subprocess.run(['docker', 'build', '-t', 'avian-qualification:local', str(qualification)], check=True)
    result = subprocess.run([
        'docker', 'run', '--rm', '--network', 'none', '--privileged',
        '--mount', f'type=bind,source={qualification},target=/qualification,readonly',
        '--mount', f'type=bind,source={engine},target=/engine,readonly',
        '--mount', f'type=bind,source={output},target=/results',
        '--tmpfs', '/state:mode=0700', '--entrypoint', 'python3',
        'avian-qualification:local', '/qualification/topology-policy/runtime.py',
    ])
    return result.returncode


if __name__ == '__main__':
    raise SystemExit(main())
