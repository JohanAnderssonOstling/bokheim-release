#!/usr/bin/env python3
"""Build and verify a release locally; optionally upload packages or deploy web."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import release_artifacts as artifacts
import release_config

ROOT = artifacts.ROOT


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('platform', choices=['linux', 'android', 'web'])
    parser.add_argument('--output', required=True, type=Path, help='Fresh directory outside the source checkout')
    parser.add_argument('--repository', default='JohanAnderssonOstling/bokheim-release')
    parser.add_argument('--upload', action='store_true', help='Upload verified native artifacts to an existing tag’s draft')
    parser.add_argument('--tag', help='Existing immutable version tag, required for --upload')
    parser.add_argument('--deploy', action='store_true', help='Deploy the verified web bundle directly via SSH')
    parser.add_argument('--dry-run', action='store_true')
    args = parser.parse_args()
    if args.upload and (args.platform == 'web' or not args.tag):
        parser.error('--upload requires a native platform and an existing --tag')
    if args.deploy and args.platform != 'web':
        parser.error('--deploy is only for web')
    output = args.output.resolve()
    if output.exists() or output.is_relative_to(ROOT):
        parser.error('--output must be a fresh directory outside the source checkout')
    if args.dry_run:
        print(json.dumps(dict(platform=args.platform, output=str(output),
              build=['bash', 'scripts/ci/local-platform.sh', args.platform, str(output)],
              upload=args.upload, tag=args.tag, deploy=args.deploy), indent=2))
        return
    sha = artifacts.command('git', 'rev-parse', 'HEAD')
    artifacts.clean_revision(sha)
    release_environment = release_config.environment(os.environ)
    env = {**os.environ, **release_environment, 'RUSTUP_TOOLCHAIN': release_environment['RUST_VERSION'], 'RUSTC_BOOTSTRAP': '1'}
    subprocess.run(['bash', 'scripts/ci/local-platform.sh', args.platform, str(output)], cwd=ROOT, env=env, check=True)
    artifacts.clean_revision(sha)
    artifacts.record(args.platform, output, sha, args.repository)
    if args.upload:
        artifacts.upload(args.platform, output, args.repository, args.tag)
    if args.deploy:
        subprocess.run(['bash', 'servers/sync/deploy/deploy-web-remote.sh', '--dist', str(output)], cwd=ROOT, env=env, check=True)
    print(f'Verified {args.platform} release: {output}')


if __name__ == '__main__':
    main()
