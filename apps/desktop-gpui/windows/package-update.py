#!/usr/bin/env python3
"""Package the optimized Windows runtime for signed updates and portable use."""
import argparse
import hashlib
from pathlib import Path
import shutil
import tempfile
import zipfile


def package(binary, icon, output, portable_output=None):
    if portable_output is not None and portable_output.resolve() == output.resolve():
        raise ValueError('Update and portable packages must have different output paths')
    binary = binary.resolve(strict=True)
    files = {'Bokheim.exe': binary, 'Bokheim.ico': icon.resolve(strict=True),
             'bokheim-opds-webview.exe': binary.with_name('bokheim-opds-webview.exe').resolve(strict=True)}
    files['taxonomy.sqlite3'] = (binary.parent / 'taxonomy.sqlite3').resolve(strict=True)
    speech = binary.parent / 'tts'
    if not (speech / 'alba/en_GB-alba-medium.onnx').is_file() or not (speech / 'george/model.onnx').is_file():
        raise ValueError('Prepare both desktop speech voices before packaging')
    files.update({path.relative_to(binary.parent).as_posix(): path for path in speech.rglob('*') if path.is_file()})
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
        if portable_output is not None:
            # Both downloads contain the same complete runtime. Compress the large
            # speech models once, retaining separate names and checksum sidecars.
            portable_output.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(output, portable_output)
            portable_output.with_suffix(portable_output.suffix + '.sha256').write_text(
                f'{digest.hexdigest()}  {portable_output.name}\n', encoding='ascii')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path, help='Release bokheim.exe')
    parser.add_argument('--icon', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--portable-output', type=Path, help='Also write a complete standalone ZIP without an installer')
    args = parser.parse_args()
    package(args.binary, args.icon, args.output, args.portable_output)


if __name__ == '__main__':
    main()
