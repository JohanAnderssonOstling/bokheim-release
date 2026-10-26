import importlib.util
import lzma
from pathlib import Path
import shutil
import struct
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location('linux_toolchain', ROOT / 'scripts/linux/toolchain.py')
toolchain = importlib.util.module_from_spec(spec)
spec.loader.exec_module(toolchain)


def entry(name, contents, mode, inode):
    name = name.encode() + b'\0'
    fields = [inode, mode, 0, 0, 1, 0, len(contents), 0, 0, 0, 0, len(name), 0]
    header = b'070701' + ''.join(f'{field:08x}' for field in fields).encode()
    data = header + name
    data += b'\0' * (-len(data) % 4)
    data += contents
    return data + b'\0' * (-len(contents) % 4)


@unittest.skipUnless(shutil.which('bsdtar'), 'libarchive tools are required')
class LinuxSdkExtractionTests(unittest.TestCase):
    def test_leading_library_symlink_creates_missing_parent_directories(self):
        # CentOS libxcb starts with this symlink and omits directory entries.
        payload = entry('./usr/lib64/libexample.so.0', b'libexample.so.0.0.0', 0o120777, 1)
        payload += entry('./usr/lib64/libexample.so.0.0.0', b'library contents', 0o100755, 2)
        payload += entry('TRAILER!!!', b'', 0, 0)
        empty_header = b'\x8e\xad\xe8\x01' + b'\0' * 4 + struct.pack('>II', 0, 0)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / 'library.rpm'
            archive.write_bytes(b'\0' * 96 + empty_header * 2 + lzma.compress(payload))
            destination = root / 'sdk'
            destination.mkdir()
            toolchain.extract_rpm(archive, destination)
            link = destination / 'usr/lib64/libexample.so.0'
            self.assertTrue(link.is_symlink())
            self.assertEqual(link.read_bytes(), b'library contents')
