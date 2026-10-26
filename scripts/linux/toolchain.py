#!/usr/bin/env python3
"""Prepare the pinned glibc 2.17 SDK, or run a command with its build environment."""
import argparse
import hashlib
import json
import lzma
import os
from pathlib import Path
import platform
import shutil
import struct
import subprocess
import tarfile
import tempfile
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = Path(__file__).with_name('toolchain.json')
TARGET = 'x86_64-unknown-linux-gnu'


def sdk_path():
    return Path(os.environ.get('BOKHEIM_LINUX_SDK', ROOT / '.linux-toolchain')).resolve()


def download(artifact, cache, offline):
    path = cache / artifact['name']
    if not path.exists():
        if offline:
            raise RuntimeError(f'Missing offline Linux toolchain input: {path}')
        print(f"Downloading {artifact['name']}", flush=True)
        with urllib.request.urlopen(artifact['url'], timeout=120) as source:
            with tempfile.NamedTemporaryFile(dir=cache, delete=False) as output:
                temporary = Path(output.name)
                try:
                    shutil.copyfileobj(source, output)
                except BaseException:
                    temporary.unlink(missing_ok=True)
                    raise
        temporary.replace(path)
    with path.open('rb') as source:
        checksum = hashlib.file_digest(source, 'sha256').hexdigest()
    if checksum != artifact['sha256']:
        raise RuntimeError(f'Linux toolchain checksum mismatch: {path}')
    return path


def extract_rpm(archive, destination):
    # CentOS 7 RPMs have an XZ-compressed newc payload. Skip the lead and the
    # signature/package headers; no rpm installation or root access is needed.
    data = archive.read_bytes()
    offset = 96
    for signature in (True, False):
        if data[offset:offset + 3] != b'\x8e\xad\xe8':
            raise RuntimeError(f'Invalid RPM header: {archive}')
        count, size = struct.unpack_from('>II', data, offset + 8)
        offset += 16 + count * 16 + size
        if signature:
            offset = (offset + 7) & ~7
    # libarchive creates missing parent directories for leading symlink entries.
    # Ubuntu 22.04's cpio fails on the first libxcb entry before usr/lib64 exists.
    subprocess.run(['bsdtar', '-xf', '-', '-C', str(destination)],
                   input=lzma.decompress(data[offset:]), cwd=destination, check=True)


def extract_conda(archive, destination):
    with zipfile.ZipFile(archive) as source:
        for name in source.namelist():
            if name.endswith('.tar.zst'):
                unpacked = subprocess.run(['zstd', '-d', '-q', '-c'], input=source.read(name),
                                          stdout=subprocess.PIPE, check=True).stdout
                import io
                with tarfile.open(fileobj=io.BytesIO(unpacked)) as package:
                    package.extractall(destination, filter='data')


def prepare(offline=False):
    if platform.system() != 'Linux' or platform.machine() not in ('x86_64', 'AMD64'):
        raise RuntimeError('The Linux release SDK requires an x86-64 Linux build host')
    sdk = sdk_path()
    manifest = MANIFEST.read_bytes()
    compiler = Path(__file__).with_name('compiler.py')
    fingerprint = hashlib.sha256(manifest + Path(__file__).read_bytes() + compiler.read_bytes()).hexdigest()
    marker = sdk / 'ready'
    if marker.exists() and marker.read_text() == fingerprint:
        return sdk
    for tool in ('bsdtar', 'zstd', 'pkg-config', 'readelf'):
        if not shutil.which(tool):
            raise RuntimeError(f'Linux release toolchain needs {tool} on PATH')
    cache = sdk / 'downloads'
    cache.mkdir(parents=True, exist_ok=True)
    config = json.loads(manifest)
    inputs = [(item, download(item, cache, offline))
              for item in [config['zig'], *config['artifacts']]]
    with tempfile.TemporaryDirectory(prefix='prepare-', dir=sdk) as temporary:
        stage = Path(temporary)
        native = stage / 'native'
        native.mkdir()
        for item, archive in inputs:
            if archive.name.endswith('.rpm'):
                extract_rpm(archive, native)
            elif archive.name.endswith('.conda'):
                extract_conda(archive, native)
            else:
                with tarfile.open(archive) as package:
                    package.extractall(stage, filter='data')
        # Zig ships modern headers gated by the requested glibc version.
        # GCC 12's __has_include alone mistakes the gated 2.32 header for a
        # usable API. Preserve libstdc++'s existing pthread-based fallback.
        atomicity = native / 'x86_64-conda-linux-gnu/include/c++/12.3.0/ext/atomicity.h'
        text = atomicity.read_text()
        text = text.replace('__has_include(<sys/single_threaded.h>)',
                            '(__GLIBC_PREREQ(2, 32) && __has_include(<sys/single_threaded.h>))')
        atomicity.write_text(text)
        libraries = stage / 'lib'
        libraries.mkdir()
        for directory in (native / 'lib64', native / 'usr/lib64', native / 'lib'):
            if directory.exists():
                for library in directory.glob('*.so*'):
                    if library.is_file():
                        # Flatten symlinks so the SDK is relocatable.
                        shutil.copy2(library.resolve(), libraries / library.name)
        for library in sorted(libraries.glob('*.so.*')):
            link = libraries / (library.name.split('.so.', 1)[0] + '.so')
            if not link.exists():
                link.symlink_to(library.name)
        for name in ('asound', 'xcb', 'xcb-xkb', 'xkbcommon', 'xkbcommon-x11',
                     'fontconfig', 'freetype', 'stdc++', 'gcc_s'):
            stem = f'lib{name}.so'
            matches = sorted(libraries.glob(stem + '.*'))
            if not matches:
                raise RuntimeError(f'Linux SDK is missing {stem}')
            link = libraries / stem
            link.unlink(missing_ok=True)
            link.symlink_to(matches[0].name)
        zig = next(stage.glob('zig-*-linux-*'))
        zig.rename(stage / 'zig')
        for name in ('native', 'lib', 'zig'):
            destination = sdk / name
            if destination.exists():
                shutil.rmtree(destination)
            (stage / name).rename(destination)
    wrappers = sdk / 'bin'
    wrappers.mkdir(exist_ok=True)
    for name in ('cc', 'c++', 'linker'):
        path = wrappers / name
        path.unlink(missing_ok=True)
        shutil.copy2(compiler, path)
        path.chmod(0o755)
    marker.write_text(fingerprint)
    return sdk


def build_environment(offline=False):
    sdk = prepare(offline)
    temporary = sdk / 'tmp'
    temporary.mkdir(exist_ok=True)
    return {**os.environ,
            'BOKHEIM_LINUX_SDK': str(sdk),
            'TMPDIR': os.environ.get('TMPDIR', str(temporary)),
            'CC_x86_64_unknown_linux_gnu': str(sdk / 'bin/cc'),
            'CXX_x86_64_unknown_linux_gnu': str(sdk / 'bin/c++'),
            'CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER': str(sdk / 'bin/linker')}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--offline', action='store_true')
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    environment = build_environment(args.offline)
    command = args.command
    if command[:1] == ['--']:
        command = command[1:]
    if command:
        subprocess.run(command, env=environment, check=True)
    else:
        print(environment['BOKHEIM_LINUX_SDK'])


if __name__ == '__main__':
    main()
