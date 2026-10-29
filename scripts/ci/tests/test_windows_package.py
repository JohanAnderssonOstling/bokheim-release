import importlib.util
import hashlib
from pathlib import Path
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location('windows_package', Path(__file__).resolve().parents[3] / 'apps/desktop-gpui/windows/package-update.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class WindowsPackageTests(unittest.TestCase):
    def test_complete_payload_is_reproducible_and_portable_checksum_is_written(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            binary = root / 'desktop-gpui.exe'
            binary.write_bytes(b'MZ executable')
            (root / 'taxonomy.sqlite3').write_bytes(b'SQLite format 3\0')
            (root / 'bokheim-opds-webview.exe').write_bytes(b'MZ helper')
            (root / 'tts/alba').mkdir(parents=True)
            (root / 'tts/alba/en_GB-alba-medium.onnx').write_bytes(b'voice')
            (root / 'tts/george').mkdir(parents=True)
            (root / 'tts/george/model.onnx').write_bytes(b'george-fp32')
            (root / 'pdfium').mkdir()
            (root / 'pdfium/pdfium.dll').write_bytes(b'MZ library')
            icon = root / 'icon.ico'
            icon.write_bytes(b'icon')
            first, second = root / 'first.zip', root / 'second.zip'
            portable = root / 'Standalone app/Portable.zip'
            module.package(binary, icon, first, portable)
            module.package(binary, icon, second)
            self.assertEqual(first.read_bytes(), second.read_bytes())
            self.assertEqual(first.read_bytes(), portable.read_bytes())
            digest = hashlib.sha256(portable.read_bytes()).hexdigest()
            self.assertEqual(portable.with_suffix('.zip.sha256').read_text(), f'{digest}  Portable.zip\n')
            with zipfile.ZipFile(first) as archive:
                self.assertEqual(set(archive.namelist()), {'Bokheim.exe', 'Bokheim.ico', 'bokheim-opds-webview.exe', 'taxonomy.sqlite3', 'tts/alba/en_GB-alba-medium.onnx', 'tts/george/model.onnx'})
                self.assertNotIn('pdfium/pdfium.dll', archive.namelist())
            with zipfile.ZipFile(portable) as archive:
                extracted = root / 'Extracted app'
                archive.extractall(extracted)
            self.assertEqual((extracted / 'Bokheim.exe').read_bytes(), binary.read_bytes())
            self.assertEqual((extracted / 'bokheim-opds-webview.exe').read_bytes(), b'MZ helper')
            self.assertEqual((extracted / 'taxonomy.sqlite3').read_bytes(), b'SQLite format 3\0')
            self.assertEqual((extracted / 'tts/alba/en_GB-alba-medium.onnx').read_bytes(), b'voice')
            self.assertEqual((extracted / 'tts/george/model.onnx').read_bytes(), b'george-fp32')
            self.assertTrue(first.with_suffix('.zip.sha256').read_text().endswith('  first.zip\n'))
            binary.write_bytes(b'invalid executable')
            with self.assertRaises(ValueError):
                module.package(binary, icon, root / 'missing.zip', root / 'missing-portable.zip')
            self.assertFalse((root / 'missing.zip').exists())
            self.assertFalse((root / 'missing-portable.zip').exists())

    def test_update_and_portable_outputs_must_be_distinct(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            output = root / 'runtime.zip'
            with self.assertRaisesRegex(ValueError, 'different output paths'):
                module.package(root / 'app.exe', root / 'icon.ico', output, output)
            self.assertFalse(output.exists())
