import argparse
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location('publish_release', ROOT / 'servers/updates/publish-github-release.py')
publish = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publish)


class PublishReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bundle = self.root / 'prepared'
        self.bundle.mkdir()
        self.sha = 'a' * 40
        self.source = 'owner/source'
        self.repository = 'owner/releases'
        self.packages = {target: dict(url=f'https://updates.example.org/releases/v1.2.3/{filename}', bytes=7, sha256=publish.hashlib.sha256(b'package').hexdigest())
                         for target, (_, filename) in publish.prepare_release.PACKAGES.items()}
        for _, filename in publish.prepare_release.PACKAGES.values():
            path = self.bundle / 'artifacts/updates/releases/v1.2.3' / filename
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b'package')
        self.manifest = dict(protocol_version=1, channel='stable', sequence=4, application=dict(version='1.2.3', artifacts=self.packages), taxonomy=None)
        self.receipt = dict(source_repository=self.source, source_commit=self.sha, workflow_run=123,
                            release_repository=self.repository, tag='v1.2.3', signing_key_id='release-1', artifacts=self.packages)
        (self.bundle / 'manifest.json').write_text(json.dumps(self.manifest))
        (self.bundle / 'provenance.json').write_text(json.dumps(self.receipt))
        self.key = self.root / 'key.pk8'
        self.key.write_bytes(b'unit-test-key-placeholder')
        self.config = self.root / 'config.json'
        self.config.write_text(json.dumps(dict(endpoint='https://updates.example.org/v1/stable', channel='stable', trusted_keys={'release-1': 'ab' * 32}, origins={'https://updates.example.org': 'updates'})))
        self.args = argparse.Namespace(bundle=self.bundle, review_sha256=publish.reviewed_inputs(self.bundle)[2],
                                       key=self.key, config=self.config, output=self.root / 'signed', host='publisher@example.org', staging_root=None)
        self.calls = []
        self.draft = False
        self.source_receipt = self.sha
        self.shared_checks = 0
        self.superseded = False
        self.fail_publication = False
        self.mutate_input = False

    def command(self, *args):
        self.calls.append(args)
        if args[:2] == ('gh', 'api'):
            if self.mutate_input:
                (self.bundle / 'manifest.json').write_text('{}')
            return json.dumps(dict(repository={'full_name': self.source}, head_repository={'full_name': self.source},
                                   path='.github/workflows/desktop-release.yml', event='workflow_dispatch', status='completed', conclusion='success', head_sha=self.sha))
        if args[:3] == ('gh', 'release', 'view'):
            return json.dumps(dict(tagName='v1.2.3', isDraft=self.draft, isPrerelease=False,
                                   assets=[{'name': name} for name in ['desktop-source-commit.txt', *[p[1] for p in publish.prepare_release.PACKAGES.values()]]]))
        if args[:3] == ('gh', 'release', 'download'):
            (Path(args[args.index('--dir') + 1]) / 'desktop-source-commit.txt').write_text(self.source_receipt)
            return ''
        if any(str(arg).endswith('require-shared-tests.py') for arg in args):
            self.shared_checks += 1
            if self.superseded and self.shared_checks == 2:
                raise RuntimeError('shared gate superseded')
            self.assertEqual(args[-2:], (self.source, self.sha))
            return ''
        if args[0] == 'cargo' and 'sign_manifest' in args:
            self.assertIn('--release', args)
            self.assertNotIn('--locked', args)
            manifest, key, key_id, output = args[args.index('--') + 1:]
            self.assertEqual(Path(key), self.key)
            Path(output).write_text(json.dumps(dict(key_id=key_id, payload=Path(manifest).read_text(), signature='ab' * 64)))
            return ''
        if args[0] == 'bash' or (args[0] == 'cargo' and 'update-publisher' in args):
            self.assertEqual({p.name for p in self.args.output.iterdir()}, {'manifest.json', 'manifest.signed.json', 'provenance.json', 'artifacts'})
            if self.fail_publication:
                raise RuntimeError('publication result uncertain')
            return ''
        raise AssertionError(args)

    def run_publish(self, feed=None):
        def fetched(endpoint):
            self.assertEqual(endpoint, 'https://updates.example.org/v1/stable')
            return (self.args.output / 'manifest.signed.json').read_bytes() if feed is None else feed
        with patch.object(publish, 'command', self.command), patch.object(publish, 'fetch_feed', fetched), patch('builtins.print'):
            publish.publish(self.args)

    def test_signs_reviewed_snapshot_and_rechecks_gate_before_deploy(self):
        expected = (self.bundle / 'manifest.json').read_bytes()
        self.mutate_input = True
        self.run_publish()
        self.assertEqual((self.args.output / 'manifest.json').read_bytes(), expected)
        self.assertEqual(self.shared_checks, 2)
        self.assertEqual(self.calls[-1][0], 'bash')

    def test_local_receipts_are_rechecked_before_signing(self):
        receipts = {target: {'files': {publish.prepare_release.PACKAGES[target][1]:
                    {k: artifact[k] for k in ('bytes', 'sha256')}}} for target, artifact in self.packages.items()}
        self.receipt.update(workflow_run=None, build_receipts=receipts)
        (self.bundle / 'provenance.json').write_text(json.dumps(self.receipt))
        self.args.review_sha256 = publish.reviewed_inputs(self.bundle)[2]
        with patch.object(publish.prepare_release, 'verified_receipts', return_value=receipts):
            self.run_publish()
        self.assertTrue((self.args.output / 'manifest.signed.json').is_file())
        self.args.output = self.root / 'changed-receipts'
        self.calls.clear()
        with patch.object(publish.prepare_release, 'verified_receipts', return_value={}), self.assertRaisesRegex(ValueError, 'receipts differ'):
            self.run_publish()
        self.assertFalse(any('sign_manifest' in call for call in self.calls))

    def test_review_digest_covers_manifest_and_provenance(self):
        for name in ['manifest.json', 'provenance.json']:
            with self.subTest(name=name):
                path = self.bundle / name
                original = path.read_bytes()
                path.write_bytes(original + b'\n')
                with self.assertRaisesRegex(ValueError, 'Review digest differs'):
                    self.run_publish()
                path.write_bytes(original)
                self.assertFalse(self.calls)

    def test_draft_or_wrong_public_source_never_reaches_signer(self):
        for draft, source in [(True, self.sha), (False, 'b' * 40)]:
            self.draft, self.source_receipt = draft, source
            self.calls.clear()
            with self.assertRaises(ValueError):
                self.run_publish()
            self.assertFalse(any('sign_manifest' in call for call in self.calls))
            self.assertFalse(self.args.output.exists())

    def test_superseded_shared_result_blocks_publication(self):
        self.superseded = True
        with self.assertRaisesRegex(RuntimeError, 'superseded'):
            self.run_publish()
        self.assertFalse(any(call[0] == 'bash' for call in self.calls))
        self.assertFalse(self.args.output.exists())

    def test_uncertain_publish_and_wrong_public_feed_retain_signed_bundle(self):
        self.fail_publication = True
        with self.assertRaisesRegex(RuntimeError, 'uncertain'):
            self.run_publish()
        self.assertTrue((self.args.output / 'manifest.signed.json').is_file())
        self.args.output = self.root / 'signed-again'
        self.fail_publication = False
        with self.assertRaisesRegex(ValueError, 'public feed does not match'):
            self.run_publish(feed=b'old feed')
        self.assertTrue((self.args.output / 'manifest.signed.json').is_file())

    def test_tampered_prepared_package_never_reaches_signer(self):
        path = next((self.bundle / 'artifacts').rglob('*.apk'))
        path.write_bytes(b'tampered')
        with self.assertRaisesRegex(ValueError, 'reviewed hash or size'):
            self.run_publish()
        self.assertFalse(any('sign_manifest' in call for call in self.calls))
        self.assertFalse(self.args.output.exists())

    def test_local_staging_uses_release_publisher_without_ssh(self):
        self.args.host = None
        self.args.staging_root = self.root / 'staging'
        self.run_publish()
        self.assertEqual(self.calls[-1][0], 'cargo')
        self.assertIn('update-publisher', self.calls[-1])
        self.assertIn('--release', self.calls[-1])
        self.assertNotIn('--locked', self.calls[-1])
        self.assertFalse(any(call[0] == 'bash' for call in self.calls))


if __name__ == '__main__':
    unittest.main()
