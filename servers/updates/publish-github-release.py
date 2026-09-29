#!/usr/bin/env python3
"""Review, sign and publish a prepared GitHub release from a trusted release machine."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('prepare_release', Path(__file__).with_name('prepare-github-release.py'))
prepare_release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare_release)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def read_regular(path):
    require(path.is_file() and not path.is_symlink(), f'Expected a regular file: {path}')
    with path.open('rb') as stream:
        data = stream.read(1024 * 1024 + 1)
    require(len(data) <= 1024 * 1024, f'Metadata exceeds size limit: {path.name}')
    return data


def reviewed_inputs(bundle):
    # Retain these exact bytes: later file changes must not change what is signed.
    payload = read_regular(bundle / 'manifest.json')
    provenance = read_regular(bundle / 'provenance.json')
    digest = hashlib.sha256(b'bokheim-prepared-release-v1\0' + len(payload).to_bytes(8, 'big') + payload + provenance).hexdigest()
    return payload, provenance, digest


def command(*args):
    return subprocess.check_output(args, text=True, cwd=ROOT)


def validate_release(manifest, receipt, config):
    source = receipt['source_repository']
    repository = receipt['release_repository']
    sha = receipt['source_commit']
    tag = receipt['tag']
    run_id = receipt['workflow_run']
    for name in (source, repository):
        require(re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', name), 'Invalid repository in provenance')
    require(re.fullmatch(r'[0-9a-f]{40}', sha), 'Invalid source commit in provenance')
    require(type(run_id) is int and run_id > 0, 'Invalid workflow run in provenance')
    require(re.fullmatch(r'v\d+\.\d+\.\d+', tag), 'Expected a stable release tag')
    application = manifest.get('application') or {}
    require(application.get('version') == tag[1:], 'Manifest version differs from provenance')
    require(application.get('artifacts') and application['artifacts'] == receipt.get('artifacts'), 'Manifest packages differ from provenance')
    run = json.loads(command('gh', 'api', f'repos/{source}/actions/runs/{run_id}'))
    require(prepare_release.validate_run(run, source) == sha, 'Workflow source differs from reviewed provenance')
    command(sys.executable, str(ROOT / 'scripts/ci/require-shared-tests.py'), source, sha)
    release = json.loads(command('gh', 'release', 'view', tag, '--repo', repository, '--json', 'tagName,isDraft,isPrerelease,assets'))
    require(release.get('tagName') == tag and release.get('isDraft') is False and release.get('isPrerelease') is False,
            'Release must be published and stable before signing/publication')
    names = {asset['name'] for asset in release['assets']}
    require('desktop-source-commit.txt' in names, 'Missing release source receipt')
    with tempfile.TemporaryDirectory(prefix='bokheim-public-source-') as scratch:
        command('gh', 'release', 'download', tag, '--repo', repository, '--pattern', 'desktop-source-commit.txt', '--dir', scratch)
        require(read_regular(Path(scratch) / 'desktop-source-commit.txt').decode().strip() == sha, 'Release source differs from reviewed provenance')
    for target, artifact in application['artifacts'].items():
        require(target in prepare_release.PACKAGES, f'Unsupported prepared platform: {target}')
        filename = prepare_release.PACKAGES[target][1]
        expected_url, _ = prepare_release.hosted_artifact(config, tag, filename)
        require(filename in names and artifact['url'] == expected_url, 'Package identity differs from reviewed provenance')


def fetch_feed(endpoint):
    require(endpoint.startswith('https://'), 'Public feed verification requires HTTPS')
    request = urllib.request.Request(endpoint, headers={'Cache-Control': 'no-cache'})
    with urllib.request.urlopen(request, timeout=60) as response:
        require(response.geturl().startswith('https://'), 'Feed verification redirected away from HTTPS')
        return response.read(1024 * 1024 + 1)


def publish(args):
    payload, provenance, digest = reviewed_inputs(args.bundle)
    require(digest == args.review_sha256, 'Review digest differs: review manifest.json and provenance.json again')
    manifest, receipt = json.loads(payload), json.loads(provenance)
    config = json.loads(read_regular(args.config))
    key_id = receipt['signing_key_id']
    require(key_id in config['trusted_keys'], 'Reviewed signing key is not trusted by publisher configuration')
    require(manifest['channel'] == config['channel'], 'Publisher channel differs from reviewed manifest')
    require(not args.output.exists() and not args.output.is_symlink(), 'Signed output already exists; use a fresh output directory')
    require(args.key.is_file(), 'Signing key does not exist on this release machine')
    validate_release(manifest, receipt, config)
    # Copy only reviewed application packages; taxonomy was already published.
    # GitHub credentials stay on the release machine, not the serving host.
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.bokheim-sign-', dir=args.output.parent) as scratch:
        stage = Path(scratch).resolve() / 'bundle'
        stage.mkdir()
        for target, artifact in manifest['application']['artifacts'].items():
            filename = prepare_release.PACKAGES[target][1]
            _, relative = prepare_release.hosted_artifact(config, receipt['tag'], filename)
            source = args.bundle / 'artifacts' / relative
            require(source.is_file() and not source.is_symlink()
                    and source.resolve().is_relative_to(args.bundle.resolve()), 'Missing or unsafe prepared package')
            destination = stage / 'artifacts' / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, destination)
            require(prepare_release.fingerprint(destination) == {k: artifact[k] for k in ('bytes', 'sha256')},
                    'Prepared package differs from reviewed hash or size')
        (stage / 'manifest.json').write_bytes(payload)
        (stage / 'provenance.json').write_bytes(provenance)
        command('cargo', 'run', '--release', '-p', 'update-client', '--example', 'sign_manifest', '--',
                str(stage / 'manifest.json'), str(args.key.resolve()), key_id, str(stage / 'manifest.signed.json'))
        envelope = json.loads(read_regular(stage / 'manifest.signed.json'))
        require(envelope['payload'].encode() == payload and envelope['key_id'] == key_id, 'Signer output differs from reviewed payload')
        # Recheck after building/signing; a superseding test result must block promotion.
        command(sys.executable, str(ROOT / 'scripts/ci/require-shared-tests.py'), receipt['source_repository'], receipt['source_commit'])
        stage.rename(args.output)
    # Keep the signed output on failure. Publishing is idempotent for these exact
    # bytes; never issue an automatic rollback after an uncertain network result.
    if args.staging_root:
        command('cargo', 'run', '--release', '-p', 'update-publisher', '--',
                str(args.config.resolve()), str(args.output.resolve()), str(args.staging_root.resolve()))
        print(f'Published to local staging root: {args.staging_root}')
    else:
        command('bash', str(ROOT / 'servers/updates/deploy-remote.sh'), args.host, str(args.output.resolve()))
        expected = read_regular(args.output / 'manifest.signed.json')
        require(fetch_feed(config['endpoint']) == expected,
                'Publication ran, but the public feed does not match; retain the signed bundle and investigate before retrying')
        print(f'Published and verified {config["endpoint"]}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bundle', required=True, type=Path, help='Output of prepare-github-release.py')
    parser.add_argument('--print-review-digest', action='store_true', help='Print the digest after reviewing both JSON files; performs no signing or publication')
    parser.add_argument('--review-sha256', help='Digest of the manifest and provenance that were reviewed')
    parser.add_argument('--config', type=Path)
    parser.add_argument('--key', type=Path, help='Local Ed25519 PKCS#8 file; never uploaded')
    parser.add_argument('--output', type=Path, help='Fresh directory for the signed bundle; retained if publication fails')
    destination = parser.add_mutually_exclusive_group()
    destination.add_argument('--host', help='Provisioned update publishing SSH destination')
    destination.add_argument('--staging-root', type=Path, help='Local publisher root for staging verification')
    args = parser.parse_args()
    if args.print_review_digest:
        print(reviewed_inputs(args.bundle)[2])
        return
    if not all((args.review_sha256, args.config, args.key, args.output, args.host or args.staging_root)):
        parser.error('Publication requires --review-sha256, --config, --key, --output and either --host or --staging-root')
    publish(args)


if __name__ == '__main__':
    main()
