#!/usr/bin/env python3
"""Validate Kobo package paths and optionally verify installed bytes or write a manifest."""
import argparse
import hashlib
import struct
from pathlib import Path, PurePosixPath
import tarfile


def minimum_kernel(elf):
    """Read the GNU ABI note without depending on a host-specific readelf."""
    if elf[:6] != b'\x7fELF\x01\x01':
        raise RuntimeError('The Kobo runtime is not a little-endian ELF32 library')
    offset = struct.unpack_from('<I', elf, 28)[0]
    size, count = struct.unpack_from('<HH', elf, 42)
    for index in range(count):
        header = offset + index * size
        if struct.unpack_from('<I', elf, header)[0] != 4:  # PT_NOTE
            continue
        position = struct.unpack_from('<I', elf, header + 4)[0]
        end = position + struct.unpack_from('<I', elf, header + 16)[0]
        while position + 12 <= end:
            name_size, data_size, kind = struct.unpack_from('<III', elf, position)
            position += 12
            name = elf[position:position + name_size]
            position += (name_size + 3) & ~3
            if name == b'GNU\0' and kind == 1 and data_size == 16:
                system, major, minor, patch = struct.unpack_from('<IIII', elf, position)
                if system == 0:
                    return major, minor, patch
            position += (data_size + 3) & ~3
    raise RuntimeError('The Kobo libc has no GNU kernel ABI note')


def verify_multi_device(archive, members):
    files = {str(PurePosixPath(member.name)): member for member in members if member.isfile()}
    info_file = files.get('.adds/bokheim/BUILD-INFO.txt')
    if info_file is None or info_file.size > 8192:
        raise RuntimeError('The multi-device package has no valid build information')
    info = archive.extractfile(info_file).read().decode()
    if 'CPU profile: generic' not in info.splitlines():
        raise RuntimeError('A multi-device package must use the generic CPU profile')
    declarations = [line.removeprefix('Supported model IDs: ') for line in info.splitlines() if line.startswith('Supported model IDs: ')]
    if len(declarations) != 1:
        raise RuntimeError('The package must declare its supported models exactly once')
    ids = [int(value.strip()) for value in declarations[0].split(',')]
    if not ids or len(set(ids)) != len(ids):
        raise RuntimeError('The package model list is empty or contains duplicates')
    for path in ['.adds/bokheim/desktop-gpui-kobo', '.adds/bokheim/pdfium/libpdfium.so', '.adds/bokheim/runtime/libc.so.6']:
        member = files.get(path)
        if member is None:
            raise RuntimeError(f'The package is missing {path}')
        header = archive.extractfile(member).read(20)
        if header[:6] != b'\x7fELF\x01\x01' or header[18:20] != b'\x28\x00':
            raise RuntimeError(f'The package contains a non-ARM32 executable: {path}')
    libc = archive.extractfile(files['.adds/bokheim/runtime/libc.so.6']).read()
    if minimum_kernel(libc) > (2, 6, 32):
        raise RuntimeError('The Kobo libc excludes older Kobo kernels')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', type=Path)
    parser.add_argument('--installed', type=Path)
    parser.add_argument('--manifest', type=Path)
    parser.add_argument('--multi-device', action='store_true', help='Require a generic ARM package and a Linux 2.6.32-compatible runtime')
    arguments = parser.parse_args()
    manifest = []
    with tarfile.open(arguments.archive) as archive:
        members = archive.getmembers()
        if arguments.multi_device:
            verify_multi_device(archive, members)
        total = 0
        for member in members:
            path = PurePosixPath(member.name)
            if path.is_absolute() or '..' in path.parts or not (member.isfile() or member.isdir()):
                raise RuntimeError(f'Unsafe package entry: {member.name}')
            if '\n' in member.name or '\r' in member.name or '\\' in member.name:
                raise RuntimeError(f'Unsupported package filename: {member.name}')
            if path.parts and path.parts[0] not in ('.adds', 'Bokheim'):
                raise RuntimeError(f'Unexpected package entry: {member.name}')
            if path.is_relative_to('Bokheim/Libraries') and path != PurePosixPath('Bokheim/Libraries'):
                raise RuntimeError(f'Package would overwrite a library: {member.name}')
            bundled_taxonomy = path == PurePosixPath('.adds/bokheim/taxonomy.sqlite3')
            if (not bundled_taxonomy and path.suffix in ('.sqlite3', '.sqlite', '.db', '.json', '.sqlite3-wal', '.sqlite3-shm')) or any(
                path.is_relative_to(f'.adds/bokheim/{directory}')
                for directory in ('data', 'libraries', 'remote-book-cache', 'taxonomy')
            ):
                raise RuntimeError(f'Package would overwrite application data: {member.name}')
            total += member.size
            if total > 512 * 1024 * 1024:
                raise RuntimeError('Kobo package is unexpectedly large')
        if arguments.installed is not None or arguments.manifest is not None:
            for member in members:
                if not member.isfile():
                    continue
                path = PurePosixPath(member.name)
                expected = hashlib.file_digest(archive.extractfile(member), 'sha256').hexdigest()
                if arguments.installed is not None:
                    with (arguments.installed / path).open('rb') as installed:
                        if hashlib.file_digest(installed, 'sha256').hexdigest() != expected:
                            raise RuntimeError(f'Installed file failed verification: {member.name}')
                manifest.append(f'{expected}  {path}\n')
    if arguments.manifest is not None:
        arguments.manifest.write_text(''.join(manifest))


if __name__ == '__main__':
    main()
