import hashlib
import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location('windows_standalone', Path(__file__).resolve().parents[3] / 'apps/desktop-gpui/windows/package-standalone.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class StandalonePackageTests(unittest.TestCase):
    def test_one_executable_contains_the_complete_verified_zip_and_a_checksum(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            launcher = root / 'launcher.exe'
            launcher.write_bytes(b'MZ native launcher')
            archive = root / 'Portable.zip'
            with zipfile.ZipFile(archive, 'w') as payload:
                for name in module.REQUIRED:
                    payload.writestr(name, b'runtime resource')
            output = root / 'Single app/Standalone.exe'
            module.package(launcher, archive, output)
            contents = output.read_bytes()
            self.assertTrue(contents.startswith(launcher.read_bytes()))
            self.assertEqual(contents[-56:-40], module.MAGIC)
            length = struct.unpack('<Q', contents[-40:-32])[0]
            embedded = contents[-56 - length:-56]
            self.assertEqual(embedded, archive.read_bytes())
            self.assertEqual(contents[-32:], hashlib.sha256(embedded).digest())
            self.assertEqual(output.with_suffix('.exe.sha256').read_text(),
                             f'{hashlib.sha256(contents).hexdigest()}  Standalone.exe\n')

    def test_incomplete_runtime_is_rejected_before_creating_an_executable(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            launcher = root / 'launcher.exe'
            launcher.write_bytes(b'MZ launcher')
            archive = root / 'Incomplete.zip'
            with zipfile.ZipFile(archive, 'w') as payload:
                payload.writestr('Bokheim.exe', b'MZ app')
            output = root / 'Standalone.exe'
            with self.assertRaisesRegex(ValueError, 'missing required runtime files'):
                module.package(launcher, archive, output)
            self.assertFalse(output.exists())
