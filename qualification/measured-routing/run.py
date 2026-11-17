"""Run measured forwarding checks in a disposable Linux network namespace fixture."""
import argparse
import hashlib
import json
import math
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--engine-directory', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--diagnose-recovery', action='store_true')
    parser.add_argument('--scenario', choices=['measured', 'growth', 'blackhole', 'relay-stop'], default='measured')
    parser.add_argument('--growth-seconds', type=float, default=30)
    parser.add_argument('--memory-budget-mib', type=int, default=512)
    parser.add_argument('--load-seconds', type=float, default=10)
    parser.add_argument('--bottleneck-bps', type=int, default=2000000)
    arguments = parser.parse_args()
    if any(not math.isfinite(value) or value <= 0 for value in [arguments.growth_seconds, arguments.memory_budget_mib,
                                                            arguments.load_seconds, arguments.bottleneck_bps]):
        parser.error('Budgets must be finite positive values')
    qualification = Path(__file__).resolve().parents[1]
    engine = arguments.engine_directory.resolve()
    build = json.loads((engine / 'build.json').read_text())
    recorded_hashes = build.get('binary_sha256', {})
    if not {'easytier-core', 'easytier-cli'}.issubset(recorded_hashes):
        parser.error('Build provenance must include the core and CLI binaries')
    if any(Path(name).name != name or (engine / name).is_symlink()
           or not (engine / name).is_file() for name in recorded_hashes):
        parser.error('Build provenance contains an invalid binary path')
    hashes = {name: hashlib.sha256((engine / name).read_bytes()).hexdigest()
              for name in recorded_hashes}
    if hashes != recorded_hashes or build.get('measured_routing') is not True:
        parser.error('Expected measured routing build provenance and matching binary hashes')
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    (output / 'provenance.json').write_text(json.dumps(build, indent=2) + '\n')
    subprocess.run(['docker', 'build', '-t', 'avian-qualification:local', str(qualification)], check=True)
    command = ['docker', 'run', '--rm', '--network', 'none', '--privileged',
        '--mount', f'type=bind,source={qualification},target=/qualification,readonly',
        '--mount', f'type=bind,source={engine},target=/engine,readonly',
        '--mount', f'type=bind,source={output},target=/results', '--tmpfs', '/state:mode=0700',
        '--entrypoint', 'python3', 'avian-qualification:local',
        '/qualification/measured-routing/' + ('runtime.py' if arguments.scenario == 'measured' else 'regressions.py')]
    if arguments.diagnose_recovery:
        command[2:2] = ['--env', 'AVIAN_DIAGNOSE_RECOVERY=1']
    if arguments.scenario != 'measured':
        command.extend(['--scenario', arguments.scenario, '--bottleneck-bps', str(arguments.bottleneck_bps)])
        if arguments.scenario == 'growth':
            command.extend(['--growth-seconds', str(arguments.growth_seconds), '--memory-budget-mib', str(arguments.memory_budget_mib),
                            '--load-seconds', str(arguments.load_seconds)])
    return subprocess.run(command).returncode


if __name__ == '__main__':
    raise SystemExit(main())
