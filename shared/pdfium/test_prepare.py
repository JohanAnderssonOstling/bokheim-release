import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import prepare


class PreparationTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.archive = self.root / "input.tgz"
        with tarfile.open(self.archive, "w:gz") as archive:
            for name, data in {"lib/libpdfium.so": b"native library", "LICENSE": b"Copyright \xa9 test"}.items():
                info = tarfile.TarInfo(name)
                info.size = len(data)
                archive.addfile(info, io.BytesIO(data))
        self.manifest = self.root / "artifacts.toml"
        self.manifest.write_text(f'''schema = 1
[targets.test]
version = "1"
url = "https://invalid.example/pdfium.tgz"
sha256 = "{prepare.digest(self.archive)}"
[targets.test.files]
"lib/libpdfium.so" = "libpdfium.so"
''')
        self.cache = self.root / "cache"
        self.destination = self.root / "prepared"
        self.patch = patch.object(prepare, "MANIFEST", self.manifest)
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def run_prepare(self, **kwargs):
        return prepare.prepare("test", cache=self.cache, destination=self.destination, offline=True, **kwargs)

    def test_local_archive_primes_offline_cache_and_preserves_unchanged_files(self):
        self.run_prepare(archive=self.archive)
        library = self.destination / "libpdfium.so"
        before = library.stat().st_mtime_ns
        self.archive.unlink()
        self.run_prepare()
        self.assertEqual(library.stat().st_mtime_ns, before)
        self.assertIn("Copyright © test", (self.destination / "LICENSE").read_text())
        receipt = json.loads((self.destination / "receipt.json").read_text())
        self.assertEqual(receipt["files"]["libpdfium.so"], hashlib.sha256(library.read_bytes()).hexdigest())

    def test_modified_prepared_library_is_repaired(self):
        self.run_prepare(archive=self.archive)
        library = self.destination / "libpdfium.so"
        library.write_bytes(b"broken")
        self.run_prepare()
        self.assertEqual(library.read_bytes(), b"native library")

    def test_rejects_corrupt_local_archive_before_staging(self):
        self.archive.write_bytes(b"corrupt")
        with self.assertRaisesRegex(RuntimeError, "checksum mismatch"):
            self.run_prepare(archive=self.archive)
        self.assertFalse(self.destination.exists())

    def test_offline_cache_miss_does_not_attempt_download(self):
        with patch.object(prepare.urllib.request, "urlretrieve") as download:
            with self.assertRaisesRegex(RuntimeError, "unavailable"):
                self.run_prepare()
            download.assert_not_called()

    def test_local_notices_are_verified_before_preparation(self):
        notices = self.root / "NOTICES.txt"
        notices.write_text("Pinned notices")
        self.manifest.write_text(self.manifest.read_text().replace(
            "[targets.test.files]",
            f'notices_file = "NOTICES.txt"\nnotices_sha256 = "{prepare.digest(notices)}"\n[targets.test.files]'))
        self.run_prepare(archive=self.archive)
        self.assertEqual((self.destination / "LICENSE").read_text(), "Pinned notices")
        notices.write_text("modified")
        with self.assertRaisesRegex(RuntimeError, "notices checksum mismatch"):
            self.run_prepare()

    def test_rejects_output_path_traversal(self):
        self.manifest.write_text(self.manifest.read_text().replace('= "libpdfium.so"', '= "../escape.so"'))
        with self.assertRaisesRegex(RuntimeError, "Invalid PDFium artifact"):
            self.run_prepare(archive=self.archive)
        self.assertFalse((self.root / "escape.so").exists())


if __name__ == "__main__":
    unittest.main()
