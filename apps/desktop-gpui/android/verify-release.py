#!/usr/bin/env python3
"""Read runtime metadata from a release APK on an explicitly selected test device."""
import argparse
import hashlib
import json
import re
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cert-sha256', required=True, help='Pinned production signing certificate, lowercase SHA-256')
    parser.add_argument('--serial', required=True, help='Dedicated test device; installs both APKs')
    parser.add_argument('--adb', default='adb')
    parser.add_argument('--apk', type=Path, required=True)
    parser.add_argument('--test-apk', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch('[0-9a-f]{64}', args.cert_sha256):
        raise ValueError('Expected a lowercase SHA-256 production signing certificate fingerprint')

    def adb(*command):
        return subprocess.check_output([args.adb, '-s', args.serial, *command], text=True, timeout=180)

    if adb('get-state').strip() != 'device':
        raise RuntimeError('Selected Android device is unavailable')
    if 'arm64-v8a' not in adb('shell', 'getprop', 'ro.product.cpu.abilist').strip().split(','):
        raise RuntimeError('The release requires an ARM64 device')
    for apk in (args.apk, args.test_apk):
        if 'Success' not in adb('install', '-r', str(apk.resolve())):
            raise RuntimeError(f'APK installation failed: {apk.name}')
    output = adb('shell', 'am', 'instrument', '-w', '-r',
                 'se.bokheim.reader.gpui.test/se.bokheim.reader.gpui.UpdateMetadataInstrumentation')
    prefix = 'INSTRUMENTATION_RESULT: bokheim_update_info='
    metadata = [line[len(prefix):] for line in output.splitlines() if line.startswith(prefix)]
    if 'INSTRUMENTATION_CODE: -1' not in output.splitlines() or len(metadata) != 1:
        raise RuntimeError(f'Packaged runtime metadata probe failed:\n{output}')
    info = json.loads(metadata[0])
    if info.get('target') != 'android-aarch64-apk' or info.get('android', {}).get('debuggable') is not False:
        raise RuntimeError('Expected an optimized ARM64 release application')
    if info.get('android', {}).get('signing_cert_sha256') != [args.cert_sha256]:
        raise RuntimeError('APK signer differs from the pinned production certificate')
    with args.apk.open('rb') as stream:
        info['apk_sha256'] = hashlib.file_digest(stream, 'sha256').hexdigest()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(info, indent=2) + '\n')


if __name__ == '__main__':
    main()
