#!/usr/bin/env python3
"""Activate the verified LibraryThing release. Run --check without root first."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pwd
import signal
import sqlite3
import subprocess
import time
import urllib.request
import urllib.error

ROOT = Path('/home/johan/data/librarything-fallback')
BINARY = Path('/home/johan/bin/metadata-server-librarything')
EXPECTED_SHA = '9945ae61b60af9ac00eec369bb7e695e5cf6601fa2ee796e6ac5f660fc309c07'
UNIT = Path('/etc/systemd/system/bokheim-metadata.service')
TEMPLATE = Path('/usr/local/share/bokheim-deploy/bokheim-metadata.service')
INSTALLED = Path('/usr/local/bin/metadata-server')
KEY = Path('/home/johan/.config/bokheim/librarything-api-key')
ENV = KEY.with_name('metadata.env')
CACHE = Path('/var/lib/bokheim-metadata/librarything.sqlite')
LC = Path('/home/johan/data/loc-books-2016')
LC_DEPLOY = LC / 'deploy-lc-snapshot.py'
OLD_BINARY_LINE = "BINARY=P('/home/johan/bin/metadata-server-lc-all-records')"
NEW_BINARY_LINE = "BINARY=P('/usr/local/bin/metadata-server')"


def preflight():
    assert hashlib.sha256(BINARY.read_bytes()).hexdigest() == EXPECTED_SHA, 'Staged binary changed; review it before installing'
    assert KEY.is_file() and KEY.stat().st_mode & 0o077 == 0, 'Key must be private'
    assert UNIT.read_bytes() == TEMPLATE.read_bytes(), 'Installed unit differs from deployment template'
    text = LC_DEPLOY.read_text()
    assert OLD_BINARY_LINE in text or NEW_BINARY_LINE in text, 'Unexpected LC deployment script'
    state = json.loads((LC / 'activation-all-records-pending.json').read_text())
    assert state['status'] in ('waiting_for_import', 'deployed', 'failed'), 'LC activation is currently running'
    assert not Path('/var/lib/bokheim-deploy/metadata-catalog-incoming').exists(), 'Another deployment is staged'
    return state


def write_atomic(path, data, mode, uid, gid):
    pending = path.with_name(path.name + '.librarything-pending')
    fd = os.open(pending, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.chown(pending, uid, gid)
        os.chmod(pending, mode)
        os.replace(pending, path)
    finally:
        pending.unlink(missing_ok=True)


def request(endpoint, data=None):
    req = urllib.request.Request('http://127.0.0.1:8091/' + endpoint,
        data=json.dumps(data).encode() if data else None,
        headers={'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(req, timeout=40) as response:
            body = response.read()
    except urllib.error.HTTPError as exc:
        detail = exc.read(16384).decode('utf-8', 'replace')
        raise RuntimeError(f'{endpoint}: HTTP {exc.code}: {detail}') from None
    return json.loads(body) if data else body.decode().strip()


def ready():
    for _ in range(30):
        try:
            if request('health') == 'ok':
                return
        except OSError:
            pass
        time.sleep(1)
    raise RuntimeError('Metadata service did not become healthy')


def deploy():
    assert os.geteuid() == 0, 'Run this installation with sudo'
    with open('/run/lock/bokheim-metadata-catalog-deploy.lock', 'a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        state = preflight()
        worker = None
        saved = []
        modified = False
        backup = ROOT / ('activation-' + str(time.time_ns()))
        backup.mkdir(mode=0o700)
        account = pwd.getpwnam('johan')
        try:
            if state['status'] == 'waiting_for_import':
                worker_pid = json.loads((LC / 'activation-all-records-worker-process.json').read_text())['pid']
                proc = Path('/proc') / str(worker_pid)
                assert str(LC / 'activate-lc-when-ready.py').encode() in (proc / 'cmdline').read_bytes(), 'Worker PID changed'
                os.kill(worker_pid, signal.SIGSTOP)
                worker = worker_pid
                for _ in range(20):
                    if '\nState:\tT' in (proc / 'status').read_text():
                        break
                    time.sleep(0.05)
                else:
                    raise RuntimeError('Could not pause activation worker')
                assert not (proc / 'task' / str(worker) / 'children').read_text().strip(), 'Worker already launched deployment'
                assert json.loads((LC / 'activation-all-records-pending.json').read_text())['status'] == 'waiting_for_import'
            for index, path in enumerate([UNIT, TEMPLATE, INSTALLED, ENV, LC_DEPLOY]):
                if path.exists():
                    stat = path.stat()
                    data = path.read_bytes()
                    (backup / str(index)).write_bytes(data)
                    saved.append((path, data, stat.st_mode & 0o777, stat.st_uid, stat.st_gid))
                else:
                    saved.append((path, None, 0, 0, 0))
            (backup / 'manifest.json').write_text(json.dumps([str(s[0]) for s in saved]))
            CACHE.parent.mkdir(mode=0o700, exist_ok=True)
            os.chown(CACHE.parent, account.pw_uid, account.pw_gid)
            os.chmod(CACHE.parent, 0o700)
            # Preserve already-verified positive and negative lookups from the smoke check.
            if not CACHE.exists():
                source = sqlite3.connect((ROOT / 'smoke-cache.sqlite').as_uri() + '?mode=ro', uri=True)
                dest = sqlite3.connect(CACHE)
                source.backup(dest)
                dest.close()
                source.close()
                os.chown(CACHE, account.pw_uid, account.pw_gid)
                os.chmod(CACHE, 0o600)
            env = ENV.read_text() if ENV.exists() else ''
            env = '\n'.join(line for line in env.splitlines() if not line.startswith(('BOKHEIM_LIBRARYTHING_KEY_FILE=', 'BOKHEIM_LIBRARYTHING_CACHE=')))
            env += f'\nBOKHEIM_LIBRARYTHING_KEY_FILE={KEY}\nBOKHEIM_LIBRARYTHING_CACHE={CACHE}\n'
            unit = UNIT.read_text()
            settings = f'EnvironmentFile=-{ENV}\nStateDirectory=bokheim-metadata\nStateDirectoryMode=0700\n'
            if f'EnvironmentFile=-{ENV}' not in unit:
                unit = unit.replace('[Install]', settings + '\n[Install]')
            assert settings.strip() in unit, 'Unexpected existing LibraryThing unit configuration'
            modified = True
            write_atomic(ENV, env.encode(), 0o600, account.pw_uid, account.pw_gid)
            for path in [UNIT, TEMPLATE]:
                write_atomic(path, unit.encode(), 0o644, 0, 0)
            write_atomic(INSTALLED, BINARY.read_bytes(), 0o755, 0, 0)
            subprocess.run(['systemctl', 'daemon-reload'], check=True)
            subprocess.run(['systemctl', 'restart', 'bokheim-metadata.service'], check=True)
            ready()
            # Cached entries must not consume API quota on either endpoint.
            def count():
                with sqlite3.connect(CACHE.as_uri() + '?mode=ro', uri=True) as db:
                    return db.execute('SELECT count FROM librarything_requests').fetchone()[0]
            before = count()
            for endpoint in ['v2/classifications', 'v2/enrichment']:
                response = request(endpoint, {'isbns': ['9781538724736', '9798897248889']})
                codes = [c for r in response['results'] for m in r['matches'] for c in m['classifications']]
                assert any(c['scheme'] == 'library_of_congress' and c['notation'].startswith('PS3608') and any(e['method'] == 'librarything_isbn:22550208' for e in c.get('evidence', [])) for c in codes), response
            assert before == count(), 'Cached smoke lookup unexpectedly called LibraryThing'
            # Future LC snapshot activation uses the installed release, avoiding a downgrade.
            stat = LC_DEPLOY.stat()
            write_atomic(LC_DEPLOY, LC_DEPLOY.read_text().replace(OLD_BINARY_LINE, NEW_BINARY_LINE).encode(), stat.st_mode & 0o777, stat.st_uid, stat.st_gid)
            report = {'status': 'deployed', 'sha256': EXPECTED_SHA, 'cache': str(CACHE), 'backup': str(backup), 'completed_at': time.time(), 'cache_reused': True}
            (ROOT / 'deployment-result.json').write_text(json.dumps(report, indent=2))
            print(json.dumps(report))
        except BaseException as exc:
            try:
                diagnostic = {'status': 'failed', 'error': str(exc), 'backup': str(backup), 'failed_at': time.time()}
                # Capture before rollback so service configuration and logs describe the failed release.
                for name, command in [
                    ('journal', ['journalctl', '-u', 'bokheim-metadata.service', '-n', '60', '--no-pager']),
                    ('service', ['systemctl', 'show', 'bokheim-metadata.service', '-p', 'ActiveState', '-p', 'SubState', '-p', 'MemoryCurrent', '-p', 'StateDirectory', '-p', 'ReadWritePaths']),
                ]:
                    diagnostic[name] = subprocess.run(command, capture_output=True, text=True, timeout=15).stdout
                failure = ROOT / 'deployment-failure.json'
                # Only diagnostics from this provider; never include environment values or key contents.
                write_atomic(failure, json.dumps(diagnostic, indent=2).encode(), 0o600, account.pw_uid, account.pw_gid)
            except Exception as diagnostic_error:
                print(f"Could not save diagnostics: {diagnostic_error}", flush=True)
            if modified:
                for path, data, mode, uid, gid in reversed(saved):
                    if data is None:
                        path.unlink(missing_ok=True)
                    else:
                        write_atomic(path, data, mode, uid, gid)
                subprocess.run(['systemctl', 'daemon-reload'], check=True)
                subprocess.run(['systemctl', 'restart', 'bokheim-metadata.service'], check=True)
            raise
        finally:
            if worker is not None:
                os.kill(worker, signal.SIGCONT)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    if args.check:
        print(json.dumps({'status': 'ready', 'lc_activation': preflight()['status'], 'binary_sha256': EXPECTED_SHA}))
    else:
        deploy()
