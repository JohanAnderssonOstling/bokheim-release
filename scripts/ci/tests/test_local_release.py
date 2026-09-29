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

    def test_upload_never_creates_a_tag_or_uploads_to_wrong_commit(self):
        calls = []
        def command(*args):
            calls.append(args)
            if '/commits/' in args[-1]:
                return json.dumps({'sha': 'b' * 40})
            return ''
        with patch.object(artifacts, 'command', command), self.assertRaisesRegex(ValueError, 'Tag differs'):
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
