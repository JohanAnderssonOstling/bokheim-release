import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
APPIMAGE = ROOT / 'apps/desktop-gpui/linux/appimage'

# Exercise the actual packaging/check scripts without downloading release tools,
# PDFium or voices. ELF fixtures describe their required symbol versions.
TOOLS = '''import os
from pathlib import Path
import shutil
import sys

tool, args = Path(sys.argv[0]).name, sys.argv[1:]
if tool == 'file':
    data = Path(args[-1]).read_bytes()
    print('ELF 64-bit LSB executable' if data.startswith(b'ELF:') else 'data')
elif tool == 'readelf':
    version = Path(args[-1]).read_text().split(':')[1]
    print('  0x0010: Name: GLIBC_' + version + '  Flags: none  Version: 2')
elif tool == 'python3':
    if args[0].endswith('pdfium/prepare.py'):
        dest = Path(args[args.index('--dest') + 1])
        dest.mkdir(parents=True)
        (dest / 'libpdfium.so').write_text('ELF:2.16')
    elif args[0].endswith('apps/reader/crates/speech/prepare.py'):
        Path(args[args.index('--dest') + 1]).mkdir(parents=True)
    elif args[0].endswith('linux/stage-runtime.py'):
        dest = Path(args[1]) / 'usr/lib/bokheim-compat'
        dest.mkdir(parents=True)
        (dest / 'libstdc++.so.6').write_text('ELF:2.17')
    else:
        raise RuntimeError(args)
elif tool == 'linuxdeploy':
    if '--executable' in args:
        source = Path(args[args.index('--executable') + 1])
        dest = Path(args[args.index('--appdir') + 1]) / 'usr/bin' / source.name
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, dest)
    else:
        assert '--deploy-deps-only' in args
elif tool == 'appimagetool':
    shutil.copytree(args[-2], os.environ['CAPTURE_DIRECTORY'])
    Path(args[-1]).write_text('ELF:' + os.environ.get('RUNTIME_GLIBC', '2.17'))
else:
    raise RuntimeError(tool)
'''


class AppImageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='appimage test ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        tools = self.root / 'tools'
        tools.mkdir()
        for name in ('file', 'readelf', 'python3', 'linuxdeploy', 'appimagetool'):
            path = tools / name
            path.write_text(f'#!{sys.executable}\n' + TOOLS)
            path.chmod(0o755)
        self.env = {**os.environ, 'PATH': str(tools) + os.pathsep + os.environ['PATH']}
        for name in ('BOKHEIM_BROWSER', 'BOKHEIM_BROWSER_HELPER'):
            self.env.pop(name, None)
        self.appdir = self.root / 'AppDir'
        self.binary = self.elf(self.appdir / 'usr/bin/bokheim', '2.17')

    def elf(self, path, version):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text('ELF:' + version)
        path.chmod(0o755)
        return path

    def run_script(self, name, *args, **env):
        return subprocess.run(['bash', str(APPIMAGE / name), *map(str, args)],
                              env={**self.env, **env}, capture_output=True, text=True, timeout=20)

    def check(self):
        return self.run_script('verify-appdir-compatibility.sh', self.appdir)

    def test_core_only_and_optional_browser_pass_the_same_baseline(self):
        self.assertEqual(self.check().returncode, 0)
        self.elf(self.appdir / 'usr/bin/bokheim-browser', '2.17')
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('GLIBC_2.17', result.stdout)
        self.assertEqual(result.stdout.count('GLIBC_2.17'), 2)

    def test_browser_limit_is_still_enforced(self):
        self.elf(self.appdir / 'usr/bin/bokheim-browser', '2.18')
        self.assertNotEqual(self.check().returncode, 0)

    def test_no_other_binary_or_library_can_bypass_core_limit(self):
        for relative in ('usr/bin/bokheim', 'usr/lib/libspeech.so', 'usr/bin/other-helper'):
            with self.subTest(relative=relative):
                path = self.elf(self.appdir / relative, '2.18')
                self.assertNotEqual(self.check().returncode, 0)
                if path == self.binary:
                    self.elf(path, '2.17')
                else:
                    path.unlink()

    def package(self, **env):
        (self.binary.parent / 'taxonomy.sqlite3').write_bytes(b'taxonomy')
        fonts = self.binary.parent / 'fonts'
        fonts.mkdir(exist_ok=True)
        (fonts / 'DroidSansFallback.ttf').write_bytes(b'font')
        return self.run_script('build-appimage.sh',
                               DESKTOP_BINARY=str(self.binary),
                               RELEASE_DIRECTORY=str(self.root / 'release'),
                               CAPTURE_DIRECTORY=str(self.root / 'captured'),
                               LINUXDEPLOY='linuxdeploy', APPIMAGETOOL='appimagetool', **env)

    def test_packaging_without_helper_succeeds(self):
        result = self.package()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / 'captured/usr/bin/bokheim-browser').exists())
        self.assertTrue((self.root / 'release/Bokheim-x86_64.AppImage.sha256').exists())
        installer = self.root / 'release/install.sh'
        self.assertEqual(installer.read_bytes(), (APPIMAGE.parent / 'install.sh').read_bytes())
        self.assertTrue(os.access(installer, os.X_OK))
        registration = self.root / 'captured/usr/libexec/bokheim-register-desktop'
        self.assertEqual(registration.read_bytes(), (APPIMAGE / 'register-desktop.sh').read_bytes())
        self.assertTrue(os.access(registration, os.X_OK))
        for name in ('LICENSE', 'COPYRIGHT'):
            self.assertEqual((self.root / 'captured/usr/share/licenses/bokheim' / name).read_bytes(),
                             (ROOT / name).read_bytes())

    def test_separately_built_browser_is_packaged_without_its_libraries(self):
        helper = self.elf(self.root / 'separate build/bokheim-browser', '2.17')
        self.elf(helper.parent / 'libwebkit2gtk-4.1.so', '2.39')
        result = self.package(BOKHEIM_BROWSER_HELPER=str(helper))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / 'captured/usr/bin/bokheim-browser').read_text(), 'ELF:2.17')
        self.assertFalse(list((self.root / 'captured').rglob('libwebkit*')))

    def test_disabled_browser_does_not_package_a_stale_helper(self):
        self.elf(self.binary.with_name('bokheim-browser'), '2.44')
        result = self.package(BOKHEIM_BROWSER='disabled')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / 'captured/usr/bin/bokheim-browser').exists())

    def test_required_or_explicit_missing_helper_fails(self):
        for env in ({'BOKHEIM_BROWSER': 'required'}, {'BOKHEIM_BROWSER_HELPER': str(self.root / 'missing')},
                    {'BOKHEIM_BROWSER': 'auto', 'BOKHEIM_BROWSER_HELPER': str(self.root / 'missing')}):
            with self.subTest(env=env):
                result = self.package(**env)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('Browser helper is missing', result.stderr)

    def test_packaging_rejects_newer_core_and_browser_before_creating_artifact(self):
        for relative, version in (('bokheim', '2.18'), ('bokheim-browser', '2.18')):
            with self.subTest(relative=relative):
                self.elf(self.binary.with_name(relative), version)
                result = self.package()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('newer than supported', result.stderr)
                self.assertFalse((self.root / 'release/Bokheim-x86_64.AppImage').exists())
                self.elf(self.binary, '2.17')

    def test_newer_appimage_runtime_cannot_pass_release_checks(self):
        result = self.package(RUNTIME_GLIBC='2.18')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('newer than supported GLIBC_2.17', result.stderr)
        self.assertFalse((self.root / 'release/Bokheim-x86_64.AppImage.sha256').exists())

    def test_relr_loader_requirement_is_checked(self):
        self.elf(self.binary, 'ABI_DT_RELR')
        result = self.check()
        self.assertNotEqual(result.returncode, 0)

    def test_kobo_installer_has_the_same_core_and_runtime_limit(self):
        script = ROOT / 'apps/kobo-installer/linux/build-appimage.sh'
        for core, runtime in [('2.17', '2.17'), ('2.18', '2.17'), ('2.17', '2.18')]:
            with self.subTest(core=core, runtime=runtime):
                output = self.root / f'installer-{core}-{runtime}'
                installer = self.elf(self.root / 'bokheim-kobo-installer', core)
                result = subprocess.run(['bash', str(script)], env={**self.env,
                    'INSTALLER_BINARY': str(installer), 'RELEASE_DIRECTORY': str(output),
                    'CAPTURE_DIRECTORY': str(self.root / f'capture-{core}-{runtime}'),
                    'LINUXDEPLOY': 'linuxdeploy', 'APPIMAGETOOL': 'appimagetool',
                    'RUNTIME_GLIBC': runtime}, capture_output=True, text=True, timeout=20)
                self.assertEqual(result.returncode == 0, core == runtime == '2.17', result.stderr)
                self.assertEqual((output / 'Bokheim-Kobo-Installer-Linux-x86_64.AppImage.sha256').exists(),
                                 core == runtime == '2.17')


if __name__ == '__main__':
    unittest.main()
