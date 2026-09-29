#!/usr/bin/env python3
"""Resumable projection of indexed Wikidata JSON into compressed JSONL chunks.

Extraction only: output is staged evidence, not a live metadata-service index.
Dependencies: indexed_bzip2, orjson, and the zstd command. No raw dump is stored.
"""
from __future__ import annotations

import argparse
import collections
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

import indexed_bzip2
import orjson

VERSION = 2
# Preserve complete selected statements, including rank, qualifiers and references.
PROPERTIES = frozenset('''
P31 P279 P1476 P1680 P212 P957 P50 P2093 P629 P577 P407 P123 P8360
P648 P569 P570 P106 P6886 P166 P69 P108 P737 P135 P19 P27 P856
P101 P800 P184 P185 P18 P214 P213 P244 P2963 P2969 P8383 P7400
P12430 P496 P5587
P136 P921 P179 P1545 P478 P393 P655 P98 P110 P1104 P291 P437
P155 P156 P433 P953 P996 P724
'''.split())
# Classifiers can be either external identifiers or strings. Preserve both
# generically, including properties added after this extractor was written.
EXCLUDED = frozenset()
PRESERVED_DATATYPES = frozenset({'external-id', 'string'})
FIELDS = ('id', 'type', 'lastrevid', 'modified', 'labels', 'aliases', 'descriptions')


def fingerprint(path):
    stat = path.stat()
    digest = hashlib.sha256()
    with path.open('rb') as f:
        digest.update(f.read(1024 * 1024))
        if stat.st_size > 1024 * 1024:
            f.seek(stat.st_size - 1024 * 1024)
            digest.update(f.read())
    return dict(canonical_path=str(path.resolve()), size=stat.st_size,
                mtime_ns=stat.st_mtime_ns, edge_sha256=digest.hexdigest())


def sync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def atomic_json(path, value):
    temporary = path.with_name(path.name + '.part')
    with temporary.open('wb') as f:
        f.write(orjson.dumps(value, option=orjson.OPT_SORT_KEYS))
        f.write(b'\n')
        f.flush()
        os.fsync(f.fileno())
    os.replace(temporary, path)
    sync_directory(path.parent)


def file_hash(path):
    digest = hashlib.sha256()
    with path.open('rb') as f:
        for data in iter(lambda: f.read(1024 * 1024), b''):
            digest.update(data)
    return digest.hexdigest()


def project(record):
    if not isinstance(record, dict) or not isinstance(record.get('id'), str):
        raise ValueError('entity has no string identifier')
    kept = {field: record[field] for field in FIELDS if field in record}
    claims = {}
    for prop, statements in record.get('claims', {}).items():
        if prop in EXCLUDED:
            continue
        if prop in PROPERTIES:
            claims[prop] = statements
        else:
            external = [s for s in statements if s.get('mainsnak', {}).get('datatype') in PRESERVED_DATATYPES]
            if external:
                claims[prop] = external
    if claims:
        kept['claims'] = claims
    # Only Wikipedia sitelinks were requested; Wikisource etc. can be added later.
    links = {site: value for site, value in record.get('sitelinks', {}).items()
             if site.endswith('wiki') and site not in ('commonswiki', 'specieswiki', 'wikidatawiki', 'mediawikiwiki', 'metawiki')}
    if links:
        kept['sitelinks'] = links
    return kept


def load_index(source, path):
    index = orjson.loads(path.read_bytes())
    if index.get('version') != 1 or index.get('source') != fingerprint(source):
        raise ValueError('seek index does not match source dump')
    return index


def counts_for(record):
    claims = record.get('claims', {})
    instances = [s.get('mainsnak', {}).get('datavalue', {}).get('value') for s in claims.get('P31', [])]
    return {'records': 1, 'with_isbn': int('P212' in claims or 'P957' in claims),
            'with_lcc': int('P8360' in claims), 'with_ddc': int('P8359' in claims or 'P1036' in claims),
            'with_topic_lcc': int('P1149' in claims), 'with_bisac': int('P12164' in claims), 'with_author_links': int('P50' in claims),
            'humans': int(any(isinstance(v, dict) and v.get('id') == 'Q5' for v in instances))}


def write_chunk(decoded, directory, start, end, expected_offset=None):
    destination = directory / f'entities-{start:012d}-{end:012d}.jsonl.zst'
    temporary = destination.with_name(destination.name + '.part')
    started = time.monotonic()
    counts = collections.Counter()
    raw_bytes = output_bytes = 0
    first = last = None
    classifier_properties = []
    with temporary.open('wb') as output:
        compressor = subprocess.Popen(['zstd', '-q', '-3', '-T1', '-c'], stdin=subprocess.PIPE, stdout=output)
        try:
            buffer = bytearray()
            while counts['records'] < end - start:
                line = decoded.readline()
                if not line:
                    raise ValueError(f'unexpected EOF at record {start + counts["records"]}')
                raw_bytes += len(line)
                line = line.strip().rstrip(b',')
                if not line or line in (b'[', b']'):
                    continue
                record = orjson.loads(line)
                kept = project(record)
                if kept.get('type') == 'property':
                    types = {s.get('mainsnak',{}).get('datavalue',{}).get('value',{}).get('id')
                             for s in kept.get('claims',{}).get('P31',[])
                             if isinstance(s.get('mainsnak',{}).get('datavalue',{}).get('value'),dict)}
                    if types & {'Q95388829','Q95388859'}:
                        classifier_properties.append(kept)
                counts.update(counts_for(kept))
                if counts['records'] % 20000 == 0:
                    heartbeat = dict(chunk_start=start, scanned_record=start + counts['records'],
                                     elapsed_seconds=round(time.monotonic() - started, 2), updated_at_unix=int(time.time()))
                    atomic_json(directory / 'heartbeat.json', heartbeat)
                    print(json.dumps(heartbeat, separators=(',', ':')), flush=True)
                first = first or kept['id']
                last = kept['id']
                data = orjson.dumps(kept) + b'\n'
                output_bytes += len(data)
                buffer.extend(data)
                if len(buffer) >= 1024 * 1024:
                    compressor.stdin.write(buffer)
                    buffer.clear()
            if expected_offset is not None and decoded.tell() != expected_offset:
                raise ValueError('decoded record boundary differs from seek index')
            if buffer:
                compressor.stdin.write(buffer)
            compressor.stdin.close()
            if compressor.wait() != 0:
                raise RuntimeError('zstd failed')
            output.flush()
            os.fsync(output.fileno())
        except BaseException:
            compressor.kill()
            compressor.wait()
            try:
                compressor.stdin.close()
            except OSError:
                pass
            raise
    os.replace(temporary, destination)
    sync_directory(directory)
    metadata = dict(version=VERSION, start=start, end=end, first_id=first, last_id=last,
                    classifier_properties=classifier_properties,
                    counts=dict(counts), source_bytes=raw_bytes, projected_bytes=output_bytes,
                    compressed_bytes=destination.stat().st_size, sha256=file_hash(destination),
                    file=destination.name, elapsed_seconds=round(time.monotonic() - started, 3))
    # Publishing this marker commits the chunk. An orphan data file is replaced
    # on retry, while a committed chunk is verified and never appended twice.
    atomic_json(destination.with_suffix('.meta.json'), metadata)
    return metadata


def extract(source, index_path, directory, *, start=0, stop=None, chunk_records=200000,
            threads=2, max_chunks=None, min_free_bytes=100 * 1024 ** 3):
    source, index_path, directory = map(Path, (source, index_path, directory))
    directory.mkdir(parents=True, exist_ok=True)
    with (directory / '.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        index = load_index(source, index_path)
        total = index['records']
        stop = total if stop is None else stop
        if not 0 <= start < stop <= total:
            raise ValueError('invalid record range')
        if chunk_records <= 0 or threads <= 0 or (max_chunks is not None and max_chunks <= 0):
            raise ValueError('chunk size, threads, and max-chunks must be positive')
        # Every restart boundary must be seekable without rescanning the dump.
        if str(start) not in index['record_offsets'] or chunk_records % index['checkpoint_interval']:
            raise ValueError('start/chunk size must align with indexed record checkpoints')
        manifest = dict(version=VERSION, source=index['source'], total_records=total, start=start, stop=stop,
                        chunk_records=chunk_records, properties=sorted(PROPERTIES), excluded=sorted(EXCLUDED),
                        projection='selected claims and all external-ID/string claims; all entity labels, aliases, descriptions; Wikipedia sitelinks')
        manifest_path = directory / 'manifest.json'
        if manifest_path.exists():
            if orjson.loads(manifest_path.read_bytes()) != manifest:
                raise ValueError('output manifest differs from requested extraction')
        else:
            if list(directory.glob('entities-*')):
                raise ValueError('existing chunks have no manifest')
            atomic_json(manifest_path, manifest)
        progress = collections.Counter()
        cursor = start
        while cursor < stop:
            end = min(cursor + chunk_records, stop)
            path = directory / f'entities-{cursor:012d}-{end:012d}.jsonl.zst'
            marker = path.with_suffix('.meta.json')
            if not marker.exists():
                break
            meta = orjson.loads(marker.read_bytes())
            if meta['start'] != cursor or meta['end'] != end or meta['counts']['records'] != end - cursor or meta['file'] != path.name:
                raise ValueError('invalid committed chunk metadata')
            if path.stat().st_size != meta['compressed_bytes'] or file_hash(path) != meta['sha256']:
                raise ValueError(f'committed chunk is corrupt: {path}')
            progress.update(meta['counts'])
            progress['compressed_bytes'] += meta['compressed_bytes']
            cursor = end
        def report(status):
            data = dict(status=status, next_record=cursor, stop=stop, full_dump=(start == 0 and stop == total),
                        counts=dict(progress), updated_at_unix=int(time.time()))
            atomic_json(directory / 'progress.json', data)
            print(json.dumps(data, separators=(',', ':')), flush=True)
        if cursor == stop:
            report('complete')
            return
        report('running')
        try:
            with indexed_bzip2.open(str(source), parallelization=threads) as decoded:
                decoded.set_block_offsets(dict(index['block_offsets']))
                decoded.seek(index['record_offsets'][str(cursor)])
                chunks = 0
                while cursor < stop:
                    if shutil.disk_usage(directory).free < min_free_bytes:
                        raise RuntimeError('free space below extraction reserve; resume after freeing space')
                    end = min(cursor + chunk_records, stop)
                    expected_offset = index['record_offsets'].get(str(end)) if end < total else None
                    meta = write_chunk(decoded, directory, cursor, end, expected_offset)
                    progress.update(meta['counts'])
                    progress['compressed_bytes'] += meta['compressed_bytes']
                    cursor = end
                    chunks += 1
                    report('complete' if cursor == stop else 'running')
                    if max_chunks is not None and chunks >= max_chunks and cursor < stop:
                        report('paused')
                        return
        except BaseException:
            report('failed')
            raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('index', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('--start', type=int, default=0)
    parser.add_argument('--stop', type=int)
    parser.add_argument('--chunk-records', type=int, default=200000)
    parser.add_argument('--threads', type=int, default=2)
    parser.add_argument('--max-chunks', type=int)
    parser.add_argument('--min-free-gib', type=float, default=100)
    args = parser.parse_args()
    if args.min_free_gib < 0:
        parser.error('min-free-gib must not be negative')
    extract(args.source, args.index, args.output, start=args.start, stop=args.stop,
            chunk_records=args.chunk_records, threads=args.threads, max_chunks=args.max_chunks,
            min_free_bytes=int(args.min_free_gib * 1024 ** 3))


if __name__ == '__main__':
    main()
