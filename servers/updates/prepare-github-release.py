#!/usr/bin/env python3
"""Prepare an unsigned update from tested GitHub packages; never publish or sign."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import shutil
from urllib.parse import urlsplit
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('release_artifacts', ROOT / 'scripts/ci/release_artifacts.py')
release_artifacts = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release_artifacts)

# Add a platform only after its activation/recovery adapter is implemented.
PACKAGES = {
    'linux-x86_64-appimage': ('bokheim-desktop-appimage-x86-64', 'Bokheim-x86_64.AppImage'),
    'windows-x86_64-zip': ('bokheim-desktop-windows-x86-64', 'Bokheim-Windows-x86_64-Update.zip'),
    'android-aarch64-apk': ('bokheim-desktop-android-arm64', 'Bokheim-Android-arm64.apk'),
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def command(*args):
    return subprocess.check_output(args, text=True)


def gh_json(*args):
    return json.loads(command('gh', *args))


def fingerprint(path):
    digest = hashlib.sha256()
    size = 0
    with path.open('rb') as stream:
        while block := stream.read(1024 * 1024):
            size += len(block)
            digest.update(block)
    require(size > 0, f'Empty package: {path.name}')
    return {'bytes': size, 'sha256': digest.hexdigest()}


def validate_run(run, repository):
    require(run.get('repository', {}).get('full_name') == repository, 'Wrong workflow repository')
    require(run.get('head_repository', {}).get('full_name') == repository, 'Fork builds cannot authorize releases')
    require(run.get('path') == '.github/workflows/desktop-release.yml', 'Expected desktop release workflow')
    require(run.get('event') in ('push', 'workflow_dispatch'), 'Expected trusted release trigger')
    require(run.get('status') == 'completed' and run.get('conclusion') == 'success', 'Release workflow has not succeeded')
    sha = run.get('head_sha', '')
    require(re.fullmatch('[0-9a-f]{40}', sha), 'Invalid source SHA')
    return sha


def validate_info(info, target, version, trust, key_id):
    require(info.get('target') == target and info.get('application') == version, 'Package version/target mismatch')
    require(type(info.get('schema')) is int and info['schema'] > 0, 'Missing package schema version')
    formats = info.get('taxonomy_formats')
    require(isinstance(formats, list) and formats and all(type(n) is int and n > 0 for n in formats), 'Missing taxonomy compatibility')
    actual = info.get('update_trust') or {}
    keys = actual.get('keys') or {}
    require(actual.get('endpoint') == trust['endpoint'] and actual.get('channel') == trust['channel']
            and keys.get(key_id) == trust['keys'][key_id]
            and all(trust['keys'].get(k) == v for k, v in keys.items()),
            'Package does not contain the expected update endpoint and signing keys')


def hosted_artifact(config, tag, filename):
    endpoint = urlsplit(config['endpoint'])
    origin = f'{endpoint.scheme}://{endpoint.netloc}'
    namespace = config.get('origins', {}).get(origin)
    require(endpoint.scheme == 'https' and endpoint.hostname and not endpoint.username
            and not endpoint.password and not endpoint.query and not endpoint.fragment,
            'Expected an HTTPS publisher endpoint')
    require(isinstance(namespace, str) and re.fullmatch(r'[A-Za-z0-9_-]+', namespace),
            'Publisher endpoint requires a safe local origin mapping')
    require(re.fullmatch(r'v[0-9]+\.[0-9]+\.[0-9]+', tag)
            and re.fullmatch(r'[A-Za-z0-9_.-]+', filename) and filename not in ('.', '..'),
            'Invalid hosted package path')
    relative = Path(namespace) / 'releases' / tag / filename
    return f'{origin}/releases/{tag}/{filename}', relative


def verified_receipts(repository, source, sha, tag, directory):
    """Release upload permission is the local builder trust boundary."""
    require(gh_json('api', f'repos/{repository}/commits/{tag}')['sha'] == sha, 'Release tag differs from tested source')
    receipts = {}
    for platform in ('linux', 'windows', 'android'):
        name = release_artifacts.receipt_name(platform)
        command('gh', 'release', 'download', tag, '--repo', repository, '--pattern', name, '--dir', str(directory))
        receipt = json.loads((directory / name).read_text())
        release_artifacts.validate(receipt, platform, sha, source)
        if platform == 'windows':
            run = gh_json('api', f'repos/{source}/actions/runs/{receipt["workflow_run"]}')
            require(validate_run(run, source) == sha, 'Windows workflow differs from tested source')
        receipts[release_artifacts.TARGETS[platform]] = receipt
    return receipts


def prepare(args):
    for repo in (args.source_repository, args.release_repository):
        require(re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repo), 'Expected OWNER/REPO')
    require(re.fullmatch(r'v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', args.tag), 'Expected stable vMAJOR.MINOR.PATCH tag')
    require((args.run_id is None or args.run_id > 0) and args.sequence > 0 and 1 <= args.valid_days <= 90, 'Invalid run, sequence or validity (1–90 days)')
    require(not args.output.exists(), 'Output directory already exists; choose a new directory')
    config = json.loads(args.config.read_text())
    trust = {'endpoint': config['endpoint'], 'channel': config['channel'], 'keys': config['trusted_keys']}
    require(trust['channel'] == 'stable', 'This release command prepares the stable channel')
    require(trust['keys'] and all(re.fullmatch('[0-9a-f]{64}', k) for k in trust['keys'].values()), 'Expected lowercase hexadecimal public keys')
    require(args.key_id in trust['keys'], 'Selected signing key is not trusted by publisher configuration')
    android_cert = args.android_cert_sha256
    require(isinstance(android_cert, str) and re.fullmatch('[0-9a-f]{64}', android_cert), 'Expected Android production certificate SHA-256')
    if args.run_id is not None:
        run = gh_json('api', f'repos/{args.source_repository}/actions/runs/{args.run_id}')
        sha = validate_run(run, args.source_repository)
    else:
        sha = args.source_commit
        require(re.fullmatch('[0-9a-f]{40}', sha), 'Expected full source commit')
    command(sys.executable, str(ROOT / 'scripts/ci/require-shared-tests.py'), args.source_repository, sha)
    release = gh_json('release', 'view', args.tag, '--repo', args.release_repository,
                      '--json', 'tagName,isDraft,isPrerelease,assets')
    require(release.get('tagName') == args.tag and not release.get('isPrerelease'), 'Wrong release or prerelease')
    assets = {a['name'] for a in release['assets']}
    require('desktop-source-commit.txt' in assets, 'Release has no source commit receipt')
    previous = json.loads(args.previous_manifest.read_text()) if args.previous_manifest else None
    if previous:
        require(previous.get('protocol_version') == 1 and previous.get('channel') == trust['channel'], 'Previous payload protocol/channel mismatch')
        require(args.sequence > previous['sequence'], 'Sequence must exceed previous payload')
        old_targets = set((previous.get('application') or {}).get('artifacts', {}))
        require(old_targets <= set(PACKAGES), 'This tool cannot silently remove previously advertised platforms')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='bokheim-release-') as scratch:
        scratch = Path(scratch)
        downloaded = scratch / 'release'
        downloaded.mkdir()
        command('gh', 'release', 'download', args.tag, '--repo', args.release_repository,
                '--pattern', 'desktop-source-commit.txt', '--dir', str(downloaded))
        require((downloaded / 'desktop-source-commit.txt').read_text().strip() == sha, 'Release source differs from tested commit')
        build_receipts = verified_receipts(args.release_repository, args.source_repository, sha, args.tag, scratch) if args.run_id is None else None
        artifacts = {}
        first_info = None
        for target, (archive, filename) in PACKAGES.items():
            require(filename in assets, f'Missing release package: {filename}')
            built = scratch / target
            built.mkdir()
            if build_receipts is not None:
                for name in build_receipts[target]['files']:
                    command('gh', 'release', 'download', args.tag, '--repo', args.release_repository,
                            '--pattern', name, '--dir', str(built))
                    require(fingerprint(built / name) == build_receipts[target]['files'][name], 'Release artifact differs from verified local build')
            else:
                command('gh', 'run', 'download', str(args.run_id), '--repo', args.source_repository,
                        '--name', archive, '--dir', str(built))
            info = json.loads((built / f'update-info-{target}.json').read_text())
            validate_info(info, target, args.tag[1:], trust, args.key_id)
            if target == 'android-aarch64-apk':
                android = info.get('android') or {}
                major, minor, patch = map(int, args.tag[1:].split('.'))
                require(android.get('package') == 'se.bokheim.reader.gpui'
                        and android.get('version_code') == major * 1_000_000 + minor * 1_000 + patch
                        and android.get('signing_cert_sha256') == [android_cert]
                        and android.get('debuggable') is False,
                        'Android package identity, version, signing certificate or release mode mismatch')
            if first_info:
                require((info['schema'], info['taxonomy_formats']) == (first_info['schema'], first_info['taxonomy_formats']), 'Platform compatibility differs')
            first_info = info
            expected = fingerprint(built / filename)
            if target == 'android-aarch64-apk':
                require(info.get('apk_sha256') == expected['sha256'], 'Android metadata belongs to a different APK')
            command('gh', 'release', 'download', args.tag, '--repo', args.release_repository,
                    '--pattern', filename, '--dir', str(downloaded))
            require(fingerprint(downloaded / filename) == expected, 'Release package differs from successful build')
            url, _ = hosted_artifact(config, args.tag, filename)
            artifacts[target] = dict(url=url, **expected)
        # Downloads may take a while. Recheck the shared gate before producing
        # signable output in case a newer test attempt superseded its success.
        command(sys.executable, str(ROOT / 'scripts/ci/require-shared-tests.py'), args.source_repository, sha)
        now = int(time.time())
        manifest = {
            'protocol_version': 1, 'channel': trust['channel'], 'sequence': args.sequence,
            'issued_at_unix': now, 'expires_at_unix': now + args.valid_days * 86400,
            'application': {
                'version': args.tag[1:], 'name': f'Bokheim {args.tag[1:]}',
                'schema_version': first_info['schema'], 'taxonomy_format_versions': first_info['taxonomy_formats'],
                'artifacts': artifacts,
            },
            'taxonomy': previous.get('taxonomy') if previous else None,
        }
        receipt = {'source_repository': args.source_repository, 'source_commit': sha,
                   'workflow_run': args.run_id, 'release_repository': args.release_repository,
                   'tag': args.tag, 'signing_key_id': args.key_id,
                   'release_was_draft': release['isDraft'], 'android_cert_sha256': android_cert, 'artifacts': artifacts}
        if build_receipts is not None:
            receipt['build_receipts'] = build_receipts
        # Only expose a complete preparation result; a failed download leaves no
        # output that a later signing step might accidentally consume.
        with tempfile.TemporaryDirectory(prefix='.prepare-', dir=args.output.parent) as stage:
            result = Path(stage) / 'result'
            result.mkdir()
            for _, filename in PACKAGES.values():
                _, relative = hosted_artifact(config, args.tag, filename)
                destination = result / 'artifacts' / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(downloaded / filename, destination)
            (result / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
            (result / 'provenance.json').write_text(json.dumps(receipt, indent=2) + '\n')
            result.rename(args.output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-repository', required=True)
    parser.add_argument('--release-repository', required=True)
    origin = parser.add_mutually_exclusive_group(required=True)
    origin.add_argument('--run-id', type=int, help='Legacy combined Actions build')
    origin.add_argument('--source-commit', help='Exact source SHA of local builds and the Windows run')
    parser.add_argument('--tag', required=True)
    parser.add_argument('--sequence', required=True, type=int)
    parser.add_argument('--valid-days', type=int, default=14)
    parser.add_argument('--config', required=True, type=Path)
    parser.add_argument('--android-cert-sha256', required=True, help='Expected production APK signing certificate SHA-256, lowercase hex')
    parser.add_argument('--key-id', default='release-1', help='Key that will sign the prepared manifest')
    base = parser.add_mutually_exclusive_group(required=True)
    base.add_argument('--previous-manifest', type=Path, help='Reviewed previous unsigned payload; preserves the taxonomy offer')
    base.add_argument('--initial', action='store_true', help='Explicitly prepare the first application-only offer')
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    prepare(args)
    print(f'Prepared {args.output}/manifest.json and provenance.json. Review, sign, then publish after the release is public.')


if __name__ == '__main__':
    main()
