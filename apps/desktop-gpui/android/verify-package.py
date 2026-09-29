#!/usr/bin/env python3
"""Verify a signed release APK without installing it or requiring an Android device."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import zipfile


def require(ok, message):
    if not ok:
        raise ValueError(message)


def sdk_tool(name):
    found = shutil.which(name)
    if found:
        return found
    sdk = os.environ.get('ANDROID_HOME') or os.environ.get('ANDROID_SDK_ROOT')
    if sdk:
        tool = Path(sdk) / 'build-tools/36.0.0' / name
        if tool.is_file():
            return str(tool)
    raise ValueError(f'{name} is missing: set ANDROID_HOME or add Android build-tools to PATH')


def verify(apk, version, certificate, metadata, apksigner, aapt):
    require(re.fullmatch('[0-9a-f]{64}', certificate), 'Invalid production certificate SHA-256')
    require(re.fullmatch(r'\d+\.\d+\.\d+', version), 'Expected stable application version')
    signature = subprocess.check_output([apksigner, 'verify', '--verbose', '--print-certs', str(apk)], text=True)
    signers = re.findall(r'^Signer #\d+ certificate SHA-256 digest: ([0-9a-fA-F]{64})$', signature, re.M)
    require([s.lower() for s in signers] == [certificate], 'APK signer differs from production certificate')
    badging = subprocess.check_output([aapt, 'dump', 'badging', str(apk)], text=True)
    package = re.search(r"^package: name='([^']+)' versionCode='(\d+)' versionName='([^']+)'", badging, re.M)
    major, minor, patch = map(int, version.split('.'))
    require(package is not None and package.group(1) == 'se.bokheim.reader.gpui'
            and package.group(3) == version and int(package.group(2)) == major * 1_000_000 + minor * 1_000 + patch,
            'APK package/version mismatch')
    require(not re.search(r'^application-debuggable(?:\s|$)', badging, re.M), 'Debuggable APK cannot be distributed')
    require(re.search(r"^native-code: 'arm64-v8a'\s*$", badging, re.M), 'Expected ARM64 APK')
    with zipfile.ZipFile(apk) as archive:
        for name in ('libdesktop_gpui.so', 'libpdfium.so'):
            with archive.open('lib/arm64-v8a/' + name) as library:
                header = library.read(20)
            require(header[:6] == b'\x7fELF\x02\x01' and int.from_bytes(header[18:20], 'little') == 183,
                    f'Missing or invalid ARM64 native library: {name}')
    info = dict(metadata)
    require(info.get('application') == version and type(info.get('schema')) is int and info['schema'] > 0
            and info.get('taxonomy_formats') and info.get('update_trust'), 'Missing build compatibility/trust metadata')
    info.update(target='android-aarch64-apk',
                android=dict(package=package.group(1), version_code=int(package.group(2)),
                             signing_cert_sha256=[certificate], debuggable=False),
                verification={'package': 'passed', 'device_runtime': 'not_run', 'compatibility': 'build_metadata'})
    with apk.open('rb') as stream:
        info['apk_sha256'] = hashlib.file_digest(stream, 'sha256').hexdigest()
    return info


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apk', required=True, type=Path)
    parser.add_argument('--version', required=True)
    parser.add_argument('--cert-sha256', required=True)
    parser.add_argument('--metadata', required=True, type=Path, help='Compatibility metadata from the same build source/configuration')
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    info = verify(args.apk, args.version, args.cert_sha256, json.loads(args.metadata.read_text()), sdk_tool('apksigner'), sdk_tool('aapt'))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(info, indent=2) + '\n')
    print(f'Package verified: {args.apk.name}; device runtime testing not performed')


if __name__ == '__main__':
    main()
