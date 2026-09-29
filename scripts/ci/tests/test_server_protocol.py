import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch
from urllib.error import HTTPError

spec = importlib.util.spec_from_file_location('check_server_protocol', Path(__file__).parents[1] / 'check-server-protocol.py')
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class ProtocolProbeTests(unittest.TestCase):
    def test_deployed_protocol_gate_accepts_decode_rejection_only(self):
        for status in (400, 426, 401, 404, 502):
            with self.subTest(status=status), patch.object(probe.subprocess, 'check_output', return_value='pub const MEDIA_TYPE: &str = "application/vnd.example; version=42";'), patch.object(probe.urllib.request, 'urlopen', side_effect=HTTPError('https://server/api/auth/login', status, '', {}, None)) as request:
                if status == 400:
                    probe.check('a' * 40)
                else:
                    with self.assertRaises(RuntimeError):
                        probe.check('a' * 40)
                self.assertEqual(request.call_args.args[0].data, b'')
                self.assertEqual(request.call_args.args[0].get_header('Content-type'), 'application/vnd.example; version=42')
