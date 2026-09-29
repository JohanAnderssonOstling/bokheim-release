import argparse
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('prepare_release', Path(__file__).resolve().parents[3] / 'servers/updates/prepare-github-release.py')
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class PreparationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.sha = 'a' * 40
        self.run = dict(repository={'full_name': 'owner/source'}, head_repository={'full_name': 'owner/source'},
                        path='.github/workflows/desktop-release.yml', event='workflow_dispatch',
                        status='completed', conclusion='success', head_sha=self.sha)
        self.config = dict(endpoint='https://updates.example.org/v1/stable', channel='stable', trusted_keys={'release-1': 'ab' * 32},
                           origins={'https://updates.example.org': 'updates'}, external_artifact_prefixes=[])
        config = root / 'config.json'
        config.write_text(json.dumps(self.config))
        self.args = argparse.Namespace(source_repository='owner/source', release_repository='owner/packages', run_id=123,
                                       tag='v2.0.0', sequence=2, valid_days=14, config=config,
                                       previous_manifest=None, key_id='release-1', android_cert_sha256='cd' * 32, output=root / 'output')
        self.info = dict(target='linux-x86_64-appimage', application='2.0.0', schema=80, taxonomy_formats=[11],
                         update_trust=dict(endpoint=self.config['endpoint'], channel='stable', keys=self.config['trusted_keys']))
        self.windows_info = {}
        self.android_info = {}
        self.corrupt = False
        self.shared_passed = True
        self.calls = []

    def command(self, *args):
        self.calls.append(args)
        if args[0] != 'gh':
            if not self.shared_passed:
                raise RuntimeError('shared tests failed')
            self.assertEqual(args[-2:], ('owner/source', self.sha))
            return ''
        if args[1] == 'api':
            return json.dumps(self.run)
        if args[1:3] == ('release', 'view'):
            return json.dumps(dict(tagName='v2.0.0', isDraft=True, isPrerelease=False,
                                   assets=[{'name': n} for n in ('desktop-source-commit.txt', *[value[1] for value in release.PACKAGES.values()])]))
        directory = Path(args[args.index('--dir') + 1])
        if args[1] == 'run':
            archive = args[args.index('--name') + 1]
            target, (_, filename) = next((target, value) for target, value in release.PACKAGES.items() if value[0] == archive)
            info = dict(self.info, target=target)
            if target == 'windows-x86_64-zip':
                info.update(self.windows_info)
            if target == 'android-aarch64-apk':
                info['apk_sha256'] = release.hashlib.sha256(b'package').hexdigest()
                info['android'] = dict(package='se.bokheim.reader.gpui', version_code=2_000_000, signing_cert_sha256=['cd' * 32], debuggable=False)
                info['android'].update(self.android_info)
            (directory / f'update-info-{target}.json').write_text(json.dumps(info))
            (directory / filename).write_bytes(b'package')
        else:
            name = args[args.index('--pattern') + 1]
            (directory / name).write_bytes(self.sha.encode() if name.endswith('.txt') else (b'corrupt' if self.corrupt else b'package'))
        return ''

    def prepare(self):
        with patch.object(release, 'command', self.command):
            release.prepare(self.args)

    def test_prepares_verified_package_and_preserves_taxonomy(self):
        previous = self.args.output.parent / 'previous.json'
        taxonomy = {'release_id': 42, 'artifact': {'url': 'https://taxonomy.example.org/releases/42/snapshot'}}
        previous.write_text(json.dumps(dict(protocol_version=1, channel='stable', sequence=1, taxonomy=taxonomy)))
        self.args.previous_manifest = previous
        self.prepare()
        manifest = json.loads((self.args.output / 'manifest.json').read_text())
        self.assertEqual(manifest['taxonomy'], taxonomy)
        self.assertEqual(manifest['application']['schema_version'], 80)
        package = manifest['application']['artifacts']['linux-x86_64-appimage']
        self.assertEqual(package['url'], 'https://updates.example.org/releases/v2.0.0/Bokheim-x86_64.AppImage')
        self.assertEqual((self.args.output / 'artifacts/updates/releases/v2.0.0/Bokheim-x86_64.AppImage').read_bytes(), b'package')
        self.assertEqual(package['bytes'], 7)
        self.assertEqual(package['sha256'], release.hashlib.sha256(b'package').hexdigest())
        self.assertIn('windows-x86_64-zip', manifest['application']['artifacts'])
        self.assertIn('android-aarch64-apk', manifest['application']['artifacts'])
        receipt = json.loads((self.args.output / 'provenance.json').read_text())
        self.assertEqual(receipt['source_commit'], self.sha)
        self.assertTrue(receipt['release_was_draft'])

    def test_local_packages_and_independent_windows_run(self):
        self.args.run_id = None
        self.args.source_commit = self.sha
        fixture = self.args.output.parent / 'built'
        fixture.mkdir()
        receipts = {}
        for platform in ('linux', 'windows', 'android'):
            target = release.release_artifacts.TARGETS[platform]
            archive, _ = release.PACKAGES[target]
            self.command('gh', 'run', 'download', '123', '--name', archive, '--dir', str(fixture))
            for name in release.release_artifacts.FILES[platform]:
                if not (fixture / name).exists():
                    (fixture / name).write_bytes(b'package')
            receipts[target] = release.release_artifacts.record(platform, fixture, self.sha, 'owner/source', 123 if platform == 'windows' else None)

        def local_command(*args):
            if args[:2] == ('gh', 'api') and '/commits/' in args[-1]:
                return json.dumps({'sha': self.sha})
            if args[:3] == ('gh', 'release', 'download'):
                name = args[args.index('--pattern') + 1]
                if name != 'desktop-source-commit.txt':
                    destination = Path(args[args.index('--dir') + 1]) / name
                    destination.write_bytes((fixture / name).read_bytes())
                    return ''
            return self.command(*args)

        with patch.object(release, 'command', local_command):
            release.prepare(self.args)
        provenance = json.loads((self.args.output / 'provenance.json').read_text())
        self.assertEqual(provenance['build_receipts'], receipts)
        self.assertIsNone(provenance['workflow_run'])
        # A replaced local package cannot be prepared under its previous receipt.
        self.args.output = self.args.output.parent / 'replaced-output'
        (fixture / 'Bokheim-x86_64.AppImage').write_bytes(b'replaced')
        with patch.object(release, 'command', local_command), self.assertRaisesRegex(ValueError, 'differs from verified local build'):
            release.prepare(self.args)
        self.assertFalse(self.args.output.exists())

    def test_windows_package_must_match_shared_schema_and_bundled_trust(self):
        for change in ({'schema': 81}, {'update_trust': None}):
            self.windows_info = change
            with self.assertRaises(ValueError):
                self.prepare()
            self.assertFalse(self.args.output.exists())

    def test_android_identity_must_match_production_release(self):
        for change in ({'signing_cert_sha256': ['ef' * 32]}, {'version_code': 1},
                       {'package': 'other.app'}, {'debuggable': True}):
            self.android_info = change
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, 'Android package identity'):
                self.prepare()
            self.assertFalse(self.args.output.exists())

    def test_failed_shared_gate_leaves_no_output(self):
        self.shared_passed = False
        with self.assertRaises(RuntimeError):
            self.prepare()
        self.assertFalse(self.args.output.exists())
        self.assertFalse(any('download' in call for call in self.calls))

    def test_replaced_package_leaves_no_output(self):
        self.corrupt = True
        with self.assertRaisesRegex(ValueError, 'differs from successful build'):
            self.prepare()
        self.assertFalse(self.args.output.exists())

    def test_missing_or_wrong_bundled_trust_rejected(self):
        for trust in (None, {}, dict(endpoint='https://other.example.org', channel='stable', keys=self.config['trusted_keys'])):
            self.info['update_trust'] = trust
            with self.assertRaisesRegex(ValueError, 'expected update endpoint'):
                self.prepare()
            self.assertFalse(self.args.output.exists())

    def test_untrusted_or_incomplete_workflow_rejected(self):
        for change in (dict(conclusion='failure'), dict(status='in_progress'), dict(event='pull_request'),
                       dict(path='.github/workflows/other.yml'), dict(head_repository={'full_name': 'fork/source'})):
            with self.subTest(change=change), self.assertRaises(ValueError):
                release.validate_run(dict(self.run, **change), 'owner/source')

    def test_publisher_can_retain_old_keys_without_requiring_them_in_new_package(self):
        self.config['trusted_keys']['old-key'] = 'cd' * 32
        # In a real build the package contains only its current key.
        self.info['update_trust']['keys'] = {'release-1': 'ab' * 32}
        self.args.config.write_text(json.dumps(self.config))
        self.prepare()
        self.assertTrue((self.args.output / 'manifest.json').exists())


if __name__ == '__main__':
    unittest.main()
