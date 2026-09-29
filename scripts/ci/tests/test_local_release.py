import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location('artifacts', ROOT / 'scripts/ci/release_artifacts.py')
artifacts = importlib.util.module_from_spec(spec)
spec.loader.exec_module(artifacts)


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.sha = 'a' * 40
        for name in artifacts.FILES['linux']:
            (self.directory / name).write_bytes(b'package')
        self.receipt = artifacts.record('linux', self.directory, self.sha, 'owner/repo')

    def test_receipt_rejects_mixed_commits_and_changed_payloads(self):
        artifacts.validate(self.receipt, 'linux', self.sha, 'owner/repo', self.directory)
        with self.assertRaisesRegex(ValueError, 'source mismatch'):
            artifacts.validate(self.receipt, 'linux', 'b' * 40, 'owner/repo', self.directory)
        (self.directory / artifacts.FILES['linux'][0]).write_bytes(b'replaced')
        with self.assertRaisesRegex(ValueError, 'changed after verification'):
            artifacts.validate(self.receipt, 'linux', self.sha, 'owner/repo', self.directory)

    def test_existing_older_tag_accepts_build_and_retains_actual_source(self):
        info = self.directory / 'update-info-linux-x86_64-appimage.json'
        info.write_text(json.dumps({'application': '1.2.3', 'target': 'linux-x86_64-appimage'}))
        artifacts.record('linux', self.directory, self.sha, 'owner/repo')
        calls = []
        def command(*args):
            calls.append(args)
            if args[:2] == ('gh', 'api'):
                if '/git/ref/tags/' in args[-1]:
                    return json.dumps({'object': {'sha': 'b' * 40}})
                if '/releases?' in args[-1]:
                    return '[[]]'
                self.fail(f'Unexpected API request: {args}')
            if args[:3] == ('gh', 'release', 'view'):
                return json.dumps({'isDraft': True, 'assets': []})
            if args[:3] == ('gh', 'release', 'create'):
                self.assertIn('--verify-tag', args)
                self.assertNotIn('--target', args)
            if args[:3] == ('gh', 'release', 'upload'):
                self.assertEqual(Path(args[-1]).read_text().strip(), self.sha)
            return ''
        with patch.object(artifacts, 'command', command):
            artifacts.upload('linux', self.directory, 'owner/repo', 'v1.2.3')
        self.assertTrue(any(c[:3] == ('gh', 'release', 'upload') for c in calls))

    def test_missing_existing_tag_still_blocks_upload(self):
        calls = []
        def command(*args):
            calls.append(args)
            if '/git/ref/tags/' in args[-1]:
                raise RuntimeError('tag missing')
            return ''
        with patch.object(artifacts, 'command', command), self.assertRaisesRegex(RuntimeError, 'tag missing'):
            artifacts.upload('linux', self.directory, 'owner/repo', 'v1.2.3')
        self.assertFalse(any('create' in c or 'upload' in c for c in calls))

    def test_web_receipt_detects_changed_bundle(self):
        (self.directory / 'index.html').write_text('page')
        (self.directory / 'app.wasm').write_bytes(b'wasm')
        receipt = artifacts.record('web', self.directory, self.sha, 'owner/repo')
        (self.directory / 'app.wasm').write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'changed after verification'):
            artifacts.validate(receipt, 'web', self.sha, 'owner/repo', self.directory)


if __name__ == '__main__':
    unittest.main()
