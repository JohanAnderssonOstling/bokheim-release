import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
LINUX = ROOT / 'scripts/linux'


def module(name):
    spec = importlib.util.spec_from_file_location(name, LINUX / f'{name}.py')
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


class CompilerTests(unittest.TestCase):
    def test_build_scripts_can_load_native_sdk_sonames_on_the_build_host(self):
        compiler = module('compiler')
        arguments = ['main.o', '-o', '/target/build/package/build_script_build-123']
        result = compiler.compiler_arguments(arguments, Path('/sdk'), True)
        self.assertIn('-Wl,-rpath,/sdk/lib', result)
        arguments[-1] = '/target/release/bokheim'
        self.assertNotIn('-Wl,-rpath,/sdk/lib', compiler.compiler_arguments(arguments, Path('/sdk'), True))

    def test_host_targets_and_library_paths_cannot_override_the_sdk(self):
        compiler = module('compiler')
        sdk = Path('/sdk with spaces')
        arguments = ['--target=x86_64-unknown-linux-gnu', '-target', 'host',
                     '-L/usr/lib', '-L', '/usr/lib/x86_64-linux-gnu',
                     '-L', '/project/native', '-lstdc++', '-lgcc_s', 'app.o']
        self.assertEqual(compiler.compiler_arguments(arguments, sdk, True),
                         ['-L', '/project/native', str(sdk / 'lib/libstdc++.so.6'),
                          str(sdk / 'lib/libgcc_s.so.1'), 'app.o'])

    def test_corrupt_cached_download_is_rejected_even_offline(self):
        toolchain = module('toolchain')
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            (cache / 'archive').write_bytes(b'corrupt')
            with self.assertRaisesRegex(RuntimeError, 'checksum mismatch'):
                toolchain.download({'name': 'archive', 'sha256': '0' * 64}, cache, True)


@unittest.skipUnless(sys.platform == 'linux', 'Linux browser SDK')
class BrowserToolchainTests(unittest.TestCase):
    def test_default_build_allows_system_library_references_only_for_the_helper(self):
        browser = module('browser-toolchain')
        with patch.object(sys, 'argv', ['browser-toolchain.py', '--offline']), \
             patch.object(browser, 'build_environment', return_value={'SDK': 'environment'}), \
             patch.object(browser.subprocess, 'run') as run:
            browser.main()
        run.assert_called_once_with(['cargo', 'rustc', '--release', '-p', 'opds-webview', '--offline',
                                     '--', '-C', 'link-arg=-Wl,--allow-shlib-undefined'],
                                    env={'SDK': 'environment'}, check=True)

    def test_pkg_config_wrapper_path_is_absolute_for_cargo_build_script_working_directories(self):
        browser = module('browser-toolchain')
        environment = {'BOKHEIM_LINUX_SDK': '/sdk', 'CC_x86_64_unknown_linux_gnu': '/sdk/bin/cc'}
        with patch.object(browser, '__file__', 'scripts/linux/browser-toolchain.py'), \
             patch.object(browser, 'core_environment', return_value=environment), \
             patch.object(browser, 'browser_libraries', return_value=Path('/sdk/browser/lib')), \
             patch.object(browser.shutil, 'which', return_value='/usr/bin/pkg-config'):
            result = browser.build_environment(offline=True)
        self.assertEqual(Path(result['PKG_CONFIG']), LINUX / 'browser-pkg-config.py')
        self.assertTrue(Path(result['PKG_CONFIG']).is_absolute())
        self.assertEqual(result['CC_x86_64_unknown_linux_gnu'], '/sdk/bin/cc')

    def test_browser_search_path_exposes_gui_libraries_and_preserves_core_sdk_libraries(self):
        browser = module('browser-toolchain')
        with tempfile.TemporaryDirectory(prefix='browser SDK ') as directory:
            root = Path(directory)
            sdk, host = root / 'sdk', root / 'host'
            (sdk / 'lib').mkdir(parents=True)
            host.mkdir()
            (sdk / 'lib/libz.so').write_bytes(b'old SDK zlib')
            for name in ('webkit2gtk-4.1', 'gtk-3', 'dbus-1', 'z', 'c'):
                (host / f'lib{name}.so').write_bytes(b'host library')
            def pkg_config(command, **kwargs):
                if '--libs' in command:
                    return '-lwebkit2gtk-4.1 -lgtk-3 -ldbus-1 -lz -pthread\n'
                return str(host) + '\n'
            with patch.object(browser.subprocess, 'check_output', side_effect=pkg_config):
                libraries = browser.browser_libraries(sdk, 'pkg-config')
            self.assertEqual((libraries / 'libgtk-3.so').resolve(), host / 'libgtk-3.so')
            self.assertEqual((libraries / 'libwebkit2gtk-4.1.so').resolve(), host / 'libwebkit2gtk-4.1.so')
            self.assertEqual((libraries / 'libdbus-1.so').resolve(), host / 'libdbus-1.so')
            self.assertFalse((libraries / 'libz.so').exists())
            self.assertFalse((libraries / 'libc.so').exists())
            self.assertEqual((sdk / 'lib/libz.so').read_bytes(), b'old SDK zlib')

    def test_host_libc_cannot_be_added_to_browser_search_path(self):
        browser = module('browser-toolchain')
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(browser.subprocess, 'check_output', side_effect=['-lc', *([directory] * len(browser.PACKAGES))]):
                with self.assertRaisesRegex(RuntimeError, 'Unexpected browser development library: -lc'):
                    browser.browser_libraries(Path(directory), 'pkg-config')

    def test_pkg_config_wrapper_keeps_headers_versions_and_failures(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            pkg_config = root / 'pkg-config'
            pkg_config.write_text(f'#!{sys.executable}\nimport sys\n'
                                 'if "--fail" in sys.argv: sys.exit(7)\n'
                                 'print("2.38" if "--modversion" in sys.argv else "-I/host/include -lgtk-3")\n')
            pkg_config.chmod(0o755)
            env = {**os.environ, 'BOKHEIM_BROWSER_PKG_CONFIG': str(pkg_config),
                   'BOKHEIM_BROWSER_LIBRARY_PATH': '/browser SDK/lib'}
            wrapper = [sys.executable, str(LINUX / 'browser-pkg-config.py')]
            flags = subprocess.run([*wrapper, '--libs', '--cflags', 'gtk+-3.0'], env=env,
                                   check=True, capture_output=True, text=True).stdout
            self.assertIn('-I/host/include -lgtk-3', flags)
            self.assertIn('-L/browser\\ SDK/lib', flags)
            version = subprocess.run([*wrapper, '--modversion', 'gtk+-3.0'], env=env,
                                     check=True, capture_output=True, text=True).stdout
            self.assertEqual(version, '2.38\n')
            self.assertEqual(subprocess.run([*wrapper, '--fail'], env=env, capture_output=True).returncode, 7)


@unittest.skipUnless(sys.platform == 'linux' and shutil.which('cc'), 'Linux C compiler required')
class LauncherTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='linux launcher ')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.app = self.root / 'AppDir'
        self.binary = self.app / 'usr/bin/probe'
        self.binary.parent.mkdir(parents=True)
        self.fallback = self.app / 'usr/lib/bokheim-compat'
        self.fallback.mkdir(parents=True)
        self.original = self.root / 'system-libraries'
        self.original.mkdir()
        self.env = {**os.environ, 'LD_LIBRARY_PATH': str(self.original)}
        self.env.pop('BOKHEIM_SYSTEM_LD_LIBRARY_PATH', None)

    def launch(self, *arguments):
        return subprocess.run(['sh', str(LINUX / 'launch.sh'), str(self.app), 'probe', *arguments],
                              env=self.env, text=True, capture_output=True, timeout=10)

    def test_system_libraries_are_preferred_and_original_path_is_saved(self):
        self.binary.symlink_to('/bin/sh')
        result = self.launch('-c', 'printf "%s\\n%s\\n%s" "$LD_LIBRARY_PATH" "$BOKHEIM_SYSTEM_LD_LIBRARY_PATH" "$1"',
                             'probe', 'argument with spaces')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), [str(self.original),
                         str(self.original), 'argument with spaces'])

    def test_missing_symbol_version_selects_fallback(self):
        source = self.root / 'runtime.c'
        source.write_text('int bokheim_runtime(void) { return 42; }\n')
        version = self.root / 'version.map'
        for number, destination in [('1', self.original), ('2', self.fallback)]:
            version.write_text(f'BOKHEIM_{number} {{ global: bokheim_runtime; }};\n')
            subprocess.run(['cc', '-shared', '-fPIC', str(source),
                            '-Wl,--version-script=' + str(version),
                            '-Wl,-soname,libbokheim-launch-test.so',
                            '-o', str(destination / 'libbokheim-launch-test.so')], check=True, capture_output=True)
        main = self.root / 'main.c'
        main.write_text('#include <stdio.h>\n#include <stdlib.h>\n'
                        'extern int bokheim_runtime(void);\n'
                        'int main(void) { puts(getenv("LD_LIBRARY_PATH")); return bokheim_runtime() != 42; }\n')
        subprocess.run(['cc', str(main), '-L' + str(self.fallback), '-lbokheim-launch-test',
                        '-o', str(self.binary)], check=True, capture_output=True)
        result = self.launch()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), f'{self.fallback}:{self.original}')


if __name__ == '__main__':
    unittest.main()
