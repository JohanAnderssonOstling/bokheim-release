#!/usr/bin/env python3
"""Run shared release tests off hosted runners; optionally attest the exact commit."""
import argparse
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
CONTEXT = 'bokheim/shared-tests-v1'


def commands():
    packages = ['account-client', 'book-access', 'book-enrichment', 'book-model',
                'library-files', 'asset-transfer', 'client-platform-runtime',
                'client-runtime', 'client-platform-android', 'client-platform-kobo', 'sync-engine', 'library-database',
                'library-replica', 'library-backend', 'subject-projection']
    return [
        ['python3', '-m', 'unittest', 'discover', '-s', 'scripts/ci/tests'],
        ['python3', 'shared/pdfium/prepare.py', '--target', 'x86_64-unknown-linux-gnu'],
        ['cargo', 'test', '--release', '--no-fail-fast', *sum((['-p', p] for p in packages), [])],
        ['cargo', 'test', '--release', '--no-fail-fast', '-p', 'update-client', '--features', 'native-state'],
        ['cargo', 'test', '--release', '--no-fail-fast', '-p', 'update-publisher'],
        ['cargo', 'test', '--release', '--no-fail-fast', '--manifest-path', 'apps/kobo-installer/Cargo.toml', '--lib'],
        ['bash', 'client/app/tests/run_sync_e2e.sh'],
    ]


def git(*args, directory=ROOT):
    return subprocess.check_output(['git', '-C', str(directory), *args], text=True).strip()


def clean_revision(expected):
    if git('rev-parse', 'HEAD') != expected or git('status', '--porcelain', '--untracked-files=normal'):
        raise RuntimeError('Reporting requires an unchanged, clean checkout of the tested commit')
    workflow = (ROOT / '.github/workflows/desktop-release.yml').read_text()
    for directory, variable in [('GPUI-Fork', 'GPUI_FORK_REF'), ('HtmlEngine', 'HTML_ENGINE_REF'), ('HtmlViewCore', 'HTML_VIEW_CORE_REF')]:
        pin = re.search(rf'^  {variable}: ([0-9a-f]{{40}})$', workflow, re.M).group(1)
        sibling = ROOT.parent / directory
        if git('rev-parse', 'HEAD', directory=sibling) != pin or git('status', '--porcelain', directory=sibling):
            raise RuntimeError(f'{directory} must be clean and at the release workflow pin {pin}')


def report(repository, sha, state):
    payload = {'state': state, 'context': CONTEXT, 'description': f'Shared release suite: {state}'}
    subprocess.run(['gh', 'api', '--method', 'POST', f'repos/{repository}/statuses/{sha}', '--input', '-'],
                   input=json.dumps(payload), text=True, check=True, stdout=subprocess.DEVNULL)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report', metavar='OWNER/REPO', help='Post commit status using gh authentication')
    parser.add_argument('--dry-run', action='store_true')
    args = parser.parse_args()
    if args.dry_run:
        print(json.dumps(commands(), indent=2))
        return
    if args.report and not re.fullmatch(r'[\w.-]+/[\w.-]+', args.report):
        parser.error('invalid repository')
    sha = git('rev-parse', 'HEAD')
    if args.report:
        clean_revision(sha)
        report(args.report, sha, 'pending')
    try:
        failures = []
        for command in commands():
            print('+ ' + ' '.join(command), flush=True)
            result = subprocess.run(command, cwd=ROOT)
            if result.returncode:
                if 'shared/pdfium/prepare.py' in command:
                    raise subprocess.CalledProcessError(result.returncode, command)
                failures.append(' '.join(command))
        if failures:
            raise RuntimeError('Shared suite failures:\n' + '\n'.join(failures))
        if args.report:
            clean_revision(sha)
            report(args.report, sha, 'success')
    except BaseException:
        if args.report:
            report(args.report, sha, 'failure')
        raise


if __name__ == '__main__':
    main()
