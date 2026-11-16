"""Run live-revocation traffic tests in an isolated Linux container."""

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
    qualification_directory = Path(__file__).resolve().parents[1]
    engine_directory = arguments.engine_directory.resolve()
    output_directory = arguments.output.resolve()
    output_directory.mkdir(parents=True, exist_ok=True)
    binary_hashes = {
        name: hashlib.sha256((engine_directory / name).read_bytes()).hexdigest()
        for name in ['easytier-core', 'easytier-cli']
    }
    provenance_path = engine_directory / 'build.json'
    provenance = None
    if provenance_path.exists():
        provenance = json.loads(provenance_path.read_text())
        if provenance.get('binary_sha256') != binary_hashes:
            raise SystemExit('Candidate binary hashes do not match recorded build provenance')
    command = [
        'docker', 'run', '--rm', '--network', 'none', '--privileged',
        '--mount', f'type=bind,source={qualification_directory},target=/qualification,readonly',
        '--mount', f'type=bind,source={engine_directory},target=/engine,readonly',
        '--mount', f'type=bind,source={output_directory},target=/results',
        '--tmpfs', '/state:mode=0700', '--entrypoint', 'python3',
        'avian-qualification:local', '/qualification/revocation/runtime.py',
    ]
    result = subprocess.run(command, check=False)
    report_path = output_directory / 'revocation.json'
    if not report_path.exists():
        raise SystemExit('Runtime did not produce a report')
    report = json.loads(report_path.read_text())
    report['binary_sha256'] = binary_hashes
    report['build_provenance'] = provenance
    report['provenance_status'] = 'verified' if provenance else 'unknown'
    report['scope'] = (
        'Local verified-cache enforcement; distributed revocation delivery remains separate.'
    )
    report_path.write_text(json.dumps(report, indent=2) + '\n')
    raise SystemExit(result.returncode)


if __name__ == '__main__':
    main()
