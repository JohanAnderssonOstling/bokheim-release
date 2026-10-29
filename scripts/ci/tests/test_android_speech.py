import hashlib
import importlib.util
import io
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SPEECH = Path(__file__).resolve().parents[3] / 'shared/reader-speech'
sys.path.insert(0, str(SPEECH))
spec = importlib.util.spec_from_file_location('android_speech', SPEECH / 'prepare_android.py')
speech = importlib.util.module_from_spec(spec)
spec.loader.exec_module(speech)
sys.path.pop(0)


class AndroidSpeechTests(unittest.TestCase):
    def test_rejects_corrupt_runtime_before_publishing_libraries(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / f'sherpa-onnx-v{speech.VERSION}-android.tar.bz2').write_bytes(b'corrupt')
            with self.assertRaisesRegex(RuntimeError, 'checksum mismatch'):
                speech.prepare_android(root, offline=True)
            self.assertFalse((root / 'jniLibs/arm64-v8a/.archive-sha256').exists())

    def test_stages_only_arm64_dependencies_and_preserves_voice_marker_for_apk(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / f'sherpa-onnx-v{speech.VERSION}-android.tar.bz2'
            with tarfile.open(archive, 'w:bz2') as output:
                for abi in ('arm64-v8a', 'x86_64'):
                    for name in (*speech.REQUIRED, 'libsherpa-onnx-jni.so'):
                        entry = tarfile.TarInfo(f'jniLibs/{abi}/{name}')
                        entry.size = 3
                        output.addfile(entry, io.BytesIO(b'elf'))
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            def voice(destination, **kwargs):
                Path(destination).mkdir(parents=True)
            george = root / 'assets/tts/george'
            george.mkdir(parents=True)
            (george / 'model.onnx').write_bytes(b'stale-george')
            with patch.object(speech, 'SHA256', digest), patch.object(speech, 'prepare', side_effect=voice) as prepare:
                libraries = speech.prepare_android(root, offline=True)
            prepare.assert_called_once_with(root / 'assets/tts/alba', offline=True, archive=None)
            self.assertEqual({path.name for path in libraries.glob('*.so')}, set(speech.REQUIRED))
            self.assertFalse((root / 'jniLibs/x86_64').exists())
            self.assertEqual((root / 'assets/tts/alba/voice.sha256').read_text().strip(), speech.VOICE_SHA256)
            self.assertEqual({path.name for path in (root / 'assets/tts').iterdir()}, {'alba'})


if __name__ == '__main__':
    unittest.main()
