#!/usr/bin/env python3
"""Stream LC UTF-8 MARC (plain or gzip) into the existing metadata snapshot.

Writes a new snapshot; never changes the source or publishes a partial import.
Usage: import-loc-lcc.py SOURCE.sqlite OUTPUT.sqlite MARC... [--dump-date 2016]
"""
import argparse
import contextlib
import gzip
import json
import os
from pathlib import Path
import re
import sqlite3
import tempfile
import time
import unicodedata

IMPORT_VERSION = 2

def title_key(value):
    value = "".join(c.lower() for c in unicodedata.normalize("NFKD", value) if not unicodedata.category(c).startswith("M"))
    return " ".join("".join(c if c.isalnum() else " " for c in value).split())


def isbn13(value):
    match = re.match(r'^\s*([0-9Xx][0-9Xx -]*)', value)
    if not match:
        return None
    s = re.sub(r'[ -]', '', match[1]).upper()
    if len(s) == 10 and s[:9].isdigit() and (s[-1].isdigit() or s[-1] == 'X'):
        if sum((10-i) * (10 if c == 'X' else int(c)) for i, c in enumerate(s)) % 11:
            return None
        s = '978' + s[:9]
        s += str((-sum(int(c) * (1 if i % 2 == 0 else 3) for i, c in enumerate(s))) % 10)
    if len(s) != 13 or not s.isdigit() or not s.startswith(('978', '979')):
        return None
    if sum(int(c) * (1 if i % 2 == 0 else 3) for i, c in enumerate(s)) % 10:
        return None
    return int(s)


def records(stream):
    """MARC directory offsets are byte offsets, including for multibyte UTF-8."""
    while True:
        prefix = stream.read(5)
        if not prefix:
            return
        if len(prefix) != 5 or not prefix.isdigit():
            raise ValueError('Invalid or truncated MARC record length')
        length = int(prefix)
        if not 25 <= length <= 99999:
            raise ValueError('Invalid MARC record length')
        record = prefix + stream.read(length - 5)
        if len(record) != length or record[-1:] != b'\x1d':
            raise ValueError('Truncated MARC record')
        base = int(record[12:17])
        if base < 25 or base >= length or record[base-1:base] != b'\x1e' or (base - 25) % 12:
            raise ValueError('Invalid MARC directory')
        fields = []
        for offset in range(24, base - 1, 12):
            entry = record[offset:offset+12]
            tag = entry[:3].decode('ascii')
            size, start = int(entry[3:7]), base + int(entry[7:12])
            if size < 1 or start < base or start + size > length - 1 or record[start+size-1:start+size] != b'\x1e':
                raise ValueError('Invalid MARC field bounds')
            raw = record[start:start+size-1]
            if tag < '010':
                fields.append((tag, '', [('', raw.decode('utf-8').strip())]))
            elif tag in {'010','020','041','050','100','110','111','245','260','264','520','600','610','611','630','648','650','651','655','700','710','711'}:
                subs = [(part[:1].decode('ascii'), part[1:].decode('utf-8').strip()) for part in raw[2:].split(b'\x1f')[1:] if part]
                fields.append((tag, raw[:2].decode('ascii'), subs))
        yield record[5:6] == b'd', fields


def extract(fields):
    def values(tag, code):
        return [v for t, _, subs in fields if t == tag for c, v in subs if c == code]
    record_id = next(iter(values('001', '') or values('010', 'a')), None)
    isbns = {i for value in values('020', 'a') if (i := isbn13(value)) is not None}
    # Only classification subfield 050$a, never the item number/year in 050$b.
    lcc = {' '.join(v.split()) for v in values('050', 'a') if re.match(r'^[A-Z]{1,3}(?:\s*\d|$)', v) and len(v) <= 256}
    subjects = []
    for tag, indicators, subs in fields:
        if tag not in {'600','610','611','630','648','650','651','655'}:
            continue
        components = [v for code, v in subs if code in 'avxyz' and v]
        authority = next((v for code, v in subs if code == '2'), 'lcsh' if indicators[1:2] == '0' else 'unspecified')
        uri = next((v for code, v in subs if code == '0' and v.startswith(('https://id.loc.gov/', 'http://id.loc.gov/'))), None)
        if components:
            subjects.append(dict(source='library_of_congress', authority=authority, components=components, authority_uri=uri))
    metadata = {
        'title': ' '.join(values('245', 'a') + values('245', 'b') + values('245', 'n') + values('245', 'p')),
        'main_title': ' '.join(values('245', 'a') + values('245', 'n') + values('245', 'p')),
        'authors': [v for tag in ('100','110','111','700','710','711') for v in values(tag, 'a')],
        'publishers': values('264', 'b') or values('260', 'b'),
        'publication_dates': values('264', 'c') or values('260', 'c'),
        'languages': values('041', 'a'),
        'descriptions': values('520', 'a'),
        'subjects': subjects,
    }
    return record_id, isbns, lcc, metadata


def import_dump(source, output, inputs, dump_date='2016', resume_building=None):
    source, output = Path(source), Path(output)
    if output.exists():
        raise FileExistsError(output)
    if not source.is_file():
        raise FileNotFoundError(source)
    records_read = 0
    staging = contextlib.nullcontext(None) if resume_building else tempfile.TemporaryDirectory(prefix=output.name + '.building-', dir=output.parent)
    if resume_building and not Path(resume_building).is_file():
        raise FileNotFoundError(resume_building)
    with staging as temp:
        staged = Path(resume_building) if resume_building else Path(temp) / 'snapshot.sqlite'
        with contextlib.closing(sqlite3.connect(source.resolve().as_uri() + '?mode=ro', uri=True)) as src, contextlib.closing(sqlite3.connect(staged)) as db:
            if resume_building:
                identity = 'SELECT schema_version,dump_date,imported_at_ms FROM snapshot WHERE singleton=1'
                if src.execute(identity).fetchone() != db.execute(identity).fetchone():
                    raise ValueError('Resume source snapshot differs from the interrupted import')
            else:
                src.backup(db)
            db.execute('PRAGMA cache_size=-262144')
            db.execute('SELECT dump_date FROM snapshot WHERE singleton=1').fetchone()
            if resume_building:
                try:
                    version = db.execute('SELECT version FROM lc_import_version').fetchone()
                except sqlite3.OperationalError as exc:
                    raise ValueError('Cannot resume an older LC import; start a fresh full import') from exc
                if version != (IMPORT_VERSION,):
                    raise ValueError('LC import version changed; start a fresh full import')
            db.executescript('''
                CREATE TABLE IF NOT EXISTS lc_import_version(version INTEGER NOT NULL);
                CREATE TABLE IF NOT EXISTS lc_record_lcc(
                    lc_record_id TEXT NOT NULL, notation TEXT NOT NULL,
                    PRIMARY KEY(lc_record_id,notation)
                ) WITHOUT ROWID;
                CREATE TABLE IF NOT EXISTS lc_title(
                    title_key TEXT NOT NULL, lc_record_id TEXT NOT NULL,
                    PRIMARY KEY(title_key,lc_record_id)
                ) WITHOUT ROWID;
                CREATE INDEX IF NOT EXISTS lc_title_record ON lc_title(lc_record_id);
                CREATE TABLE IF NOT EXISTS isbn_lcc(
                    isbn13 INTEGER NOT NULL, notation TEXT NOT NULL, lc_record_id TEXT NOT NULL,
                    PRIMARY KEY(isbn13,notation,lc_record_id)
                ) WITHOUT ROWID;
                CREATE INDEX IF NOT EXISTS isbn_lcc_record ON isbn_lcc(lc_record_id);
                CREATE TABLE IF NOT EXISTS lc_record(
                    lc_record_id TEXT PRIMARY KEY, metadata TEXT NOT NULL
                ) WITHOUT ROWID;
                CREATE TABLE IF NOT EXISTS lc_record_isbn(
                    isbn13 INTEGER NOT NULL, lc_record_id TEXT NOT NULL,
                    PRIMARY KEY(isbn13,lc_record_id)
                ) WITHOUT ROWID;
                CREATE INDEX IF NOT EXISTS lc_record_isbn_record ON lc_record_isbn(lc_record_id);
                CREATE TABLE IF NOT EXISTS lcc_import(
                    input_name TEXT PRIMARY KEY, dump_date TEXT NOT NULL, records_read INTEGER NOT NULL
                ) WITHOUT ROWID;
            ''')
            # This is a complete replacement of the LC supplement, not OL metadata.
            if not resume_building:
                for table in ('isbn_lcc', 'lc_record', 'lc_record_isbn', 'lcc_import', 'lc_record_lcc', 'lc_title', 'lc_import_version'):
                    db.execute('DELETE FROM ' + table)
            if not resume_building:
                db.execute('INSERT INTO lc_import_version VALUES(?)', (IMPORT_VERSION,))
            completed = dict(db.execute('SELECT input_name,records_read FROM lcc_import'))
            for path in map(Path, inputs):
                if str(path) in completed:
                    records_read += completed[str(path)]
                    print(json.dumps({'input': path.name, 'resumed_completed_records': completed[str(path)]}), flush=True)
                    continue
                count = 0
                opener = gzip.open if path.suffix == '.gz' else open
                with opener(path, 'rb') as stream:
                    for deleted, fields in records(stream):
                        count += 1
                        record_id, isbns, lcc, metadata = extract(fields)
                        if record_id:
                            for table in ('isbn_lcc', 'lc_record', 'lc_record_isbn', 'lc_record_lcc', 'lc_title'):
                                db.execute('DELETE FROM ' + table + ' WHERE lc_record_id=?', (record_id,))
                            if not deleted:
                                db.execute('INSERT INTO lc_record VALUES(?,?)', (record_id, json.dumps(metadata, ensure_ascii=False)))
                                db.executemany('INSERT INTO lc_record_lcc VALUES(?,?)', [(record_id, code) for code in lcc])
                                keys = {title_key(metadata[k]) for k in ('title', 'main_title')} - {''}
                                if lcc:
                                    db.executemany('INSERT INTO lc_title VALUES(?,?)', [(k, record_id) for k in keys])
                                db.executemany('INSERT INTO lc_record_isbn VALUES(?,?)', [(i, record_id) for i in isbns])
                                db.executemany('INSERT INTO isbn_lcc VALUES(?,?,?)', [(i, code, record_id) for i in isbns for code in lcc])
                        if count % 50000 == 0:
                            db.commit()
                            print(json.dumps({'input': path.name, 'records_read': count}), flush=True)
                db.execute('INSERT INTO lcc_import VALUES(?,?,?)', (str(path), dump_date, count))
                db.commit()
                records_read += count
                print(json.dumps({'input': path.name, 'completed_records': count}), flush=True)
            # Invalidate existing mmap indexes without changing the OL dump date.
            db.execute('UPDATE snapshot SET imported_at_ms=max(imported_at_ms+1,?) WHERE singleton=1', (int(time.time()*1000),))
            db.commit()
            for table in ('isbn_lcc', 'lc_title', 'lc_record_lcc'):
                db.execute('ANALYZE ' + table)
            if db.execute('PRAGMA integrity_check').fetchone() != ('ok',):
                raise ValueError('Imported snapshot failed integrity check')
            result = dict(records_read=records_read, mappings=db.execute('SELECT count(*) FROM isbn_lcc').fetchone()[0],
                          isbns=db.execute('SELECT count(DISTINCT isbn13) FROM isbn_lcc').fetchone()[0],
                          retained_records=db.execute('SELECT count(*) FROM lc_record').fetchone()[0],
                          lcc_records_without_isbn=db.execute('SELECT count(*) FROM (SELECT lc_record_id FROM lc_record_lcc GROUP BY lc_record_id) r WHERE NOT EXISTS (SELECT 1 FROM lc_record_isbn i WHERE i.lc_record_id=r.lc_record_id)').fetchone()[0])
        # Atomic publication without overwriting an output created concurrently.
        os.link(staged, output)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('inputs', type=Path, nargs='+')
    parser.add_argument('--dump-date', default='2016')
    parser.add_argument('--resume-building', type=Path, help='Interrupted staging SQLite; use the same source and immutable dump parts')
    args = parser.parse_args()
    print(json.dumps(import_dump(args.source, args.output, args.inputs, args.dump_date, args.resume_building)))


if __name__ == '__main__':
    main()
