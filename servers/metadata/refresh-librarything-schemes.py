#!/usr/bin/env python3
"""Run the authorized one-time classification refresh when the shared quota allows."""
import argparse
import fcntl
import json
from pathlib import Path
import sqlite3
import subprocess
import time
from contextlib import closing


def run(args):
    root = Path(args.root)
    with (root / 'schemes-refresh.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        while True:
            with closing(sqlite3.connect(Path(args.cache).resolve().as_uri() + '?mode=ro', uri=True)) as db:
                row = db.execute('SELECT day,count FROM librarything_requests WHERE id=1').fetchone()
            now = time.time()
            if not row or row[0] != int(now // 86400) or row[1] < 1000:
                break
            reset = (row[0] + 1) * 86400 + 5
            print(json.dumps({'status': 'waiting_for_quota', 'resume_at_utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(reset))}), flush=True)
            time.sleep(min(60, max(1, reset - now)))
        command = ['python3', '-u', str(root / 'audit-librarything-ranked.py'),
                   '--ranked', str(root / 'missing-classifications-refresh-ranked.json'),
                   '--database', args.database, '--binary', args.binary,
                   '--key-file', args.key_file, '--cache', args.cache,
                   '--output', str(root / 'librarything-schemes-refresh-results.json'),
                   '--refresh-schemes', '--max-consecutive-failures', '5']
        print(json.dumps({'status': 'starting_refresh'}), flush=True)
        subprocess.run(command, check=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    for name in ['root', 'database', 'binary', 'key-file', 'cache']:
        p.add_argument('--' + name, required=True)
    run(p.parse_args())
