#!/usr/bin/env python3
"""Build and verify a release locally; optionally upload packages or deploy web."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import release_artifacts as artifacts

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
              shared_tests='Reuse exact-commit success; otherwise run locally and report',
              build=['bash', 'scripts/ci/local-platform.sh', args.platform, str(output)],
              upload=args.upload, tag=args.tag, deploy=args.deploy), indent=2))
        return
    sha = artifacts.command('git', 'rev-parse', 'HEAD')
    artifacts.clean_revision(sha)
    env = {**os.environ, 'RUSTUP_TOOLCHAIN': '1.95.0', 'RUSTC_BOOTSTRAP': '1'}
    env.setdefault('CARGO_BUILD_JOBS', '1')
    # Use the same public update trust as the Windows build; never load a private feed key.
    import re
    workflow = (ROOT / '.github/workflows/desktop-release.yml').read_text()
    for key in ['BOKHEIM_UPDATE_PUBLIC_KEY_HEX', 'BOKHEIM_UPDATE_KEY_ID', 'BOKHEIM_UPDATE_ENDPOINT']:
        env.setdefault(key, re.search(rf"^  {key}: .*?\|\| '([^']+)'", workflow, re.M).group(1))
    gate = [sys.executable, 'scripts/ci/require-shared-tests.py', args.repository, sha]
    if subprocess.run(gate, cwd=ROOT, env=env).returncode:
        subprocess.run([sys.executable, 'scripts/ci/shared-tests.py', '--report', args.repository], cwd=ROOT, env=env, check=True)
    subprocess.run(['bash', 'scripts/ci/local-platform.sh', args.platform, str(output)], cwd=ROOT, env=env, check=True)
    artifacts.clean_revision(sha)
    subprocess.run(gate, cwd=ROOT, env=env, check=True)
    artifacts.record(args.platform, output, sha, args.repository)
    if args.upload:
        artifacts.upload(args.platform, output, args.repository, args.tag)
    if args.deploy:
        subprocess.run(['bash', 'servers/sync/deploy/deploy-web-remote.sh', '--dist', str(output)], cwd=ROOT, env=env, check=True)
    print(f'Verified {args.platform} release: {output}')


if __name__ == '__main__':
    main()
