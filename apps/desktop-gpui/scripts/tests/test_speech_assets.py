"""Speech preparation rejects unverified archives and separates upstream repacks."""
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[4]
spec = importlib.util.spec_from_file_location("speech_prepare", ROOT / "shared/reader-speech/prepare.py")
prepare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare)


def george_archive():
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode="w:bz2") as archive:
        for name in ("model.onnx", "voices.bin", "tokens.txt", "lexicon-gb-en.txt", "LICENSE",
                     "espeak-ng-data/en_dict", "espeak-ng-data/lang/gmw/en", "lexicon-fr-fr.txt"):
            content = name.encode()
            member = tarfile.TarInfo("kokoro-multi-lang-v1_0/" + name)
            member.size = len(content)
            archive.addfile(member, io.BytesIO(content))
    return data.getvalue()


class SpeechAssetsTests(unittest.TestCase):
    def test_upstream_repack_does_not_reuse_the_previous_named_archive(self):
        data = george_archive()
        digest = hashlib.sha256(data).hexdigest()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cache = root / "cache/bokheim/tts"
            cache.mkdir(parents=True)
            legacy = cache / "kokoro-multi-lang-v1_0.tar.bz2"
            legacy.write_bytes(b"previous upstream archive")
            with patch.dict(os.environ, {"XDG_CACHE_HOME": str(root / "cache"), "XDG_DATA_HOME": str(root / "data")}), \
                 patch.object(prepare, "GEORGE_SHA256", digest), \
                 patch.object(prepare.urllib.request, "urlopen", return_value=io.BytesIO(data)) as download:
                output = prepare.prepare_george(root / "bundle")
                download.assert_called_once()
                self.assertEqual((output / "model.onnx").read_bytes(), b"model.onnx")
                self.assertTrue((output / "VOICE-LICENSE.txt").is_file())
                self.assertFalse((output / "lexicon-fr-fr.txt").exists())
                self.assertEqual((cache / f"kokoro-multi-lang-v1_0-{digest}.tar.bz2").read_bytes(), data)
                self.assertEqual(legacy.read_bytes(), b"previous upstream archive")

    def test_failed_verification_does_not_publish_an_archive_or_bundle(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with patch.dict(os.environ, {"XDG_CACHE_HOME": str(root / "cache"), "XDG_DATA_HOME": str(root / "data")}), \
                 patch.object(prepare.urllib.request, "urlopen", return_value=io.BytesIO(b"untrusted archive")):
                with self.assertRaisesRegex(RuntimeError, "George archive checksum mismatch"):
                    prepare.prepare_george(root / "bundle")
            self.assertFalse((root / "bundle").exists())
            self.assertEqual(list((root / "cache/bokheim/tts").iterdir()), [])


if __name__ == "__main__":
    unittest.main()
