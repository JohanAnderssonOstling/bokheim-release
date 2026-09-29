#!/usr/bin/env python3
"""Reject release publication when the deployed server rejects the built protocol."""
import argparse
import re
import subprocess
import urllib.error
import urllib.request


def check(source_commit, server='https://api.bokheim.se'):
    if not re.fullmatch('[0-9a-f]{40}', source_commit):
        raise ValueError('Expected full build source SHA')
    source = subprocess.check_output(['git', 'show', f'{source_commit}:shared/protobuf-wire/src/lib.rs'], text=True)
    match = re.search(r'pub const MEDIA_TYPE: &str = "([^"]+)";', source)
    if not match:
        raise ValueError('Cannot determine the built protocol media type')
    media_type = match.group(1)
    # An envelope with a version but no value cannot authenticate or mutate state.
    # An empty byte string would decode as version zero, incorrectly returning 426.
    version = int(re.search(r'version=(\d+)', media_type).group(1))
    payload = bytearray([8])  # Envelope.version, protobuf field 1 (varint).
    while version >= 128:
        payload.append((version & 127) | 128)
        version >>= 7
    payload.append(version)
    request = urllib.request.Request(server.rstrip('/') + '/api/auth/login', data=bytes(payload),
                                     headers={'Content-Type': media_type, 'Accept': media_type})
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            status = response.status
    except urllib.error.HTTPError as error:
        status = error.code
        error.close()
    if status != 400:
        raise RuntimeError(f'Deployed server does not accept {media_type}: HTTP {status}. Deploy the matching server before publishing the client.')
    print(f'Deployed server accepts {media_type}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source_commit')
    parser.add_argument('--server', default='https://api.bokheim.se')
    args = parser.parse_args()
    check(args.source_commit, args.server)
