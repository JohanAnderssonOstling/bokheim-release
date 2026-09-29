#!/usr/bin/env python3
"""Resolve ranked works via the release CLI and the live provider cache."""
import argparse
import csv
from contextlib import closing
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import time


def save(path, report):
    tmp = path.with_suffix('.tmp')
    tmp.write_text(json.dumps(report, ensure_ascii=False, indent=2))
    tmp.replace(path)


def local_lookup(args, env, isbns):
    # Query deadlines can be hit while caches warm or a large ISBN batch is processed.
    # Retry locally, then split the batch; never turn local failures into API eligibility.
    def query(values):
        last_error = ''
        for attempt in range(3):
            try:
                process = subprocess.run([args.binary, 'lookup-lcc', args.database, *values], env=env, capture_output=True, text=True, timeout=120)
                if process.returncode == 0:
                    return json.loads(process.stdout)
                last_error = process.stderr[-1200:]
                if not any(word in last_error.lower() for word in ['interrupted', 'locked', 'timed out']):
                    break
            except subprocess.TimeoutExpired:
                last_error = 'Local classification command timed out'
            if attempt < 2:
                time.sleep(2)
        raise RuntimeError('Local classification precheck failed: ' + last_error)
    try:
        return query(isbns)
    except RuntimeError:
        if len(isbns) <= 1:
            raise
        results = []
        for isbn in isbns:
            results.extend(query([isbn]))
        return results


def run(args):
    ranked = json.loads(Path(args.ranked).read_text())
    works = {row['work_id']: row for row in ranked['popular'] + ranked['recent']}
    works = sorted(works.values(), key=lambda row: (-row['ratings'], -(row['earliest_recorded_year'] or 0), row['work_id']))
    output = Path(args.output)
    report = json.loads(output.read_text()) if output.exists() else dict(started_at=time.time(), results={}, attempts=[])
    report.update(total_works=len(works), status='running', database=str(Path(args.database).resolve()))
    with closing(sqlite3.connect(Path(args.database).resolve().as_uri() + '?mode=ro', uri=True)) as db:
        env = dict(os.environ, BOKHEIM_LIBRARYTHING_KEY_FILE=args.key_file, BOKHEIM_LIBRARYTHING_CACHE=args.cache, RUST_LOG='error')
        stop_reason = None
        consecutive_failures = 0
        failure_limit = getattr(args, "max_consecutive_failures", 5)
        for round_number in range(3):
            for row in works:
                previous = report['results'].get(row['work_id'])
                if previous and previous['status'] != 'request_failed':
                    continue
                number = int(row['work_id'][2:-1])
                isbns = sorted({str(r[0]) for r in db.execute('SELECT i.isbn13 FROM edition e JOIN edition_isbn i ON i.edition_id=e.edition_id WHERE e.work_id=?', (number,))})
                # Check the normal lookup, including duplicate-work recovery, before API calls.
                local_results = local_lookup(args, env, isbns)
                locally_resolved = [result for result in local_results if any(c['scheme'] == 'library_of_congress' for m in result['matches'] for c in m['classifications'])]
                if locally_resolved:
                    report['results'][row['work_id']] = dict(**row, status='already_resolved_locally', librarything=None, checked_isbns=[], errors=[], local_evidence=locally_resolved)
                    report['updated_at'] = time.time()
                    save(output, report)
                    print(json.dumps(dict(done=len(report['results']), total=len(works), status='already_resolved_locally', last_title=row['title'])), flush=True)
                    continue
                attempted, found, errors = [], None, []
                for isbn in isbns:
                    process = subprocess.run([args.binary, 'librarything-refresh-schemes' if getattr(args, 'refresh_schemes', False) else 'librarything-lookup', isbn], env=env, capture_output=True, text=True, timeout=30)
                    if process.returncode:
                        message = process.stderr.strip()[-1200:]
                        errors.append(dict(isbn=isbn, error=message))
                        report['attempts'].append(dict(work_id=row['work_id'], isbn=isbn, round=round_number + 1, error=message, at=time.time()))
                        consecutive_failures += 1
                        if 'daily request limit' in message:
                            stop_reason = 'quota_exhausted'
                        elif 'HTTP 429' in message:
                            stop_reason = 'rate_limited'
                        elif consecutive_failures >= failure_limit:
                            stop_reason = 'consecutive_provider_failures'
                        if stop_reason:
                            break
                        continue
                    consecutive_failures = 0
                    result = json.loads(process.stdout)
                    attempted.append(dict(isbn=isbn, **result))
                    if result['work_id']:
                        found = attempted[-1]
                        break
                status = ('lcc_found' if found['codes'] else 'other_classification_found' if found.get('ddc') or found.get('bisac') else 'matched_without_lcc') if found else ('request_failed' if errors else 'no_librarything_work')
                report['results'][row['work_id']] = dict(**row, status=status, librarything=found, checked_isbns=attempted, errors=errors)
                report['updated_at'] = time.time()
                save(output, report)
                counts = {status: sum(r['status'] == status for r in report['results'].values()) for status in ['lcc_found', 'other_classification_found', 'matched_without_lcc', 'no_librarything_work', 'request_failed', 'already_resolved_locally']}
                print(json.dumps(dict(done=len(report['results']), total=len(works), counts=counts, last_title=row['title'], round=round_number + 1)), flush=True)
                if stop_reason:
                    break
            if stop_reason or not any(r['status'] == 'request_failed' for r in report['results'].values()):
                break
            time.sleep(5)
        report['status'] = stop_reason or 'complete'
        report['consecutive_failures'] = consecutive_failures
        report['unprocessed_works'] = len(works) - len(report['results'])
        report['completed_at'] = time.time()
        save(output, report)
        with output.with_suffix('.csv').open('w', newline='') as stream:
            fields = ['work_id', 'title', 'ratings', 'earliest_recorded_year', 'status', 'librarything_work_id', 'lookup_isbn', 'lcc', 'ddc', 'bisac']
            writer = csv.DictWriter(stream, fieldnames=fields)
            writer.writeheader()
            for row in report['results'].values():
                lt = row['librarything'] or {}
                writer.writerow({**{k: row[k] for k in fields[:5]}, 'librarything_work_id': lt.get('work_id', ''), 'lookup_isbn': lt.get('isbn', ''), 'lcc': '; '.join(lt.get('codes', [])), 'ddc': '; '.join(lt.get('ddc', [])), 'bisac': '; '.join(lt.get('bisac', []))})



if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    for flag in ['ranked', 'database', 'binary', 'key-file', 'cache', 'output']:
        p.add_argument('--' + flag, required=True)
    p.add_argument('--refresh-schemes', action='store_true', help='Explicit one-time upgrade of legacy no-LCC cache results')
    p.add_argument('--max-consecutive-failures', type=int, default=5)
    run(p.parse_args())
