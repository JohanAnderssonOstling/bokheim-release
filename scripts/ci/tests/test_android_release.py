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


if __name__ == '__main__':
    unittest.main()
