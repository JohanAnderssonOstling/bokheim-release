import importlib.util
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
            (root / 'pdfium').mkdir()
            (root / 'pdfium/pdfium.dll').write_bytes(b'MZ library')
            icon = root / 'icon.ico'
            icon.write_bytes(b'icon')
            first, second = root / 'first.zip', root / 'second.zip'
            module.package(binary, icon, first)
            module.package(binary, icon, second)
            self.assertEqual(first.read_bytes(), second.read_bytes())
            with zipfile.ZipFile(first) as archive:
                self.assertEqual(set(archive.namelist()), {'Bokheim.exe', 'Bokheim.ico'})
                self.assertNotIn('pdfium/pdfium.dll', archive.namelist())
            self.assertTrue(first.with_suffix('.zip.sha256').read_text().endswith('  first.zip\n'))
            binary.write_bytes(b'invalid executable')
            with self.assertRaises(ValueError):
                module.package(binary, icon, root / 'missing.zip')
            self.assertFalse((root / 'missing.zip').exists())
