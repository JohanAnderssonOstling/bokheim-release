import importlib.util
from pathlib import Path
import unittest


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).parents[1] / f'{name}.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


gate = load('require-shared-tests')
suite = load('shared-tests')


class GateTests(unittest.TestCase):
    def test_missing_or_other_context_cannot_authorize_release(self):
        for statuses in [[], [{'context': 'other', 'id': 8, 'state': 'success'}]]:
            with self.assertRaises(RuntimeError):
                gate.require_success(statuses)

    def test_new_failure_or_pending_overrides_old_success(self):
        for state in ['pending', 'failure', 'error']:
            with self.assertRaises(RuntimeError):
                gate.require_success([
                    {'context': gate.CONTEXT, 'id': 5, 'state': state},
                    {'context': gate.CONTEXT, 'id': 4, 'state': 'success'},
                ])

    def test_latest_success_is_accepted(self):
        gate.require_success([
            {'context': gate.CONTEXT, 'id': 5, 'state': 'success'},
            {'context': gate.CONTEXT, 'id': 4, 'state': 'failure'},
        ])

    def test_shared_rust_commands_use_release_without_requiring_lockfile(self):
        for command in suite.commands():
            if command[0] == 'cargo':
                self.assertIn('--release', command)
                self.assertNotIn('--locked', command)
