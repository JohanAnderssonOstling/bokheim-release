#!/usr/bin/env python3
"""Read-only storage/SQL diagnostics. Run as root; writes only a new report directory.

Uses local PostgreSQL peer authentication, never reads application credentials.
Pass the companion audiobooks-availability.csv exported from the desktop library.
No repairs, migrations, checksumming of large audio files, or service restarts.
"""
import argparse
import collections
import csv
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import uuid


def command(args, text=None):
    result = subprocess.run(args, input=text, text=True, capture_output=True, timeout=90)
    if result.returncode:
        raise RuntimeError('{} failed: {}'.format(args[0], result.stderr.strip()))
    return result.stdout


def sql(query, database):
    # Explicit read-only transaction, bounded runtime, no psql startup files.
    output = command(['runuser', '-u', 'postgres', '--', 'psql', '-X', '-qAt',
                      '-v', 'ON_ERROR_STOP=1', '-d', database],
                     "BEGIN READ ONLY; SET LOCAL statement_timeout='30s';\n"
                     "SELECT COALESCE(json_agg(t),'[]'::json) FROM (" + query + ") t;\nCOMMIT;\n")
    return json.loads(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, default=Path(__file__).with_name('audiobooks-availability.csv'))
    parser.add_argument('--library', default='2f8f0531-71e7-4860-9b68-f2d1b45d5e5b')
    parser.add_argument('--database', default='bokheim')
    args = parser.parse_args()
    if os.geteuid() != 0:
        parser.error('run this script as root')
    library = str(uuid.UUID(args.library))
    with args.manifest.open(newline='') as source:
        manifest = list(csv.DictReader(source))
    hashes = sorted({row['content_hash'] for row in manifest})
    if not hashes or any(not re.fullmatch('[0-9a-f]{64}', h) for h in hashes):
        parser.error('manifest must contain valid 64-character content_hash values')
    os.umask(0o077)
    report = Path(tempfile.mkdtemp(prefix='bokheim-audiobook-diagnostic-', dir='/var/tmp'))
    print('Read-only diagnostic report: {}'.format(report), flush=True)
    try:
        status = command(['systemctl', 'show', 'bokheim-sync.service', '-p', 'ActiveState',
                          '-p', 'MainPID', '-p', 'User', '-p', 'ExecMainStartTimestamp', '-p', 'NRestarts'])
        (report / 'service.txt').write_text(status)
        props = dict(line.split('=', 1) for line in status.splitlines() if '=' in line)
        # Whitelist only storage configuration; never report full environment.
        environment = command(['systemctl', 'show', 'bokheim-sync.service', '-p', 'Environment', '--value'])
        variables = dict(item.split('=', 1) for item in shlex.split(environment) if '=' in item)
        root = Path(variables.get('BOKHEIM_ASSETS_DIR', '/var/lib/bokheim/assets'))
        pid = int(props.get('MainPID', '0'))
        if pid:
            for item in Path('/proc/{}/environ'.format(pid)).read_bytes().split(b'\0'):
                if item.startswith(b'BOKHEIM_ASSETS_DIR='):
                    root = Path(os.fsdecode(item.split(b'=', 1)[1]))
        if not root.is_absolute():
            raise RuntimeError('asset directory is not absolute')
        (report / 'mount.txt').write_text(command(['findmnt', '-T', str(root), '-o', 'TARGET,SOURCE,FSTYPE,OPTIONS']))
        schema_version = sql('SELECT version FROM bokheim_schema_version', args.database)
        (report / 'schema-version.json').write_text(json.dumps(schema_version, indent=2))
        owners = sql("SELECT id,name,user_id,created_at,deleted_at FROM libraries WHERE id='{}'".format(library), args.database)
        (report / 'library.json').write_text(json.dumps(owners, indent=2))
        if len(owners) != 1:
            raise RuntimeError('library UUID is absent from PostgreSQL; see library.json')
        counts = sql("SELECT kind,count(*) AS rows,count(*) FILTER(WHERE present IS TRUE) AS present_rows FROM sync_state WHERE library_id='{}' GROUP BY kind ORDER BY kind".format(library), args.database)
        (report / 'sync-counts.json').write_text(json.dumps(counts, indent=2))
        values = ','.join("('{}')".format(h) for h in hashes)
        rows = sql("""WITH requested(identity) AS (VALUES %s), owner AS
          (SELECT user_id FROM libraries WHERE id='%s')
          SELECT requested.identity,COALESCE(r.blob_hash,requested.identity) AS checksum,
            r.blob_hash IS NOT NULL AS revision_exists,b.size_bytes AS recorded_bytes,
            EXISTS(SELECT 1 FROM user_blob_charge c WHERE c.user_id=o.user_id AND c.content_hash=requested.identity) AS identity_charged,
            EXISTS(SELECT 1 FROM user_blob_reference f WHERE f.user_id=o.user_id AND f.content_hash=requested.identity AND f.reference_count>0) AS identity_referenced,
            EXISTS(SELECT 1 FROM user_blob_charge c WHERE c.user_id=o.user_id AND c.content_hash=COALESCE(r.blob_hash,requested.identity)) AS resolved_charged,
            EXISTS(SELECT 1 FROM user_blob_reference f WHERE f.user_id=o.user_id AND f.content_hash=COALESCE(r.blob_hash,requested.identity) AND f.reference_count>0) AS resolved_referenced,
            EXISTS(SELECT 1 FROM sync_state s WHERE s.library_id='%s' AND s.content_hash=requested.identity AND s.present IS TRUE) AS present_sync_reference
          FROM requested CROSS JOIN owner o
          LEFT JOIN user_book_revision r ON r.user_id=o.user_id AND r.content_hash=requested.identity
          LEFT JOIN blob_object b ON b.content_hash=COALESCE(r.blob_hash,requested.identity)
          ORDER BY requested.identity""" % (values, library, library), args.database)
        if any(not re.fullmatch('[0-9a-f]{64}', row['checksum']) for row in rows):
            raise RuntimeError('invalid stored checksum; refusing to construct file paths')
        paths = [str(root / 'books' / row['checksum'][:2] / row['checksum']) for row in rows]
        probe = """import json,os,stat,sys
out=[]
for path in json.load(sys.stdin):
 try:
  info=os.stat(path)
  if not stat.S_ISREG(info.st_mode):
   out.append({'state':'not_regular','bytes':None});continue
  with open(path,'rb') as f: f.read(1)
  out.append({'state':'readable','bytes':info.st_size})
 except FileNotFoundError: out.append({'state':'missing','bytes':None})
 except PermissionError: out.append({'state':'permission_denied','bytes':None})
 except OSError as e: out.append({'state':'os_error_'+str(e.errno),'bytes':None})
print(json.dumps(out))
"""
        probes = json.loads(command(['runuser', '-u', props.get('User') or 'bokheim', '--', 'python3', '-c', probe], json.dumps(paths)))
        index = {row['content_hash']: row for row in manifest}
        for row, path, found in zip(rows, paths, probes):
            row.update(title=index[row['identity']]['title'], format=index[row['identity']]['format'],
                       previous_http_status=index[row['identity']].get('server_http_status', ''),
                       path=path, file_state=found['state'], actual_bytes=found['bytes'])
            issues = []
            if not (row['identity_charged'] or row['identity_referenced'] or row['revision_exists']):
                issues.append('no_identity_entitlement_404')
            if found['state'] != 'readable':
                issues.append('file_' + found['state'])
            if not (row['resolved_charged'] or row['resolved_referenced']):
                issues.append('no_resolved_entitlement_404')
            if row['recorded_bytes'] is None:
                issues.append('no_blob_object_record')
            elif found['state'] == 'readable' and row['recorded_bytes'] != found['bytes']:
                issues.append('size_mismatch')
            row['findings'] = ';'.join(issues) or 'no_discrepancy_detected'
        (report / 'books.json').write_text(json.dumps(rows, indent=2))
        with (report / 'books.csv').open('w', newline='') as target:
            writer = csv.DictWriter(target, fieldnames=list(rows[0]))
            writer.writeheader()
            writer.writerows(rows)
        # Inventory only paths and sizes; never read or hash whole books.
        inventory_count = 0
        inventory_bytes = 0
        walk_errors = []
        with (report / 'storage-files.csv').open('w', newline='') as target:
            writer = csv.writer(target)
            writer.writerow(['relative_path', 'bytes'])
            for directory, _, files in os.walk(root / 'books', followlinks=False, onerror=lambda e: walk_errors.append(str(e))):
                for name in files:
                    path = Path(directory) / name
                    try:
                        if path.is_symlink():
                            continue
                        size = path.stat().st_size
                        writer.writerow([str(path.relative_to(root)), size])
                        inventory_count += 1
                        inventory_bytes += size
                    except OSError as error:
                        walk_errors.append(str(error))
        summary = dict(library_id=library, library_name=owners[0]['name'], library_deleted=owners[0]['deleted_at'],
                       assets_directory=str(root), service=status.splitlines(),
                       manifest_entries=len(rows), inventory_files=inventory_count, inventory_bytes=inventory_bytes,
                       inventory_errors=walk_errors, findings=dict(collections.Counter(row['findings'] for row in rows)),
                       limitations=['Library titles come from the supplied desktop manifest.',
                                    'Probes run as the service user outside its systemd sandbox.',
                                    'No full-file checksum verification; database and disk can change during collection.',
                                    'No repairs or production database writes were performed.'])
        (report / 'summary.json').write_text(json.dumps(summary, indent=2))
        print(json.dumps(summary, indent=2))
        print('Complete report: {}'.format(report))
    except (OSError, RuntimeError, subprocess.TimeoutExpired, ValueError) as error:
        (report / 'error.txt').write_text(str(error) + '\n')
        raise SystemExit('Diagnostic stopped: {}\nPartial report: {}'.format(error, report))


if __name__ == '__main__':
    main()
