#!/usr/bin/env python3
"""Repackage retained native binaries without recompiling the application."""
import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


def command(*args):
    return subprocess.check_output(args, text=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('run', type=int)
    parser.add_argument('platform', choices=['windows', 'arm64', 'x86_64'])
    args = parser.parse_args()
    repository = 'JohanAnderssonOstling/bokheim-release'
    windows = args.platform == 'windows'
    workflow = 'desktop-release.yml' if windows else 'macos-release.yml'
    build_step = 'Build GPUI desktop release' if windows else 'Build optimized desktop app'
    job_name = 'x86-64 Windows installer and portable app' if windows else f'Build {args.platform} macOS'
    run = json.loads(command('gh', 'api', f'repos/{repository}/actions/runs/{args.run}'))
    if run['status'] != 'completed' or run['path'] != '.github/workflows/' + workflow:
        raise ValueError('Expected a completed native packaging workflow')
    jobs = json.loads(command('gh', 'run', 'view', str(args.run), '--repo', repository, '--json', 'jobs'))['jobs']
    job, = [job for job in jobs if job['name'] == job_name]
    if not any(step['name'] == build_step and step['conclusion'] == 'success' for step in job['steps']):
        raise ValueError('The retained native build did not succeed')
    logs = command('gh', 'run', 'view', str(args.run), '--repo', repository, '--log', '--job', str(job['databaseId']))
    checkout = re.search(r'log -1 --format=%H\r?\n[^\n]*? ([0-9a-f]{40})\r?\n', logs)
    source = command('git', 'rev-parse', 'HEAD').strip()
    if checkout is None or checkout[1] != source:
        raise ValueError('Retained binaries were built from a different application source')
    target = None if windows else {'arm64': 'aarch64-apple-darwin', 'x86_64': 'x86_64-apple-darwin'}[args.platform]
    profile = Path('target') / 'release' if windows else Path('target') / target / 'release'
    profile.mkdir(parents=True, exist_ok=True)
    artifact = 'bokheim-windows-compiled-inputs' if windows else f'bokheim-macos-{args.platform}-compiled-inputs'
    with tempfile.TemporaryDirectory() as scratch:
        scratch = Path(scratch)
        command('gh', 'run', 'download', str(args.run), '--repo', repository, '--name', artifact, '--dir', str(scratch))
        generated = scratch / 'taxonomy.sqlite3'
        for name in ['bokheim', 'bokheim-browser']:
            filename = name + ('.exe' if windows else '')
            shutil.copy2(scratch / filename, profile / filename)
            (profile / filename).chmod(0o755)
        if not generated.is_file():
            candidates = list(scratch.rglob('unified-taxonomy-client-v2.sqlite3'))
            if candidates:
                generated, = candidates
        if generated.is_file():
            shutil.copyfile(generated, profile / 'taxonomy.sqlite3')
            shutil.copyfile(generated.with_name('taxonomy.digest'), profile / 'taxonomy.digest')
        else:
            # Early retention jobs kept only the executables. Regenerate their
            # platform-independent resource from the verified application source.
            subprocess.run([sys.executable, str(Path(__file__).with_name('build-native.py')),
                            'cargo', 'build', '--locked', '--release', '-p', 'subject-projection',
                            *(['--target', target] if target else [])], check=True)
    shutil.copytree(Path('apps/desktop-gpui/assets/fonts'), profile / 'fonts', dirs_exist_ok=True)
    print(f'Recovered {args.platform} binaries and generated taxonomy from run {args.run}, source {source}')


if __name__ == '__main__':
    main()
