#!/usr/bin/env python3
"""Build an unlimited classification-frequency cache from an Open Library snapshot.

Counts preserve complete classifications and edition/work semantics.
The source is opened read-only. Only the fully validated cache is published.
"""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import sqlite3
import tempfile

QUERY = """
WITH raw(source_table,scheme,notation) AS (
 SELECT 'edition_classification',scheme,notation FROM edition_classification
 UNION ALL SELECT 'work_classification',scheme,notation FROM work_classification
), compact AS (
 SELECT source_table,scheme,trim(notation) AS notation FROM raw
)
SELECT scheme,notation,COUNT(*),
       SUM(source_table='edition_classification'),SUM(source_table='work_classification')
FROM compact GROUP BY scheme,notation
"""

def rebuild(source_path, output):
    source_path, output = Path(source_path).resolve(strict=True), Path(output).absolute()
    if output.resolve() == source_path:
        raise ValueError('The cache must not replace the source snapshot')
    output.parent.mkdir(parents=True, exist_ok=True)
    if output.exists():
        raise FileExistsError(f'Build to a new output path: {output}')
    source_stat = source_path.stat()
    source = sqlite3.connect(source_path.as_uri() + '?mode=ro', uri=True)
    source.execute('PRAGMA query_only=ON')
    source.execute('PRAGMA temp_store=FILE')
    source.execute('PRAGMA cache_size=-65536')
    source.execute('BEGIN')
    snapshot = {}
    if source.execute("SELECT 1 FROM sqlite_master WHERE name='snapshot'").fetchone():
        cursor = source.execute('SELECT * FROM snapshot')
        snapshot = dict(zip([x[0] for x in cursor.description], cursor.fetchone()))
    # Count source contributions in the grouping pass instead of scanning each
    # large source table twice. Compare these source counts with persisted uses.
    totals = {'edition_classification': {}, 'work_classification': {}}
    handle, temporary = tempfile.mkstemp(prefix='.' + output.name + '-', suffix='.building', dir=output.parent)
    os.close(handle)
    target = sqlite3.connect(temporary)
    try:
        target.executescript('''
        CREATE TABLE code_usage(scheme INTEGER NOT NULL,notation TEXT NOT NULL,
          uses INTEGER NOT NULL CHECK(uses>0),PRIMARY KEY(scheme,notation)) WITHOUT ROWID;
        CREATE TABLE cache_metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL) WITHOUT ROWID;
        ''')
        target.execute('BEGIN')
        cursor = source.execute(QUERY)
        count = 0
        while batch := cursor.fetchmany(10000):
            target.executemany('INSERT INTO code_usage VALUES(?,?,?)', (row[:3] for row in batch))
            for scheme, _, _, editions, works in batch:
                for table, uses in [('edition_classification', editions), ('work_classification', works)]:
                    if uses:
                        totals[table][scheme] = totals[table].get(scheme, 0) + uses
            count += len(batch)
            if count % 100000 == 0:
                print(f'Cached {count:,} distinct classifications', flush=True)
        target.commit()
        actual = dict(target.execute('SELECT scheme,SUM(uses) FROM code_usage GROUP BY scheme'))
        expected = {k:sum(t.get(k,0) for t in totals.values()) for t in totals.values() for k in t}
        if actual != expected:
            raise ValueError(f'Count conservation failed: {actual} != {expected}')
        if (source_path.stat().st_size,source_path.stat().st_mtime_ns) != (source_stat.st_size,source_stat.st_mtime_ns):
            raise ValueError('Source snapshot changed during the build')
        metadata = dict(complete_for_source=True, row_limit=None, minimum_frequency=1,
            built_at=datetime.now(timezone.utc).isoformat(), source_path=str(source_path),
            source_bytes=source_stat.st_size, source_mtime_ns=source_stat.st_mtime_ns,
            snapshot=snapshot, source_counts=totals, distinct_codes=count,
            normalization_version=2,
            normalization='Trim outer ASCII spaces only; preserve complete notation, including internal spaces, Cutters and dates.',
            count_unit='Classification rows from edition_classification plus work_classification; not unique books.',
            coverage='All classifications retained in the supplied snapshot; snapshot import may exclude editions without valid ISBNs.')
        target.executemany('INSERT INTO cache_metadata VALUES(?,?)', [(k,json.dumps(v)) for k,v in metadata.items()])
        target.execute('CREATE INDEX code_usage_by_uses ON code_usage(uses DESC,scheme,notation)')
        target.commit()
        if target.execute('PRAGMA integrity_check').fetchone()[0] != 'ok':
            raise ValueError('Cache integrity check failed')
        target.close()
        source.close()
        os.replace(temporary, output)
        print(json.dumps(metadata,indent=2),flush=True)
        return metadata
    except BaseException:
        target.close()
        source.close()
        # An incomplete build is never installed as the cache.
        Path(temporary).unlink(missing_ok=True)
        raise

if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source',type=Path)
    parser.add_argument('output',type=Path)
    args=parser.parse_args()
    rebuild(args.source,args.output)
