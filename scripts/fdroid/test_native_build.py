import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest

from native_build import apply_source_patch, digest, extract, fetch, receipt, validate_receipt


class NativeSourceIntegrityTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def test_receipt_rejects_changed_library_and_changed_source_pins(self):
        (self.root / "library.so").write_bytes(b"built library")
        receipt(self.root, {"kind": "source", "source_manifest_sha256": "pinned"}, ["library.so"])
        validate_receipt(self.root, ["library.so"], "pinned")
        with self.assertRaisesRegex(RuntimeError, "pinned configuration"):
            validate_receipt(self.root, ["library.so"], "different source pins")
        (self.root / "library.so").write_bytes(b"replaced library")
        with self.assertRaisesRegex(RuntimeError, "checksum mismatch"):
            validate_receipt(self.root, ["library.so"], "pinned")

    def test_cached_download_is_checked_even_offline(self):
        path = self.root / "archive"
        path.write_bytes(b"original")
        expected = digest(path)
        self.assertEqual(fetch("https://invalid.example", expected, path, True), path)
        path.write_bytes(b"tampered")
        with self.assertRaisesRegex(RuntimeError, "checksum mismatch"):
            fetch("https://invalid.example", expected, path, True)

    def test_pristine_source_is_patched_and_second_run_does_not_reverse_it(self):
        path = self.root / "source.txt"
        path.write_text("before\n")
        patch = self.root / "change.patch"
        patch.write_text("--- a/source.txt\n+++ b/source.txt\n@@ -1 +1 @@\n-before\n+after\n")
        apply_source_patch(patch, self.root)
        self.assertEqual(path.read_text(), "after\n")
        apply_source_patch(patch, self.root)
        self.assertEqual(path.read_text(), "after\n")

    def test_source_archive_drops_external_example_links_without_writing_outside(self):
        archive = self.root / "source.tar.gz"
        with tarfile.open(archive, "w:gz") as stream:
            source = tarfile.TarInfo("source/code.c")
            source.size = 4
            stream.addfile(source, io.BytesIO(b"code"))
            link = tarfile.TarInfo("source/external")
            link.type = tarfile.SYMTYPE
            link.linkname = "/outside-the-source-tree"
            stream.addfile(link)
        result = extract(archive, self.root / "unpacked")
        self.assertEqual((result / "code.c").read_bytes(), b"code")
        self.assertFalse((result / "external").is_symlink())


if __name__ == "__main__":
    unittest.main()
