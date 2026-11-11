import hashlib
import importlib.util
import io
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SPEECH = Path(__file__).resolve().parents[3] / 'apps/reader/crates/speech'
spec = importlib.util.spec_from_file_location('speech_prepare', SPEECH / 'prepare.py')
speech = importlib.util.module_from_spec(spec)
spec.loader.exec_module(speech)

FILES = {
    'en_GB-alba-medium.onnx': b'model',
    'tokens.txt': b'tokens',
    'VOICE-LICENSE.txt': b'license',
    'espeak-ng-data/en_dict': b'English dictionary',
    'espeak-ng-data/ru_dict': b'Russian dictionary',
    'espeak-ng-data/sv_dict': b'Swedish dictionary',
    'espeak-ng-data/lang/gmw/en': b'language en',
    'espeak-ng-data/lang/gmw/en-GB-scotland': b'language en-gb-scotland',
    'espeak-ng-data/lang/gmw/en-US': b'language en-us',
    'espeak-ng-data/lang/gmq/sv': b'language sv',
    'espeak-ng-data/lang/zle/ru': b'language ru',
    'espeak-ng-data/phondata': b'shared phoneme data',
    'espeak-ng-data/phonindex': b'shared phoneme index',
    'espeak-ng-data/phontab': b'shared phoneme tables',
    'espeak-ng-data/intonations': b'shared intonations',
    'espeak-ng-data/voices/!v/default': b'shared voice variant',
}


def write_cached_voice(destination):
    for name, content in FILES.items():
        path = destination / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
    (destination / '.archive-sha256').write_text(speech.SHA256 + '\n')


class SpeechPrepareTests(unittest.TestCase):
    def test_george_bundles_verified_fp32_assets_and_only_english_pronunciation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / 'george.tar.bz2'
            files = {name: content for name, content in FILES.items() if name != 'en_GB-alba-medium.onnx'}
            files.update({'model.onnx': b'fp32-model', 'voices.bin': b'voices', 'lexicon-gb-en.txt': b'British lexicon',
                          'LICENSE': b'Apache-2.0', 'lexicon-zh.txt': b'unneeded Chinese lexicon'})
            with tarfile.open(archive, 'w:bz2') as output:
                for name, content in files.items():
                    entry = tarfile.TarInfo('kokoro-multi-lang-v1_0/' + name)
                    entry.size = len(content)
                    output.addfile(entry, io.BytesIO(content))
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            destination = root / 'george'
            with patch.object(speech, 'GEORGE_SHA256', digest), patch.object(speech.urllib.request, 'urlopen') as download:
                speech.prepare_george(destination, offline=True, archive=archive)
                self.assertTrue(speech.is_george_prepared(destination))
                self.assertEqual((destination / 'model.onnx').read_bytes(), b'fp32-model')
                self.assertFalse((destination / 'lexicon-zh.txt').exists())
                self.assertFalse((destination / 'espeak-ng-data/ru_dict').exists())
                self.assertTrue((destination / 'espeak-ng-data/lang/gmw/en').is_file())
                modified = (destination / 'model.onnx').stat().st_mtime_ns
                speech.prepare_george(destination, offline=True, archive=archive)
                self.assertEqual((destination / 'model.onnx').stat().st_mtime_ns, modified)
                download.assert_not_called()

    def test_george_rejects_corrupt_archive_before_installing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / 'george.tar.bz2'
            archive.write_bytes(b'corrupt')
            with self.assertRaisesRegex(RuntimeError, 'checksum mismatch'):
                speech.prepare_george(root / 'george', offline=True, archive=archive)
            self.assertFalse((root / 'george').exists())

    def assert_english_bundle(self, destination):
        data = destination / 'espeak-ng-data'
        self.assertEqual({path.name for path in data.glob('*_dict')}, {'en_dict'})
        self.assertEqual({path.relative_to(data / 'lang').as_posix()
                          for path in (data / 'lang').rglob('*') if path.is_file()},
                         {'gmw/en', 'gmw/en-GB-scotland', 'gmw/en-US'})
        for name, content in FILES.items():
            if (destination / name).is_file() and name != 'VOICE-LICENSE.txt':
                self.assertEqual((destination / name).read_bytes(), content)
        for name in ('en_dict', 'phondata', 'phonindex', 'phontab', 'intonations', 'voices/!v/default'):
            self.assertTrue((data / name).is_file(), name)
        self.assertTrue(speech.is_prepared(destination))

    def test_trims_verified_archive_and_removes_stale_destination_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / 'voice.tar.bz2'
            with tarfile.open(archive, 'w:bz2') as output:
                for name, content in FILES.items():
                    entry = tarfile.TarInfo('vits-piper-en_GB-alba-medium/' + name)
                    entry.size = len(content)
                    output.addfile(entry, io.BytesIO(content))
            destination = root / 'prepared'
            stale = destination / 'espeak-ng-data/fr_dict'
            stale.parent.mkdir(parents=True)
            stale.write_bytes(b'stale French dictionary')
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            with patch.object(speech, 'SHA256', digest):
                speech.prepare(destination, offline=True, archive=archive)
                self.assert_english_bundle(destination)

    def test_upgrades_existing_multilingual_bundle_offline(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / 'prepared'
            write_cached_voice(destination)
            self.assertFalse(speech.is_prepared(destination))
            with patch.object(speech.urllib.request, 'urlopen') as download:
                speech.prepare(destination, offline=True)
                self.assert_english_bundle(destination)
                # A repeat build can reuse the trimmed bundle without rewriting it.
                marker = destination / '.assets-sha256'
                modified = marker.stat().st_mtime_ns
                speech.prepare(destination, offline=True)
                self.assertEqual(marker.stat().st_mtime_ns, modified)
                download.assert_not_called()

    def test_copies_and_trims_old_installed_cache_without_modifying_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = root / 'data/bokheim/tts/alba'
            write_cached_voice(installed)
            destination = root / 'prepared'
            with patch.dict(os.environ, {'XDG_DATA_HOME': str(root / 'data'),
                                         'XDG_CACHE_HOME': str(root / 'cache')}), \
                    patch.object(speech.urllib.request, 'urlopen') as download:
                speech.prepare(destination, offline=True)
                self.assert_english_bundle(destination)
                download.assert_not_called()
            self.assertTrue((installed / 'espeak-ng-data/ru_dict').is_file())
            self.assertFalse((installed / '.assets-sha256').exists())


if __name__ == '__main__':
    unittest.main()
