#!/usr/bin/env python3
"""Read-only ranking of rated Open Library works lacking known LCC evidence."""
import argparse
from collections import Counter
import csv
from datetime import datetime, timezone
import gzip
import json
from pathlib import Path
import re
import sqlite3


def readonly(path):
    return sqlite3.connect(Path(path).resolve().as_uri() + '?mode=ro', uri=True)


def rank(database, ratings_path, librarything_cache, limit, since, excluded=None, popular_only=False, any_subject_code=False):
    excluded = excluded or set()
    scheme_filter = "scheme IN (1,2,3)" if any_subject_code else "scheme=2"
    ratings = Counter()
    with gzip.open(ratings_path, 'rt') as stream:
        for line in stream:
            match = re.fullmatch(r'/works/OL(\d+)W', line.split('\t', 1)[0])
            if match:
                ratings[int(match[1])] += 1
    db = readonly(database)
    # Hold a consistent read snapshot, even if the current symlink is activated elsewhere.
    db.execute('BEGIN')
    lt = readonly(librarything_cache) if librarything_cache else None
    if lt:
        lt.execute('BEGIN')
    popular, recent = [], []
    checked = 0
    cutoff = None
    current_year = datetime.now(timezone.utc).year
    try:
        for work, count in sorted(ratings.items(), key=lambda item: (-item[1], item[0])):
            # Finish the entire rating-count tie before using year as the secondary key.
            if cutoff is not None and count < cutoff:
                break
            checked += 1
            if f'OL{work}W' in excluded:
                continue
            if db.execute(f'SELECT 1 FROM work_classification WHERE work_id=? AND {scheme_filter} LIMIT 1', (work,)).fetchone():
                continue
            if db.execute(f'SELECT 1 FROM edition_work_classification WHERE work_id=? AND {scheme_filter} LIMIT 1', (work,)).fetchone():
                continue
            editions = [r[0] for r in db.execute('SELECT edition_id FROM edition WHERE work_id=?', (work,))]
            if not editions:
                continue
            isbns = set()
            coded = False
            for edition in editions:
                if db.execute(f'SELECT 1 FROM edition_classification WHERE edition_id=? AND {scheme_filter} LIMIT 1', (edition,)).fetchone():
                    coded = True
                    break
                isbns.update(r[0] for r in db.execute('SELECT isbn13 FROM edition_isbn WHERE edition_id=?', (edition,)))
            if coded:
                continue
            tried = 0
            for isbn in sorted(isbns):
                # An ISBN can be attached to several OL work records. Inspect every
                # associated edition/work, not only the work currently being ranked.
                shared = db.execute('SELECT e.edition_id,e.work_id FROM edition_isbn i JOIN edition e USING(edition_id) WHERE i.isbn13=?', (isbn,)).fetchall()
                for shared_edition, shared_work in shared:
                    if (db.execute(f'SELECT 1 FROM edition_classification WHERE edition_id=? AND {scheme_filter} LIMIT 1', (shared_edition,)).fetchone()
                        or db.execute(f'SELECT 1 FROM work_classification WHERE work_id=? AND {scheme_filter} LIMIT 1', (shared_work,)).fetchone()
                        or db.execute(f'SELECT 1 FROM edition_work_classification WHERE work_id=? AND {scheme_filter} LIMIT 1', (shared_work,)).fetchone()):
                        coded = True
                        break
                if coded:
                    break
                if db.execute('SELECT 1 FROM isbn_lcc WHERE isbn13=? LIMIT 1', (isbn,)).fetchone():
                    coded = True
                    break
                if lt:
                    result = lt.execute('SELECT result FROM librarything_isbn_lcc WHERE isbn13=?', (str(isbn),)).fetchone()
                    if result:
                        tried += 1
                        if any(json.loads(result[0]).get(k) for k in (['codes', 'ddc', 'bisac'] if any_subject_code else ['codes'])):
                            coded = True
                            break
            if coded:
                continue
            title = db.execute('SELECT title FROM work_bibliography WHERE work_id=?', (work,)).fetchone()
            years, edition_titles = [], []
            for edition in editions:
                biblio = db.execute('SELECT title,book_year FROM edition_bibliography WHERE edition_id=?', (edition,)).fetchone()
                if biblio:
                    edition_titles.append(biblio[0])
                    if biblio[1] and 1 <= biblio[1] <= current_year:
                        years.append(biblio[1])
            authors = [r[0] for r in db.execute('SELECT a.name FROM work_author w JOIN author a USING(author_id) WHERE w.work_id=? ORDER BY w.position', (work,)) if r[0]]
            year = min(years) if years else None
            row = dict(work_id=f'OL{work}W', title=title[0] if title else next(iter(edition_titles), ''), authors=authors,
                       ratings=count, earliest_recorded_year=year, isbn_count=len(isbns), librarything_tried_isbns=tried,
                       untried_isbn_count=len(isbns) - tried, example_isbns=[str(i) for i in sorted(isbns)[:5]])
            popular.append(row)
            if year is not None and year >= since:
                recent.append(row)
            if len(popular) >= limit and (popular_only or len(recent) >= limit):
                cutoff = count
        key = lambda row: (-row['ratings'], -(row['earliest_recorded_year'] or 0), row['work_id'])
        return dict(generated_at=datetime.now(timezone.utc).isoformat(), database=str(Path(database).resolve()),
                    ratings_dump=str(Path(ratings_path).resolve()), librarything_cache=str(librarything_cache) if librarything_cache else None,
                    rated_works=len(ratings), examined_works=checked, recent_since=since,
                    any_subject_code=any_subject_code, method='Works ranked by rating count descending, then earliest retained edition year descending. Excludes LCC in OL work/edition data across all records sharing any ISBN, LC ISBN supplement, and completed LibraryThing ISBN cache. Unrated works excluded. Earliest recorded year is not verified first book.',
                    popular=sorted(popular, key=key)[:limit], recent=[] if popular_only else sorted(recent, key=key)[:limit], excluded_works=len(excluded), popular_only=popular_only)
    finally:
        db.close()
        if lt:
            lt.close()


def write_csv(path, rows):
    fields = ['work_id', 'title', 'authors', 'ratings', 'earliest_recorded_year', 'isbn_count', 'librarything_tried_isbns', 'untried_isbn_count', 'example_isbns']
    with path.open('w', newline='') as stream:
        writer = csv.DictWriter(stream, fieldnames=fields)
        writer.writeheader()
        for row in rows:
            writer.writerow({k: '; '.join(v) if isinstance(v, list) else v for k, v in row.items()})


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--database', required=True)
    parser.add_argument('--ratings', required=True)
    parser.add_argument('--librarything-cache')
    parser.add_argument('--limit', type=int, default=100)
    parser.add_argument('--since', type=int, default=2020)
    parser.add_argument('--exclude-audit', action='append', default=[], help='Skip completed works in earlier audit JSON reports')
    parser.add_argument('--any-subject-code', action='store_true', help='Exclude known LCC, DDC and BISAC, not just LCC')
    parser.add_argument('--popular-only', action='store_true', help='Rank by popularity across all years, without a separate recent list')
    parser.add_argument('--output', required=True, help='JSON report path; two CSVs are written beside it')
    args = parser.parse_args()
    assert args.limit > 0
    excluded = {key for path in args.exclude_audit for key, row in json.loads(Path(path).read_text())['results'].items() if row['status'] != 'request_failed'}
    result = rank(args.database, args.ratings, args.librarything_cache, args.limit, args.since, excluded, args.popular_only, args.any_subject_code)
    output = Path(args.output)
    output.write_text(json.dumps(result, ensure_ascii=False, indent=2))
    for name in ['popular', 'recent']:
        write_csv(output.with_name(output.stem + '-' + name + '.csv'), result[name])
    print(json.dumps({k: v for k, v in result.items() if k not in ['popular', 'recent']}))
    print(json.dumps({'popular': result['popular'][:5], 'recent': result['recent'][:5]}, ensure_ascii=False))
