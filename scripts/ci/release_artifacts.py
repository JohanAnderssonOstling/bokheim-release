#!/usr/bin/env python3
"""Receipts and draft uploads for verified local and Windows release packages."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
TARGETS = {'linux': 'linux-x86_64-appimage', 'android': 'android-aarch64-apk',
           'windows': 'windows-x86_64-zip', 'web': 'web'}
FILES = {
    'linux': ['Bokheim-x86_64.AppImage', 'Bokheim-x86_64.AppImage.sha256', 'update-info-linux-x86_64-appimage.json'],
    'android': ['Bokheim-Android-arm64.apk', 'update-info-android-aarch64-apk.json'],
    'windows': ['Bokheim-Windows-x86_64-Setup.exe', 'Bokheim-Windows-x86_64-Setup.exe.sha256',
                'Bokheim-Windows-x86_64-Update.zip', 'update-info-windows-x86_64-zip.json'],
}


def require(ok, message):
    if not ok:
        raise ValueError(message)


def fingerprint(path):
    require(path.is_file() and not path.is_symlink(), f'Missing regular artifact: {path}')
    with path.open('rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    require(path.stat().st_size > 0, f'Empty artifact: {path}')
    return {'bytes': path.stat().st_size, 'sha256': digest}


def command(*args):
    return subprocess.check_output(args, text=True, cwd=ROOT).strip()


def clean_revision(sha):
    spec = importlib.util.spec_from_file_location('shared_suite', ROOT / 'scripts/ci/shared-tests.py')
    suite = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(suite)
    suite.clean_revision(sha)


def receipt_name(platform):
    return f'build-receipt-{TARGETS[platform]}.json'


def record(platform, directory, sha, repository, workflow_run=None):
    require(re.fullmatch(r'[0-9a-f]{40}', sha), 'Expected full source commit')
    require(re.fullmatch(r'[\w.-]+/[\w.-]+', repository), 'Expected OWNER/REPO')
    names = FILES.get(platform)
    if platform == 'web':
        names = sorted(str(p.relative_to(directory)) for p in directory.rglob('*') if p.is_file()
                       and p.name != receipt_name(platform))
        require('index.html' in names and any(n.endswith('.wasm') for n in names), 'Incomplete web bundle')
    receipt = dict(protocol_version=1, platform=TARGETS[platform], source_commit=sha,
                   source_repository=repository, builder='github-actions' if workflow_run else 'local',
                   files={name: fingerprint(directory / name) for name in names})
    if workflow_run:
        require(platform == 'windows' and workflow_run > 0, 'Only Windows uses Actions')
        receipt['workflow_run'] = workflow_run
    (directory / receipt_name(platform)).write_text(json.dumps(receipt, indent=2) + '\n')
    return receipt


def validate(receipt, platform, sha, repository, directory=None):
    require(receipt.get('protocol_version') == 1 and receipt.get('platform') == TARGETS[platform], 'Wrong receipt platform/protocol')
    require(receipt.get('source_commit') == sha and receipt.get('source_repository') == repository, 'Receipt source mismatch')
    require(receipt.get('builder') == ('github-actions' if platform == 'windows' else 'local'), 'Unexpected platform builder')
    if platform == 'windows':
        require(type(receipt.get('workflow_run')) is int and receipt['workflow_run'] > 0, 'Missing Windows workflow run')
    files = receipt.get('files', {})
    require(bool(files), 'Empty build receipt')
    if platform in FILES:
        require(set(files) == set(FILES[platform]), 'Incomplete package receipt')
    if platform == 'web':
        require('index.html' in files and any(n.endswith('.wasm') for n in files), 'Incomplete web receipt')
        if directory:
            actual = {str(p.relative_to(directory)) for p in directory.rglob('*') if p.is_file()
                      and p.name != receipt_name(platform)}
            require(actual == set(files), 'Web bundle files changed after verification')
    for name, expected in files.items():
        require(not Path(name).is_absolute() and '..' not in Path(name).parts and '\\' not in name, 'Unsafe receipt path')
        require(type(expected.get('bytes')) is int and expected['bytes'] > 0
                and re.fullmatch('[0-9a-f]{64}', expected.get('sha256', '')), 'Invalid artifact fingerprint')
        if directory:
            require(fingerprint(directory / name) == expected, f'Artifact changed after verification: {name}')


def upload(platform, directory, repository, tag):
    require(platform in FILES, 'Web deploys directly to the server')
    require(re.fullmatch(r'[\w.-]+/[\w.-]+', repository), 'Expected OWNER/REPO')
    require(re.fullmatch(r'v\d+\.\d+\.\d+', tag), 'Expected existing stable version tag')
    receipt = json.loads((directory / receipt_name(platform)).read_text())
    sha = receipt['source_commit']
    validate(receipt, platform, sha, repository, directory)
    command('python3', 'scripts/ci/require-shared-tests.py', repository, sha)
    # Never creates or moves tags, including when the release does not exist yet.
    command('gh', 'api', f'repos/{repository}/git/ref/tags/{tag}')
    require(json.loads(command('gh', 'api', f'repos/{repository}/commits/{tag}'))['sha'] == sha, 'Tag differs from built source')
    info = json.loads((directory / f'update-info-{TARGETS[platform]}.json').read_text())
    require(info.get('application') == tag[1:] and info.get('target') == TARGETS[platform], 'Tag differs from package version')
    releases = json.loads(command('gh', 'api', '--paginate', '--slurp', f'repos/{repository}/releases?per_page=100'))
    found = [r for page in releases for r in page if r['tag_name'] == tag]
    if not found:
        command('gh', 'release', 'create', tag, '--repo', repository, '--verify-tag', '--draft', '--title', f'Bokheim {tag[1:]}')
    release = json.loads(command('gh', 'release', 'view', tag, '--repo', repository, '--json', 'isDraft,assets'))
    require(release['isDraft'], 'Cannot overwrite a public release')
    with tempfile.TemporaryDirectory(prefix='bokheim-upload-') as scratch:
        source = Path(scratch) / 'desktop-source-commit.txt'
        if any(a['name'] == source.name for a in release['assets']):
            command('gh', 'release', 'download', tag, '--repo', repository, '--pattern', source.name, '--dir', scratch)
            require(source.read_text().strip() == sha, 'Draft already contains a different source commit')
        source.write_text(sha + '\n')
        command('gh', 'release', 'upload', tag, '--repo', repository, '--clobber',
                *[str(directory / name) for name in FILES[platform]], str(directory / receipt_name(platform)), str(source))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['record', 'upload'])
    parser.add_argument('platform', choices=list(TARGETS))
    parser.add_argument('--directory', type=Path, required=True)
    parser.add_argument('--repository', default='JohanAnderssonOstling/bokheim-release')
    parser.add_argument('--tag')
    args = parser.parse_args()
    if args.action == 'upload':
        require(args.tag, 'Upload needs an existing --tag')
        upload(args.platform, args.directory.resolve(), args.repository, args.tag)
    else:
        sha = command('git', 'rev-parse', 'HEAD')
        clean_revision(sha)
        record(args.platform, args.directory.resolve(), sha, args.repository,
               int(os.environ['GITHUB_RUN_ID']) if os.environ.get('GITHUB_ACTIONS') == 'true' else None)


if __name__ == '__main__':
    main()
