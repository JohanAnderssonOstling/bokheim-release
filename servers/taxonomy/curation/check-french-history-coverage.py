#!/usr/bin/env python3
"""Check French DC coverage using the production matcher in an optimized build.

Usage: check-french-history-coverage.py BEFORE AFTER --matcher RELEASE_BINARY
       [--usage CODE_USAGE.sqlite3] [--report OUTPUT.json]
"""
import argparse
import csv
import hashlib
import io
import json
from pathlib import Path
import sqlite3
import subprocess
import tempfile


def readonly(path):
    return sqlite3.connect(Path(path).resolve().as_uri() + '?mode=ro', uri=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('before', type=Path)
    parser.add_argument('after', type=Path)
    parser.add_argument('--matcher', type=Path, required=True)
    parser.add_argument('--usage', type=Path)
    parser.add_argument('--report', type=Path)
    args = parser.parse_args()
    # All integer classes and decimal/Cutter probes in the French portion of DC.
    # Andorra and Monaco remain outside this French-history coverage assertion.
    probes = {f'DC{n}{suffix}' for n in range(1, 920)
              for suffix in ('', '.C3 1989', '.5') if n < 919 or suffix != '.5'}
    expected = {
        'DC38': 'General & Cross-period French History',
        'DC38 .P75 1992': 'General & Cross-period French History',
        'DC38 .P7518 1996': 'General & Cross-period French History',
        'DC161': 'Revolution & Napoleon, 1789–1815',
        'DC161 .C3 1989': 'Revolution & Napoleon, 1789–1815',
        'DC161.C3 1989': 'Revolution & Napoleon, 1789–1815',
        'DC197': 'First Empire, 1804–1815',
        'DC21': 'French Travel',
        'DC611.B841': 'Brittany',
        'DC715': 'Paris Social & Cultural History',
        'DC941': 'Monaco History',
    }
    codes = probes | set(expected) | {'DC921', 'DC932.C35', 'DC947', 'DD197', 'E310', 'QA76'}
    if args.usage:
        with readonly(args.usage) as db:
            codes.update(row[0] for row in db.execute(
                "SELECT notation FROM code_usage WHERE scheme=2 AND notation LIKE 'DC%'"))
    with tempfile.TemporaryDirectory(prefix='french-history-check-') as temp:
        code_db = Path(temp) / 'codes.sqlite3'
        with sqlite3.connect(code_db) as db:
            db.execute('CREATE TABLE code_usage(scheme INTEGER, notation TEXT)')
            db.executemany('INSERT INTO code_usage VALUES(2,?)', [(c,) for c in sorted(codes)])
        def matches(taxonomy):
            result = subprocess.run([str(args.matcher.resolve()), str(taxonomy), str(code_db)],
                                    text=True, capture_output=True, check=True)
            rows = list(csv.DictReader(io.StringIO(result.stdout)))
            assert not any(r['kind'] == 'selector_conflict' for r in rows), 'Exact selector conflict'
            return {r['code']: r['concept_ids'] for r in rows}
        before, after = matches(args.before), matches(args.after)
    assert all(after[c] and '|' not in after[c] for c in probes), 'French range has gaps or ambiguities'
    changed_existing = [c for c in codes if before[c] and before[c] != after[c]]
    assert not changed_existing, f'Existing destinations changed: {changed_existing[:20]}'
    assert not any('|' in after[c] and before[c] != after[c] for c in codes), 'New ambiguity'
    with readonly(args.after) as db:
        labels = dict(db.execute('SELECT concept_id,preferred_label FROM concept'))
        french = {r[0] for r in db.execute('''WITH RECURSIVE d(id) AS (
            SELECT concept_id FROM concept_parent WHERE parent_concept_id=3523
            UNION SELECT p.concept_id FROM concept_parent p JOIN d ON p.parent_concept_id=d.id
        ) SELECT id FROM d''')}
        for code, label in expected.items():
            assert after[code] and '|' not in after[code], (code, after[code])
            assert labels[int(after[code])] == label, (code, after[code], label)
        for code in ('DC38', 'DC161', 'DC251', 'DC356', 'DC425', 'DC600'):
            assert int(after[code]) in french, f'{code} must belong to a French History child'
        assert db.execute('PRAGMA integrity_check').fetchone() == ('ok',)
        assert not db.execute('PRAGMA foreign_key_check').fetchall()
    report = {
        'before_sha256': hashlib.sha256(args.before.read_bytes()).hexdigest(),
        'after_sha256': hashlib.sha256(args.after.read_bytes()).hexdigest(),
        'tested_codes': len(codes), 'continuous_range_probes': len(probes),
        'new_matches': sum(not before[c] and bool(after[c]) for c in codes),
        'changed_existing_destinations': 0, 'new_ambiguities': 0,
        'coverage_scope': 'French portion DC1–DC919; specific travel matches preserved; Andorra and Monaco unchanged',
        'examples': {code: labels[int(after[code])] for code in expected},
    }
    if args.report:
        args.report.write_text(json.dumps(report, indent=2, ensure_ascii=False) + '\n')
    print(json.dumps(report, indent=2, ensure_ascii=False))


if __name__ == '__main__':
    main()
