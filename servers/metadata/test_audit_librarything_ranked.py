import importlib.util
import json
from pathlib import Path
import sqlite3
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('audit_librarything', Path(__file__).with_name('audit-librarything-ranked.py'))
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)


class NormalLookupGateTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        db = root / 'snapshot.sqlite'
        with sqlite3.connect(db) as c:
            c.executescript('CREATE TABLE edition(edition_id INTEGER,work_id INTEGER); CREATE TABLE edition_isbn(edition_id INTEGER,isbn13 INTEGER); INSERT INTO edition VALUES(1,1),(2,1); INSERT INTO edition_isbn VALUES(1,9780743477116),(2,9781538724736);')
        c.close()
        ranked = root / 'ranked.json'
        ranked.write_text(json.dumps({'popular': [{'work_id': 'OL1W', 'title': 'Example', 'ratings': 10, 'earliest_recorded_year': 2020}], 'recent': []}))
        self.args = SimpleNamespace(ranked=str(ranked), database=str(db), output=str(root / 'result.json'), key_file='/unused/key', cache='/unused/cache', binary='metadata-server')

    def report(self):
        return json.loads(Path(self.args.output).read_text())['results']['OL1W']

    def test_duplicate_work_code_on_another_isbn_prevents_every_provider_call(self):
        local = [{'requested_isbn': '9780743477116', 'matches': []}, {'requested_isbn': '9781538724736', 'matches': [{'classifications': [{'scheme': 'library_of_congress', 'notation': 'PZ7', 'evidence': [{'method': 'duplicate_work'}]}]}]}]
        with patch.object(audit.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, json.dumps(local), '')) as run:
            audit.run(self.args)
        self.assertEqual(run.call_count, 1)
        self.assertEqual(run.call_args.args[0], ['metadata-server', 'lookup-lcc', self.args.database, '9780743477116', '9781538724736'])
        self.assertEqual(self.report()['status'], 'already_resolved_locally')
        self.assertIsNone(self.report()['librarything'])

    def test_provider_is_called_only_after_a_successful_empty_local_lookup(self):
        local = [{'matches': [{'classifications': [{'scheme': 'bisac', 'notation': 'FIC000000'}]}]}]
        responses = [subprocess.CompletedProcess([], 0, json.dumps(local), ''), subprocess.CompletedProcess([], 0, '{"work_id":"123","codes":["PR2831"]}', '')]
        with patch.object(audit.subprocess, 'run', side_effect=responses) as run:
            audit.run(self.args)
        self.assertEqual([c.args[0][1] for c in run.call_args_list], ['lookup-lcc', 'librarything-lookup'])
        self.assertEqual(self.report()['status'], 'lcc_found')

    def test_refresh_accepts_ddc_without_reporting_it_as_lcc(self):
        self.args.refresh_schemes = True
        replies = [subprocess.CompletedProcess([], 0, '[{"matches":[]}]', ''), subprocess.CompletedProcess([], 0, '{"work_id":"123","codes":[],"ddc":["813.6"],"bisac":["FIC000000"],"all_schemes_checked":true}', '')]
        with patch.object(audit.subprocess, 'run', side_effect=replies) as run:
            audit.run(self.args)
        self.assertEqual(run.call_args_list[-1].args[0][1], 'librarything-refresh-schemes')
        self.assertEqual(self.report()['status'], 'other_classification_found')
        self.assertEqual(self.report()['librarything']['ddc'], ['813.6'])
        self.assertEqual(self.report()['librarything']['codes'], [])
        self.assertIn('813.6', Path(self.args.output).with_suffix('.csv').read_text())

    def test_local_failure_does_not_fall_through_to_librarything(self):
        with patch.object(audit.subprocess, 'run', return_value=subprocess.CompletedProcess([], 1, '', 'database unavailable')) as run:
            with self.assertRaisesRegex(RuntimeError, 'Local classification precheck failed'):
                audit.run(self.args)
        self.assertEqual(run.call_count, 2)
        self.assertTrue(all(c.args[0][1] == 'lookup-lcc' for c in run.call_args_list))

    def test_rate_limit_stops_without_trying_the_next_isbn(self):
        responses = [subprocess.CompletedProcess([], 0, '[{"matches":[]}]', ''), subprocess.CompletedProcess([], 1, '', 'LibraryThing HTTP 429')]
        with patch.object(audit.subprocess, 'run', side_effect=responses) as run:
            audit.run(self.args)
        self.assertEqual(run.call_count, 2)
        self.assertEqual(json.loads(Path(self.args.output).read_text())['status'], 'rate_limited')
        self.assertEqual(self.report()['status'], 'request_failed')

    def test_consecutive_errors_stop_instead_of_becoming_empty_results(self):
        self.args.max_consecutive_failures = 2
        responses = [subprocess.CompletedProcess([], 0, '[{"matches":[]}]', ''), subprocess.CompletedProcess([], 1, '', 'LibraryThing HTTP 403'), subprocess.CompletedProcess([], 1, '', 'LibraryThing HTTP 403')]
        with patch.object(audit.subprocess, 'run', side_effect=responses) as run:
            audit.run(self.args)
        self.assertEqual(run.call_count, 3)
        self.assertEqual(json.loads(Path(self.args.output).read_text())['status'], 'consecutive_provider_failures')
        self.assertEqual(self.report()['status'], 'request_failed')

    def test_interrupted_precheck_retries_locally_before_skipping_provider(self):
        code = [{"matches":[{"classifications":[{"scheme":"library_of_congress","notation":"PZ7"}]}]}]
        replies = [subprocess.CompletedProcess([], 1, '', 'interrupted'), subprocess.CompletedProcess([], 0, json.dumps(code), '')]
        with patch.object(audit.subprocess, 'run', side_effect=replies) as run, patch.object(audit.time, 'sleep'):
            audit.run(self.args)
        self.assertEqual(run.call_count, 2)
        self.assertTrue(all(c.args[0][1] == 'lookup-lcc' for c in run.call_args_list))
        self.assertEqual(self.report()['status'], 'already_resolved_locally')


if __name__ == '__main__':
    unittest.main()
