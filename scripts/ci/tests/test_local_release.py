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

    def test_windows_receipt_requires_and_authenticates_portable_download(self):
        portable = self.directory / 'Bokheim-Windows-x86_64-Portable.zip'
        checksum = portable.with_suffix('.zip.sha256')
        for name in artifacts.FILES['windows']:
            (self.directory / name).write_bytes(b'package')
        portable.unlink()
        with self.assertRaisesRegex(ValueError, 'Missing regular artifact'):
            artifacts.record('windows', self.directory, self.sha, 'owner/repo', 123)
        portable.write_bytes(b'portable runtime')
        receipt = artifacts.record('windows', self.directory, self.sha, 'owner/repo', 123)
        self.assertEqual(receipt['protocol_version'], 2)
        self.assertIn(portable.name, receipt['files'])
        self.assertIn(checksum.name, receipt['files'])
        artifacts.validate(receipt, 'windows', self.sha, 'owner/repo', self.directory)
        portable.write_bytes(b'replaced runtime')
        with self.assertRaisesRegex(ValueError, 'Artifact changed after verification'):
            artifacts.validate(receipt, 'windows', self.sha, 'owner/repo', self.directory)

    def test_linux_receipt_requires_installer_and_accepts_legacy_releases(self):
        self.assertEqual(self.receipt['protocol_version'], 2)
        artifacts.validate(self.receipt, 'linux', self.sha, 'owner/repo', self.directory)
        (self.directory / 'install.sh').write_bytes(b'changed installer')
        with self.assertRaisesRegex(ValueError, 'Artifact changed after verification: install.sh'):
            artifacts.validate(self.receipt, 'linux', self.sha, 'owner/repo', self.directory)
        del self.receipt['files']['install.sh']
        with self.assertRaisesRegex(ValueError, 'Incomplete package receipt'):
            artifacts.validate(self.receipt, 'linux', self.sha, 'owner/repo', self.directory)
        self.receipt['protocol_version'] = 1
        artifacts.validate(self.receipt, 'linux', self.sha, 'owner/repo', self.directory)

    def test_new_windows_receipts_require_portable_files_and_legacy_receipts_remain_valid(self):
        for name in artifacts.FILES['windows']:
            (self.directory / name).write_bytes(b'package')
        receipt = artifacts.record('windows', self.directory, self.sha, 'owner/repo', 123)
        del receipt['files']['Bokheim-Windows-x86_64-Portable.zip']
        with self.assertRaisesRegex(ValueError, 'Incomplete package receipt'):
            artifacts.validate(receipt, 'windows', self.sha, 'owner/repo', self.directory)
        del receipt['files']['Bokheim-Windows-x86_64-Portable.zip.sha256']
        with self.assertRaisesRegex(ValueError, 'Incomplete package receipt'):
            artifacts.validate(receipt, 'windows', self.sha, 'owner/repo', self.directory)
        receipt['protocol_version'] = 1
        del receipt['files']['Bokheim-Windows-x86_64-Standalone.exe']
        del receipt['files']['Bokheim-Windows-x86_64-Standalone.exe.sha256']
        artifacts.validate(receipt, 'windows', self.sha, 'owner/repo', self.directory)
        del receipt['files']['Bokheim-Windows-x86_64-Update.zip']
        with self.assertRaisesRegex(ValueError, 'Incomplete package receipt'):
            artifacts.validate(receipt, 'windows', self.sha, 'owner/repo', self.directory)

    def test_windows_receipt_requires_single_file_executable(self):
        for name in artifacts.FILES['windows']:
            (self.directory / name).write_bytes(b'package')
        executable = self.directory / 'Bokheim-Windows-x86_64-Standalone.exe'
        executable.unlink()
        with self.assertRaisesRegex(ValueError, 'Missing regular artifact'):
            artifacts.record('windows', self.directory, self.sha, 'owner/repo', 123)
        executable.write_bytes(b'MZ standalone')
        receipt = artifacts.record('windows', self.directory, self.sha, 'owner/repo', 123)
        del receipt['files'][executable.name]
        with self.assertRaisesRegex(ValueError, 'Incomplete package receipt'):
            artifacts.validate(receipt, 'windows', self.sha, 'owner/repo', self.directory)


if __name__ == '__main__':
    unittest.main()
