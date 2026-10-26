#!/usr/bin/env python3
"""Verify the prebuilt ARM application embedded in the desktop installers."""
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile
import tomllib


ROOT = Path(__file__).resolve().parents[2]


def main():
    package_directory = ROOT / 'apps/kobo-installer/package'
    manifest = json.loads((package_directory / 'manifest.json').read_text())
    version = tomllib.loads((ROOT / 'apps/desktop-gpui/Cargo.toml').read_text())['package']['version']
    if manifest['version'] != version or manifest['cpu_profile'] != 'generic':
        raise RuntimeError('Installer version or CPU profile differs from the package manifest')
    expected = manifest['archive']
    archive = package_directory / 'Bokheim-Kobo-armv7.tar.gz'
    if expected['name'] != archive.name or archive.stat().st_size != expected['bytes']:
        raise RuntimeError('Kobo package filename or size differs')
    with archive.open('rb') as source:
        if hashlib.file_digest(source, 'sha256').hexdigest() != expected['sha256']:
            raise RuntimeError('Kobo package SHA-256 differs')
    with tarfile.open(archive) as package:
        info = package.extractfile('./.adds/bokheim/BUILD-INFO.txt').read().decode().splitlines()
        for line in (f'Version: {version}', f'Built: {manifest["built_at"]}', 'CPU profile: generic'):
            if line not in info:
                raise RuntimeError(f'Embedded build information differs: {line}')
    subprocess.run([
        'python3', str(ROOT / 'apps/desktop-gpui/scripts/verify-kobo-package.py'),
        str(archive), '--multi-device',
    ], check=True)
    print(f'Verified Kobo {version}: {expected["sha256"]}')


if __name__ == '__main__':
    main()
