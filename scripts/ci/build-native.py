#!/usr/bin/env python3
"""Build a native app and stage its generated resources beside the binaries.

Usage: python3 scripts/ci/build-native.py cargo desktop --release
Cargo reports build-script output directories even when it reuses cached builds.
Use that exact output rather than searching possibly stale build directories.
"""
import json
from pathlib import Path
import shutil
import subprocess
import sys


def main():
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    outputs = set()
    profiles = set()
    build_scripts = set()
    command = [*sys.argv[1:], '--message-format=json,json-render-diagnostics']
    with subprocess.Popen(command, stdout=subprocess.PIPE, text=True, encoding='utf-8', errors='replace') as process:
        for line in process.stdout:
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                print(line, end='', flush=True)
                continue
            package = message.get('package_id', '')
            taxonomy = '/subject-projection#' in package or '#subject-projection@' in package
            if message.get('reason') == 'build-script-executed':
                build_scripts.add(package)
                if taxonomy:
                    outputs.add(Path(message['out_dir']))
            elif message.get('reason') == 'compiler-artifact' and taxonomy and 'lib' in message['target']['kind']:
                for filename in message['filenames']:
                    artifact = Path(filename)
                    if artifact.suffix in ('.rlib', '.rmeta'):
                        profile = artifact.parent
                        profiles.add(profile.parent if profile.name == 'deps' else profile)
            elif message.get('reason') == 'compiler-message':
                rendered = message['message'].get('rendered')
                if rendered:
                    print(rendered, end='', file=sys.stderr, flush=True)
        status = process.wait()
    if status:
        raise SystemExit(status)
    if len(outputs) != 1 or len(profiles) != 1:
        raise SystemExit(f'Expected one native taxonomy output and profile, got {outputs}, {profiles}; build scripts: {sorted(build_scripts)}')
    output, = outputs
    profile, = profiles
    for source, name in [('unified-taxonomy-client-v2.sqlite3', 'taxonomy.sqlite3'), ('taxonomy.digest', 'taxonomy.digest')]:
        shutil.copyfile(output / source, profile / name)
    shutil.copytree(Path('apps/desktop-gpui/assets/fonts'), profile / 'fonts', dirs_exist_ok=True)
    print(f'Staged native taxonomy in {profile}', flush=True)


if __name__ == '__main__':
    main()
