#!/usr/bin/env python3
"""Package the optimized Windows build and its runtime files for signed updates."""
import argparse
import hashlib
from pathlib import Path
import tempfile
import zipfile


def package(binary, icon, output):
    binary = binary.resolve(strict=True)
    files = {'Bokheim.exe': binary, 'Bokheim.ico': icon.resolve(strict=True)}
    with binary.open('rb') as stream:
        if stream.read(2) != b'MZ':
            raise ValueError('Bokheim.exe is not a Windows PE image')
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=output.parent, prefix='.windows-update-') as stage:
        archive = Path(stage) / output.name
        with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED, compresslevel=9) as writer:
            for name, path in sorted(files.items()):
                # Fixed timestamps and explicit names avoid filesystem-dependent
                # ZIP metadata. The complete outer archive is manifest-authenticated.
                entry = zipfile.ZipInfo(name, date_time=(2020, 1, 1, 0, 0, 0))
                entry.compress_type = zipfile.ZIP_DEFLATED
                entry.external_attr = 0o100644 << 16
                with path.open('rb') as source, writer.open(entry, 'w', force_zip64=True) as target:
                    while block := source.read(1024 * 1024):
                        target.write(block)
        digest = hashlib.sha256()
        with archive.open('rb') as stream:
            while block := stream.read(1024 * 1024):
                digest.update(block)
        archive.replace(output)
        output.with_suffix(output.suffix + '.sha256').write_text(f'{digest.hexdigest()}  {output.name}\n', encoding='ascii')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path, help='Release desktop-gpui.exe')
    parser.add_argument('--icon', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    package(args.binary, args.icon, args.output)


if __name__ == '__main__':
    main()
