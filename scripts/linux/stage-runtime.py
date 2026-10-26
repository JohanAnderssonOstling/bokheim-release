#!/usr/bin/env python3
"""Stage ABI-compatible X11 libraries and a fallback GNU C++ runtime."""
import argparse
from pathlib import Path
import shutil

from toolchain import prepare


def stage(appdir, sdk):
    libraries = appdir / 'usr/lib'
    libraries.mkdir(parents=True, exist_ok=True)
    # Fontconfig, FreeType, ALSA and graphics drivers come from the distribution.
    # Avoid freezing their parsers, device configuration or drivers in the image.
    fallback = libraries / 'bokheim-compat'
    fallback.mkdir(exist_ok=True)
    for name in ('libxcb.so.1', 'libxcb-xkb.so.1', 'libxkbcommon.so.0',
                 'libxkbcommon-x11.so.0', 'libXau.so.6', 'libstdc++.so.6', 'libgcc_s.so.1'):
        shutil.copy2(sdk / 'lib' / name, fallback / name)
    # The launcher selects this fallback only when system libraries cannot load the app.
    # Modern GPU drivers can therefore use their matching C++ and X11 libraries.
    # Do not bundle XCB driver extensions such as DRI3 or Present.
    launch = appdir / 'usr/libexec/bokheim-launch'
    launch.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(Path(__file__).with_name('launch.sh'), launch)
    launch.chmod(0o755)
    notices = appdir / 'usr/share/doc/bokheim-linux-runtime'
    for directory in ('info/licenses', 'info/recipe/parent', 'share/licenses', 'usr/share/licenses', 'usr/share/doc'):
        source = sdk / 'native' / directory
        if source.exists():
            shutil.copytree(source, notices / directory, dirs_exist_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('appdir', type=Path)
    args = parser.parse_args()
    sdk = prepare(offline=True)
    stage(args.appdir, sdk)


if __name__ == '__main__':
    main()
