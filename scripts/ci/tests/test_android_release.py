import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('android_release', Path(__file__).resolve().parents[3] / 'apps/desktop-gpui/android/verify-release.py')
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class AndroidReleaseTests(unittest.TestCase):
    def test_instrumentation_receipt_requires_success_and_expected_signer(self):
        for result, signer, accepted in ((-1, 'ab' * 32, True), (0, 'ab' * 32, False), (-1, 'cd' * 32, False)):
            with self.subTest(result=result, signer=signer), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                apk = root / 'release.apk'
                apk.write_bytes(b'release-package')
                test_apk = root / 'test.apk'
                test_apk.write_bytes(b'test-package')
                output = root / 'info.json'
                info = dict(target='android-aarch64-apk', android=dict(debuggable=False, signing_cert_sha256=[signer]))
                replies = ['device\n', 'arm64-v8a,armeabi-v7a\n', 'Success\n', 'Success\n',
                           'INSTRUMENTATION_RESULT: bokheim_update_info=' + json.dumps(info) + f'\nINSTRUMENTATION_CODE: {result}\n']
                args = ['verify-release.py', '--serial', 'test-device', '--cert-sha256', 'ab' * 32,
                        '--apk', str(apk), '--test-apk', str(test_apk), '--output', str(output)]
                with patch('sys.argv', args), patch.object(release.subprocess, 'check_output', side_effect=replies) as adb:
                    if accepted:
                        release.main()
                        actual = json.loads(output.read_text())
                        self.assertEqual(actual['apk_sha256'], release.hashlib.sha256(b'release-package').hexdigest())
                    else:
                        with self.assertRaises(RuntimeError):
                            release.main()
                        self.assertFalse(output.exists())
                    for call in adb.call_args_list:
                        self.assertEqual(call.args[0][:3], ['adb', '-s', 'test-device'])

spec = importlib.util.spec_from_file_location('android_package', Path(__file__).resolve().parents[3] / 'apps/desktop-gpui/android/verify-package.py')
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


class AndroidPackageTests(unittest.TestCase):
    def test_package_verification_needs_no_device_and_rejects_wrong_identity(self):
        import zipfile
        with tempfile.TemporaryDirectory() as directory:
            apk = Path(directory) / 'app.apk'
            header = b'\x7fELF\x02\x01' + bytes(12) + (183).to_bytes(2, 'little')
            with zipfile.ZipFile(apk, 'w') as archive:
                for name in ('libdesktop_gpui.so', 'libpdfium.so'):
                    archive.writestr('lib/arm64-v8a/' + name, header)
            metadata = dict(application='1.2.3', schema=1, taxonomy_formats=[1], update_trust={'keys': {'key': 'ab' * 32}})
            good = "package: name='se.bokheim.reader.gpui' versionCode='1002003' versionName='1.2.3'\nnative-code: 'arm64-v8a'\n"
            for signer, badging, accepted in [('ab' * 32, good, True), ('cd' * 32, good, False),
                    ('ab' * 32, good + 'application-debuggable\n', False),
                    ('ab' * 32, good.replace("versionName='1.2.3'", "versionName='1.2.4'"), False)]:
                with self.subTest(signer=signer, badging=badging):
                    replies = [f'Signer #1 certificate SHA-256 digest: {signer}\n', badging]
                    with patch.object(package.subprocess, 'check_output', side_effect=replies) as tools:
                        if accepted:
                            info = package.verify(apk, '1.2.3', 'ab' * 32, metadata, 'apksigner', 'aapt')
                            self.assertEqual(info['verification']['device_runtime'], 'not_run')
                            self.assertEqual(info['apk_sha256'], package.hashlib.sha256(apk.read_bytes()).hexdigest())
                        else:
                            with self.assertRaises(ValueError):
                                package.verify(apk, '1.2.3', 'ab' * 32, metadata, 'apksigner', 'aapt')
                        self.assertTrue(all(call.args[0][0] in ('apksigner', 'aapt') for call in tools.call_args_list))


if __name__ == '__main__':
    unittest.main()
