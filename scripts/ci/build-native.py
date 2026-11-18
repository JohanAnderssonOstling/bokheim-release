#!/usr/bin/env python3
"""Build a native app and stage its generated resources beside the binaries.

Usage: python3 scripts/ci/build-native.py cargo desktop --release
Cargo reports build-script output directories even when it reuses cached builds.
Use that exact output rather than searching possibly stale build directories.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


def main():
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    outputs = set()
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
            elif message.get('reason') == 'compiler-message':
                rendered = message['message'].get('rendered')
                if rendered:
                    print(rendered, end='', file=sys.stderr, flush=True)
        status = process.wait()
    if status:
        raise SystemExit(status)
    if len(outputs) != 1:
        raise SystemExit(f'Expected one native taxonomy output, got {outputs}; build scripts: {sorted(build_scripts)}')
    output, = outputs
    # Build artifacts can include generated files and host-side dependencies.
    # Cargo's target directory and the requested target/profile are unambiguous.
    parser = argparse.ArgumentParser(add_help=False)
    for option in ['--target', '--target-dir', '--manifest-path', '--profile']:
        parser.add_argument(option)
    parser.add_argument('--release', action='store_true')
    args, _ = parser.parse_known_args(sys.argv[1:])
    metadata_command = [sys.argv[1], 'metadata', '--locked', '--no-deps', '--format-version=1']
    if args.manifest_path:
        metadata_command.extend(['--manifest-path', args.manifest_path])
    metadata = json.loads(subprocess.check_output(metadata_command, text=True))
    profile = Path(args.target_dir or metadata['target_directory'])
    target = args.target or os.environ.get('CARGO_BUILD_TARGET')
    if target:
        profile /= Path(target).stem if target.endswith('.json') else target
    name = args.profile or ('release' if args.release else 'dev')
    profile /= 'debug' if name == 'dev' else name
    profile.mkdir(parents=True, exist_ok=True)
    for source, name in [('unified-taxonomy-client-v2.sqlite3', 'taxonomy.sqlite3'), ('taxonomy.digest', 'taxonomy.digest')]:
        shutil.copyfile(output / source, profile / name)
    shutil.copytree(Path('apps/desktop-gpui/assets/fonts'), profile / 'fonts', dirs_exist_ok=True)
    print(f'Staged native taxonomy in {profile}', flush=True)


if __name__ == '__main__':
    main()
