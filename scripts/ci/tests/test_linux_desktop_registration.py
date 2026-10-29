import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
APPIMAGE = ROOT / 'apps/desktop-gpui/linux/appimage'


@unittest.skipUnless(sys.platform == 'linux', 'Linux AppImage desktop registration')
class DesktopRegistrationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='bokheim registration ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.data = self.root / "user's application data"
        self.bin = self.root / 'user bin'
        self.tools = self.root / 'tools'
        self.tools.mkdir()
        self.launches = self.root / 'launches.jsonl'
        self.cache_calls = self.root / 'cache-calls'
        self.env = {**os.environ, 'XDG_DATA_HOME': str(self.data), 'BOKHEIM_BIN_DIR': str(self.bin),
                    'PATH': str(self.tools) + os.pathsep + os.environ['PATH'],
                    'REGISTRATION_TEST_LAUNCHES': str(self.launches)}
        for name in ('APPIMAGE', 'APPDIR', 'BOKHEIM_DISABLE_SELF_UPDATE', 'BOKHEIM_DISABLE_DESKTOP_INTEGRATION'):
            self.env.pop(name, None)
        for name in ('kbuildsycoca6', 'kbuildsycoca5', 'gtk-update-icon-cache', 'update-desktop-database'):
            tool = self.tools / name
            tool.write_text(f'#!{sys.executable}\nfrom pathlib import Path\nwith Path({str(self.cache_calls)!r}).open("a") as f: f.write({name!r} + "\\n")\n')
            tool.chmod(0o755)
        self.desktop = self.data / 'applications/se.bokheim.Bokheim.desktop'

    def image(self, version, name):
        appdir = self.root / (name + ' AppDir')
        helpers = appdir / 'usr/libexec'
        helpers.mkdir(parents=True)
        shutil.copy2(APPIMAGE / 'AppRun', appdir / 'AppRun')
        shutil.copy2(APPIMAGE / 'register-desktop.sh', helpers / 'bokheim-register-desktop')
        (helpers / 'bokheim-register-desktop').chmod(0o755)
        icon = appdir / 'usr/share/icons/hicolor/512x512/apps/se.bokheim.Bokheim.png'
        icon.parent.mkdir(parents=True)
        icon.write_text('icon ' + version)
        version_file = appdir / 'usr/share/bokheim/version'
        version_file.parent.mkdir(parents=True)
        version_file.write_text(version + '\n')
        launch = helpers / 'bokheim-launch'
        launch.write_text(f'''#!{sys.executable}
import json, os, sys
from pathlib import Path
version = (Path(sys.argv[1]) / 'usr/share/bokheim/version').read_text().strip()
args = sys.argv[3:]
if args == ['--bokheim-update-info']:
    print(json.dumps({{'application': version}}))
else:
    with open(os.environ['REGISTRATION_TEST_LAUNCHES'], 'a') as stream:
        stream.write(json.dumps({{'version': version, 'appimage': os.environ.get('APPIMAGE'), 'args': args}}) + '\\n')
''')
        launch.chmod(0o755)
        image = self.root / (name + '.AppImage')
        image.write_text(f'''#!{sys.executable}
import os, sys
from pathlib import Path
appdir = Path({str(appdir)!r})
env = {{**os.environ, 'APPIMAGE': os.path.realpath(sys.argv[0]), 'APPDIR': str(appdir)}}
os.execve(str(appdir / 'AppRun'), [str(appdir / 'AppRun'), *sys.argv[1:]], env)
''')
        image.chmod(0o755)
        return image, appdir

    def run_image(self, image, *args, env=None):
        return subprocess.run([str(image), *args], env={**self.env, **(env or {})},
                              capture_output=True, text=True, timeout=20)

    def launched(self):
        return [json.loads(line) for line in self.launches.read_text().splitlines()]

    def test_first_launch_registers_current_image_without_copying_or_relaunching(self):
        image, _ = self.image('1.2.3', 'downloaded Bokheim (1)')
        original = image.read_bytes()
        result = self.run_image(image, 'a book.epub', '--example')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(image.read_bytes(), original)
        self.assertEqual(self.launched(), [{'version': '1.2.3', 'appimage': str(image),
                                           'args': ['a book.epub', '--example']}])
        self.assertIn(f'Exec="{image}"\n', self.desktop.read_text())
        self.assertEqual((self.bin / 'bokheim').resolve(), image)
        self.assertEqual((self.data / 'icons/hicolor/512x512/apps/se.bokheim.Bokheim.png').read_text(), 'icon 1.2.3')
        self.assertFalse(list(self.data.rglob('*.AppImage')))
        self.assertFalse(list(self.data.rglob('.register-*')))
        if shutil.which('desktop-file-validate'):
            subprocess.run(['desktop-file-validate', str(self.desktop)], check=True, capture_output=True)

    def test_launch_after_moving_and_renaming_repairs_menu_and_command(self):
        image, _ = self.image('1.2.3', 'download')
        self.assertEqual(self.run_image(image).returncode, 0)
        icon = self.data / 'icons/hicolor/512x512/apps/se.bokheim.Bokheim.png'
        icon_modified = icon.stat().st_mtime_ns
        moved = self.root / "user's AppImages" / 'Bokheim renamed.AppImage'
        moved.parent.mkdir()
        image.rename(moved)
        self.assertIn(f'Exec="{image}"\n', self.desktop.read_text())
        self.assertFalse((self.bin / 'bokheim').exists())

        result = self.run_image(moved, 'a book.epub')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f'Exec="{moved}"\n', self.desktop.read_text())
        self.assertEqual((self.bin / 'bokheim').resolve(), moved)
        self.assertEqual(self.launched()[-1]['appimage'], str(moved))
        self.assertEqual(icon.stat().st_mtime_ns, icon_modified)
        self.assertFalse(list(self.data.rglob('*.AppImage')))
        self.assertEqual(self.run_image(self.bin / 'bokheim').returncode, 0)
        self.assertEqual(self.launched()[-1]['appimage'], str(moved))

    def test_repeated_launches_leave_registration_and_caches_unchanged(self):
        image, _ = self.image('1.2.3', 'download')
        self.assertEqual(self.run_image(image).returncode, 0)
        modified = self.desktop.stat().st_mtime_ns
        image_modified = image.stat().st_mtime_ns
        cache_calls = self.cache_calls.read_bytes()
        for path in (image, self.bin / 'bokheim'):
            result = self.run_image(path)
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.desktop.stat().st_mtime_ns, modified)
        self.assertEqual(image.stat().st_mtime_ns, image_modified)
        self.assertEqual(self.cache_calls.read_bytes(), cache_calls)

    def test_registration_follows_launched_image_without_overwriting_other_versions(self):
        old, _ = self.image('1.2.3', 'old')
        new, _ = self.image('1.2.4', 'new')
        old_bytes, new_bytes = old.read_bytes(), new.read_bytes()
        for path, expected in ((old, '1.2.3'), (new, '1.2.4'), (old, '1.2.3')):
            result = self.run_image(path)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(self.launched()[-1]['version'], expected)
            self.assertIn(f'Exec="{path}"\n', self.desktop.read_text())
            self.assertEqual((self.bin / 'bokheim').resolve(), path)
        self.assertEqual(old.read_bytes(), old_bytes)
        self.assertEqual(new.read_bytes(), new_bytes)
        self.assertEqual((self.data / 'icons/hicolor/512x512/apps/se.bokheim.Bokheim.png').read_text(), 'icon 1.2.3')
        self.assertFalse(list(self.data.rglob('*.AppImage')))

    def test_headless_checks_update_trials_and_native_runs_do_not_register(self):
        image, appdir = self.image('1.2.3', 'download')
        for args in (('--bokheim-update-info',), ('--bokheim-prepare-update', 'job.json'),
                     ('--bokheim-update-trial', 'trial-id')):
            result = self.run_image(image, *args)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(self.data.exists())
        result = subprocess.run([str(appdir / 'AppRun')], env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.data.exists())
        for name in ('BOKHEIM_DISABLE_SELF_UPDATE', 'BOKHEIM_DISABLE_DESKTOP_INTEGRATION'):
            result = self.run_image(image, env={name: '1'})
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(self.data.exists())

    def test_registration_replaces_development_entry_and_keeps_backup(self):
        self.desktop.parent.mkdir(parents=True)
        old_desktop = '[Desktop Entry]\nExec=/developer/target/release/bokheim\n'
        self.desktop.write_text(old_desktop)
        self.bin.mkdir()
        (self.bin / 'bokheim').symlink_to('/developer/target/release/bokheim')
        image, _ = self.image('1.2.3', 'download')
        result = self.run_image(image)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(Path(str(self.desktop) + '.before-install').read_text(), old_desktop)
        self.assertEqual((self.bin / 'bokheim.before-install').readlink(), Path('/developer/target/release/bokheim'))
        self.assertIn(str(image), self.desktop.read_text())

    def test_registration_failure_keeps_application_usable(self):
        image, _ = self.image('1.2.3', 'download')
        self.data.write_text('not a directory')
        result = self.run_image(image)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('continuing from the current location', result.stderr)
        self.assertEqual(self.launched()[-1]['appimage'], str(image))


if __name__ == '__main__':
    unittest.main()
