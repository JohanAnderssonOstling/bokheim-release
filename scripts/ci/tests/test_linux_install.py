import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
INSTALLER = ROOT / 'apps/desktop-gpui/linux/install.sh'
IMAGE = '''#!/usr/bin/env bash
set -eu
if [[ ${1-} == --appimage-extract ]]; then
  [[ $# == 2 && $2 == usr/share/icons/hicolor/512x512/apps/se.bokheim.Bokheim.png ]]
  mkdir -p squashfs-root/usr/share/icons/hicolor/512x512/apps
  printf 'image icon' > squashfs-root/usr/share/icons/hicolor/512x512/apps/se.bokheim.Bokheim.png
else
  printf '%s\\n' "$@"
fi
'''


@unittest.skipUnless(sys.platform == 'linux', 'Linux installer')
class LinuxInstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='bokheim install ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.data = self.root / "user's data"
        self.bin = self.root / 'user bin'
        self.tools = self.root / 'tools'
        self.tools.mkdir()
        self.env = {**os.environ, 'XDG_DATA_HOME': str(self.data),
                    'BOKHEIM_BIN_DIR': str(self.bin),
                    'PATH': str(self.tools) + os.pathsep + os.environ['PATH']}
        # Avoid refreshing the host desktop session during isolated installation tests.
        for command in ('kbuildsycoca5', 'kbuildsycoca6', 'update-desktop-database', 'gtk-update-icon-cache'):
            self.tool(command, '#!/bin/sh\nexit 0\n')
        self.tool('uname', '#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo x86_64;; esac\n')
        self.image = self.root / 'downloaded Bokheim (1).AppImage'
        self.image.write_text(IMAGE)
        self.checksum = Path(str(self.image) + '.sha256')
        self.checksum.write_text(hashlib.sha256(self.image.read_bytes()).hexdigest() + '  Bokheim-x86_64.AppImage\n')
        self.installed = self.data / 'bokheim/application/Bokheim.AppImage'
        self.desktop = self.data / 'applications/se.bokheim.Bokheim.desktop'

    def tool(self, name, text):
        path = self.tools / name
        path.write_text(text)
        path.chmod(0o755)
        return path

    def run_install(self, *args):
        return subprocess.run(['bash', str(INSTALLER), *map(str, args)],
                              env=self.env, capture_output=True, text=True, timeout=20)

    def local_install(self):
        return self.run_install('--appimage', self.image)

    def assert_installed(self):
        self.assertEqual(self.installed.read_bytes(), self.image.read_bytes())
        self.assertTrue(os.access(self.installed, os.X_OK))
        self.assertEqual((self.bin / 'bokheim').resolve(), self.installed)
        self.assertIn(f'Exec="{self.installed}"\n', self.desktop.read_text())
        self.assertEqual((self.data / 'icons/hicolor/512x512/apps/se.bokheim.Bokheim.png').read_text(), 'image icon')
        self.assertFalse(list(self.installed.parent.glob('.install-*')))

    def test_local_install_integrates_menu_and_passes_launch_arguments(self):
        result = self.local_install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_installed()
        launch = subprocess.run([str(self.bin / 'bokheim'), 'a book.epub', '--bokheim-update-info'],
                                capture_output=True, text=True, check=True)
        self.assertEqual(launch.stdout.splitlines(), ['a book.epub', '--bokheim-update-info'])
        if shutil.which('desktop-file-validate'):
            subprocess.run(['desktop-file-validate', str(self.desktop)], capture_output=True, text=True, check=True)

    def test_reinstall_replaces_developer_symlink_and_preserves_previous_launcher(self):
        self.bin.mkdir()
        old_target = self.root / 'developer/target/release/bokheim'
        (self.bin / 'bokheim').symlink_to(old_target)
        self.desktop.parent.mkdir(parents=True)
        previous_desktop = '[Desktop Entry]\nExec=/old/launcher\n'
        self.desktop.write_text(previous_desktop)
        library = self.data / 'bokheim/libraries/personal.epub'
        library.parent.mkdir(parents=True)
        library.write_bytes(b'user library')
        for _ in range(2):
            result = self.local_install()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assert_installed()
        self.assertEqual((self.bin / 'bokheim.before-install').readlink(), old_target)
        self.assertEqual(Path(str(self.desktop) + '.before-install').read_text(), previous_desktop)
        self.assertEqual(library.read_bytes(), b'user library')
        self.assertFalse(old_target.exists())

    def test_bad_or_missing_checksum_keeps_existing_installation(self):
        self.assertEqual(self.local_install().returncode, 0)
        for checksum in ('0' * 64, 'invalid', None):
            with self.subTest(checksum=checksum):
                if checksum is None:
                    self.checksum.unlink()
                else:
                    self.checksum.write_text(checksum + '\n')
                self.image.write_text('unverified executable')
                result = self.local_install()
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.installed.read_text(), IMAGE)
                self.assertEqual((self.bin / 'bokheim').resolve(), self.installed)
                self.assertFalse(list(self.installed.parent.glob('.install-*')))

    def test_failed_extraction_does_not_change_existing_installation(self):
        self.assertEqual(self.local_install().returncode, 0)
        self.image.write_text('#!/bin/sh\nexit 1\n')
        self.checksum.write_text(hashlib.sha256(self.image.read_bytes()).hexdigest() + '\n')
        self.assertNotEqual(self.local_install().returncode, 0)
        self.assertEqual(self.installed.read_text(), IMAGE)

    def test_download_resolves_latest_once_and_checks_same_release(self):
        log = self.root / 'downloads'
        self.env.update(INSTALL_TEST_IMAGE=str(self.image), INSTALL_TEST_LOG=str(log))
        self.tool('curl', f'''#!{sys.executable}
import hashlib, os
from pathlib import Path
import sys
args = sys.argv[1:]
url = args[-1]
with open(os.environ['INSTALL_TEST_LOG'], 'a') as stream:
    stream.write(url + '\\n')
if url.endswith('/latest'):
    print('https://github.com/JohanAnderssonOstling/bokheim-release/releases/tag/v1.2.3', end='')
else:
    output = Path(args[args.index('--output') + 1])
    image = Path(os.environ['INSTALL_TEST_IMAGE']).read_bytes()
    output.write_bytes((hashlib.sha256(image).hexdigest() + '  Bokheim-x86_64.AppImage\\n').encode()
                       if url.endswith('.sha256') else image)
''')
        result = self.run_install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_installed()
        base = 'https://github.com/JohanAnderssonOstling/bokheim-release/releases/'
        self.assertEqual(log.read_text().splitlines(), [base + 'latest',
            base + 'download/v1.2.3/Bokheim-x86_64.AppImage',
            base + 'download/v1.2.3/Bokheim-x86_64.AppImage.sha256'])
        log.write_text('')
        result = self.run_install('--version', 'v0.1.10')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(log.read_text().splitlines(), [base + 'download/v0.1.10/Bokheim-x86_64.AppImage',
            base + 'download/v0.1.10/Bokheim-x86_64.AppImage.sha256'])

    def test_unsupported_architecture_and_invalid_options_make_no_installation(self):
        for args in (('--version',), ('--appimage',), ('--version', '1.2.3', '--appimage', str(self.image)),
                     ('--unknown',)):
            with self.subTest(args=args):
                self.assertNotEqual(self.run_install(*args).returncode, 0)
                self.assertFalse(self.installed.exists())
        self.tool('uname', '#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo aarch64;; esac\n')
        result = self.local_install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('x86-64', result.stderr)
        self.assertFalse(self.installed.parent.exists())


if __name__ == '__main__':
    unittest.main()
