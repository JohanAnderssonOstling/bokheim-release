#!/usr/bin/env python3
"""Preview/apply broad Law coverage, preserving structural/special selectors.

Uses the existing release production matcher with display labels isolated from
unrelated route collisions. Does not modify any descendant or other subject.
"""
import argparse
import csv
import io
import json
from pathlib import Path
import sqlite3
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
DB = ROOT / 'shared/subject-projection/data/unified-taxonomy-v2.sqlite3'
PROBE = ROOT / 'target/release/taxonomy_probe'


def rows(conn):
    tables = [r[0] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")]
    return {t: sorted(conn.execute(f'SELECT * FROM "{t}"').fetchall()) for t in tables}


def matches(path, codes):
    result = {}
    for offset in range(0, len(codes), 200):
        batch = codes[offset:offset + 200]
        run = subprocess.run([str(PROBE), str(path), '--code-only', *('lcc:' + c for c in batch)], check=True, capture_output=True, text=True)
        result.update({r['code']: r['resolved'] for r in csv.DictReader(io.StringIO(run.stdout))})
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apply', action='store_true')
    args = parser.parse_args()
    directory = Path(tempfile.mkdtemp(prefix='law-selectors-', dir=ROOT / 'tmp'))
    before_path, after_path = directory / 'before.sqlite3', directory / 'after.sqlite3'
    with sqlite3.connect(f'file:{DB}?mode=ro', uri=True) as source:
        with sqlite3.connect(before_path) as before:
            source.backup(before)
        with sqlite3.connect(after_path) as after:
            source.backup(after)
    with sqlite3.connect(before_path) as before, sqlite3.connect(after_path) as after:
        snapshot = rows(before)
        assert before.execute('SELECT preferred_label FROM concept WHERE concept_id=5276').fetchone() == ('Law',)
        after.execute("DELETE FROM lcc_selector WHERE concept_id=5276 AND selector LIKE 'K%' AND selector GLOB '*[^A-Z]*'")
        # These structural ranges are still needed by subclass/plus-span parsing.
        # The bundled outline alone does not supply equivalent evidence.
        after.execute("DELETE FROM lcc_range WHERE concept_id=5276 AND start_code LIKE 'K%' AND end_code LIKE 'K%' AND start_code NOT GLOB 'KFP*' AND start_code NOT GLOB 'KFX*' AND start_code NOT GLOB 'KKM*' AND start_code NOT GLOB 'KNX*'")
        after.execute("INSERT INTO lcc_range VALUES('K','KZ',5276)")
        after.commit()
        updated = rows(after)
        for table in snapshot:
            if table not in ('lcc_selector', 'lcc_range'):
                assert snapshot[table] == updated[table], table
        for table, index in [('lcc_selector', 0), ('lcc_range', 2)]:
            assert [r for r in snapshot[table] if r[index] != 5276] == [r for r in updated[table] if r[index] != 5276]
        codes = {'K', '(K)', 'JX', 'KF500', 'K3151', 'K3225', 'KZ7146', 'KZ118', 'KZ5638', 'QA76', 'D100'}
        codes.update(r[0] for r in before.execute("SELECT selector FROM lcc_selector WHERE selector LIKE 'K%'") if '*' not in r[0])
        for start, end in before.execute("SELECT start_code,end_code FROM lcc_range WHERE start_code LIKE 'K%' OR concept_id=5276"):
            codes.update((start, end, start + '..' + end))
        cache = Path.home() / '.cache/bokheim/openlibrary-code-usage.sqlite3'
        if cache.exists():
            with sqlite3.connect(f'file:{cache}?mode=ro', uri=True) as usage:
                codes.update(r[0] for r in usage.execute("SELECT notation FROM code_usage WHERE scheme=2 AND (notation LIKE 'K%' OR notation LIKE 'JX%') ORDER BY uses DESC LIMIT 2000"))
        codes = sorted(codes)
    old, new = matches(before_path, codes), matches(after_path, codes)
    changes = [{'code': c, 'before': old[c], 'after': new[c]} for c in codes if old[c] != new[c]]
    losses = [r for r in changes if r['before'] and not r['after']]
    narrower_changes = [r for r in changes if r['before'] and r['before'] != '5276']
    report = {'probes': len(codes), 'losses': losses, 'changed_existing_non_law_destinations': narrower_changes, 'changes': changes}
    (directory / 'audit.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'directory': str(directory), 'probes': len(codes), 'changes': len(changes), 'losses': len(losses), 'changed_existing_non_law_destinations': len(narrower_changes)}), flush=True)
    assert not losses and not narrower_changes, 'Review audit before applying'
    if args.apply:
        with sqlite3.connect(DB) as live:
            live.execute('BEGIN IMMEDIATE')
            assert rows(live) == snapshot, 'Taxonomy changed during review; rerun'
            live.execute("DELETE FROM lcc_selector WHERE concept_id=5276 AND selector LIKE 'K%' AND selector GLOB '*[^A-Z]*'")
            live.execute("DELETE FROM lcc_range WHERE concept_id=5276 AND start_code LIKE 'K%' AND end_code LIKE 'K%' AND start_code NOT GLOB 'KFP*' AND start_code NOT GLOB 'KFX*' AND start_code NOT GLOB 'KKM*' AND start_code NOT GLOB 'KNX*'")
            live.execute("INSERT INTO lcc_range VALUES('K','KZ',5276)")
            assert rows(live) == updated
            assert live.execute('PRAGMA integrity_check').fetchone() == ('ok',)
            assert not live.execute('PRAGMA foreign_key_check').fetchall()
        print('Applied Law fallback; original snapshot retained in audit directory.')


if __name__ == '__main__':
    main()
