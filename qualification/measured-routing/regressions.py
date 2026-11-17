"""Run the existing real load and fault fixtures with measured routing enabled."""
import argparse
import importlib.util
import json
from pathlib import Path
import sys

specification = importlib.util.spec_from_file_location('measured_fixture', '/qualification/measured-routing/runtime.py')
fixture = importlib.util.module_from_spec(specification)
sys.modules[specification.name] = fixture
specification.loader.exec_module(fixture)
MeasuredNode = fixture.MeasuredNode
sys.path.insert(0, '/qualification')
from load import runtime as growth


class RegressionNode(MeasuredNode):
    observed_decisions = {}

    def start(self):
        self.policy(metered_addresses=[])
        super().start()

    def command(self, *arguments):
        result = super().command(*arguments)
        if arguments == ('peer',):
            self.observed_decisions[self.name] = len(super().command('route', 'measured'))
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--scenario', choices=['growth', 'blackhole', 'relay-stop'], required=True)
    arguments, remaining = parser.parse_known_args()
    growth.LoadNode = RegressionNode
    if arguments.scenario == 'growth':
        sys.argv = ['load/runtime.py', '--priority-forwarding', *remaining]
        status = growth.main()
    else:
        from load import failover
        failover.LoadNode = RegressionNode
        sys.argv = ['load/failover.py', '--priority-forwarding', '--fault', arguments.scenario, *remaining]
        status = failover.main()
    path = Path('/results/runtime.json')
    report = json.loads(path.read_text())
    report['measured_routing'] = True
    report['observed_class_decision_counts'] = RegressionNode.observed_decisions
    if not any(RegressionNode.observed_decisions.values()):
        report['status'] = 'error'
        report['error'] = 'No actual measured class decisions observed in this fixture'
        status = 2
    path.write_text(json.dumps(report, indent=2) + '\n')
    return status


if __name__ == '__main__':
    raise SystemExit(main())
