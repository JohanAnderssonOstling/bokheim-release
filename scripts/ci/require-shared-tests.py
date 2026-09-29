#!/usr/bin/env python3
"""Fail closed unless the newest shared-suite status for this commit passed."""
import argparse
import json
import re
import subprocess

CONTEXT = 'bokheim/shared-tests-v1'


def require_success(statuses):
    matches = [s for s in statuses if s.get('context') == CONTEXT]
    if not matches or max(matches, key=lambda s: s['id']).get('state') != 'success':
        raise RuntimeError('Shared tests have not passed for this exact commit. Run scripts/ci/shared-tests.py --report OWNER/REPO on the trusted runner, then retry.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('repository')
    parser.add_argument('sha')
    args = parser.parse_args()
    if not re.fullmatch(r'[\w.-]+/[\w.-]+', args.repository) or not re.fullmatch(r'[0-9a-f]{40}', args.sha):
        parser.error('expected OWNER/REPO and full commit SHA')
    pages = json.loads(subprocess.check_output([
        'gh', 'api', '--paginate', '--slurp', f'repos/{args.repository}/commits/{args.sha}/statuses?per_page=100'
    ], text=True))
    require_success([status for page in pages for status in page])
    print(f'Shared tests passed for {args.sha}')


if __name__ == '__main__':
    main()
