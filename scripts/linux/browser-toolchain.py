#!/usr/bin/env python3
"""Link the browser with the glibc 2.17 SDK and host WebKitGTK development libraries."""
import argparse
from pathlib import Path
import runpy
import shlex
import shutil
import subprocess

PACKAGES = ('webkit2gtk-4.1', 'gdk-x11-3.0', 'libsoup-3.0', 'dbus-1')


def core_environment(offline):
    return runpy.run_path(str(Path(__file__).resolve().with_name('toolchain.py')))['build_environment'](offline)


def browser_libraries(sdk, pkg_config):
    """Expose only explicitly requested GUI libraries, never the host's libc."""
    flags = shlex.split(subprocess.check_output([pkg_config, '--libs', *PACKAGES], text=True))
    directories = [Path(flag[2:]) for flag in flags if flag.startswith('-L')]
    for package in PACKAGES:
        directory = Path(subprocess.check_output([pkg_config, '--variable=libdir', package], text=True).strip())
        if directory not in directories:
            directories.append(directory)
    libraries = sdk / 'browser/lib'
    libraries.mkdir(parents=True, exist_ok=True)
    for flag in flags:
        if not flag.startswith('-l'):
            continue
        name = flag[2:]
        if not name or '/' in name or name in ('c', 'm', 'pthread', 'dl', 'rt', 'stdc++', 'gcc_s'):
            raise RuntimeError(f'Unexpected browser development library: {flag}')
        filename = f'lib{name}.so'
        if (sdk / 'lib' / filename).exists():
            continue
        source = next((directory / filename for directory in directories if (directory / filename).is_file()), None)
        if source is None:
            raise RuntimeError(f'Browser development library not found: {filename}')
        destination = libraries / filename
        if not destination.is_symlink() or destination.resolve() != source.resolve():
            destination.unlink(missing_ok=True)
            destination.symlink_to(source.resolve())
    return libraries


def build_environment(offline=False):
    environment = core_environment(offline)
    pkg_config = shutil.which('pkg-config')
    if not pkg_config:
        raise RuntimeError('The browser build needs pkg-config')
    libraries = browser_libraries(Path(environment['BOKHEIM_LINUX_SDK']), pkg_config)
    return {**environment,
            'PKG_CONFIG': str(Path(__file__).resolve().with_name('browser-pkg-config.py')),
            'BOKHEIM_BROWSER_PKG_CONFIG': pkg_config,
            'BOKHEIM_BROWSER_LIBRARY_PATH': str(libraries)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--offline', action='store_true')
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    environment = build_environment(args.offline)
    command = args.command[1:] if args.command[:1] == ['--'] else args.command
    if not command:
        command = ['cargo', 'rustc', '--release', '-p', 'opds-webview']
        if args.offline:
            command.append('--offline')
        # Host GUI libraries are not bundled. Their internal libc references
        # resolve against the user's system, while the helper targets 2.17.
        command += ['--', '-C', 'link-arg=-Wl,--allow-shlib-undefined']
    subprocess.run(command, env=environment, check=True)


if __name__ == '__main__':
    main()
