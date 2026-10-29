#!/usr/bin/env python3
"""Build one Windows EXE that extracts the complete runtime on first launch."""
import argparse
import hashlib
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import zipfile

MAGIC = b'BOKHEIM-SFX-V1\0\0'
REQUIRED = {'Bokheim.exe', 'Bokheim.ico', 'bokheim-opds-webview.exe', 'taxonomy.sqlite3',
            'tts/alba/en_GB-alba-medium.onnx', 'tts/george/model.onnx'}


def package(launcher, archive, output):
    launcher, archive = launcher.resolve(strict=True), archive.resolve(strict=True)
    if output.resolve() in (launcher, archive):
        raise ValueError('Standalone output must differ from the launcher and archive')
    with launcher.open('rb') as source:
        if source.read(2) != b'MZ':
            raise ValueError('The standalone launcher is not a Windows executable')
    with zipfile.ZipFile(archive) as payload:
        if not REQUIRED.issubset(payload.namelist()):
            raise ValueError('The portable archive is missing required runtime files')
    with archive.open('rb') as source:
        digest = hashlib.file_digest(source, 'sha256').digest()
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=output.parent, prefix='.windows-standalone-') as stage:
        packaged = Path(stage) / output.name
        with packaged.open('wb') as target, launcher.open('rb') as stub, archive.open('rb') as payload:
            shutil.copyfileobj(stub, target, 1024 * 1024)
            shutil.copyfileobj(payload, target, 1024 * 1024)
            target.write(MAGIC + struct.pack('<Q', archive.stat().st_size) + digest)
        with packaged.open('rb') as source:
            checksum = hashlib.file_digest(source, 'sha256').hexdigest()
        packaged.replace(output)
        output.with_suffix(output.suffix + '.sha256').write_text(f'{checksum}  {output.name}\n', encoding='ascii')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', required=True, type=Path, help='Complete portable or update ZIP')
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--launcher', type=Path, help='Use an already compiled Windows launcher')
    parser.add_argument('--target', help='Windows Rust target; defaults to the native host')
    parser.add_argument('--target-dir', type=Path)
    args = parser.parse_args()
    launcher = args.launcher
    if launcher is None:
        target = args.target or next(line.removeprefix('host: ') for line in
            subprocess.check_output(['rustc', '-vV'], text=True).splitlines() if line.startswith('host: '))
        if 'windows' not in target:
            parser.error('Build on Windows, pass a Windows --target, or supply --launcher')
        manifest = Path(__file__).resolve().parent / 'standalone/Cargo.toml'
        target_dir = (args.target_dir or manifest.parents[4] / 'target/windows-standalone').resolve()
        environment = dict(os.environ)
        environment['RUSTFLAGS'] = environment.get('RUSTFLAGS', '') + ' -C target-feature=+crt-static'
        subprocess.run(['cargo', 'build', '--locked', '--release', '--manifest-path', str(manifest),
                        '--target', target, '--target-dir', str(target_dir)], check=True, env=environment)
        launcher = target_dir / target / 'release/bokheim-standalone.exe'
    package(launcher, args.archive, args.output)
    print(f'Created standalone Windows app: {args.output}')


if __name__ == '__main__':
    main()
