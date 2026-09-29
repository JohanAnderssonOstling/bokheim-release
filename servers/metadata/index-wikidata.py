#!/usr/bin/env python3
"""Build and consume a seek index for a line-oriented Wikidata bzip2 dump."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sys
from pathlib import Path

import indexed_bzip2


INDEX_VERSION = 1


def configured_threads(command_line_threads: int) -> int:
    """Allow the resumable importer to tune decompression per host."""
    configured = os.environ.get("BOKHEIM_WIKIDATA_THREADS")
    if configured is None:
        return command_line_threads
    threads = int(configured)
    if threads <= 0:
        raise ValueError("BOKHEIM_WIKIDATA_THREADS must be positive")
    return threads


def fingerprint(path: Path) -> dict[str, object]:
    stat = path.stat()
    sample_size = 1024 * 1024
    digest = hashlib.sha256()
    with path.open("rb") as source:
        digest.update(source.read(sample_size))
        if stat.st_size > sample_size:
            source.seek(max(0, stat.st_size - sample_size))
            digest.update(source.read(sample_size))
    return {
        "canonical_path": str(path.resolve()),
        "size": stat.st_size,
        "mtime_ns": stat.st_mtime_ns,
        "edge_sha256": digest.hexdigest(),
    }


def is_entity_line(line: bytes) -> bool:
    value = line.strip().lstrip(b"[").rstrip(b"]").rstrip(b",").strip()
    return bool(value)


def write_json_atomic(path: Path, value: object) -> None:
    temporary = path.with_name(path.name + ".part")
    with temporary.open("w", encoding="utf-8") as output:
        json.dump(value, output, separators=(",", ":"), sort_keys=True)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, path)
    directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def build(source: Path, index: Path, interval: int, threads: int) -> None:
    if not source.is_absolute() or not index.is_absolute():
        raise ValueError("source and index paths must be absolute")
    source_fingerprint = fingerprint(source)
    record_offsets: dict[str, int] = {"0": 0}
    records = 0
    with indexed_bzip2.open(str(source), parallelization=threads) as decoded:
        while line := decoded.readline():
            if not is_entity_line(line):
                continue
            records += 1
            if records % interval == 0:
                record_offsets[str(records)] = decoded.tell()
            if records % 1_000_000 == 0:
                print(f"indexed {records} Wikidata entities", flush=True)
        record_offsets[str(records)] = decoded.tell()
        block_offsets = [[compressed, uncompressed] for compressed, uncompressed in sorted(decoded.block_offsets().items())]
        decoded_size = decoded.tell()
    write_json_atomic(
        index,
        {
            "version": INDEX_VERSION,
            "source": source_fingerprint,
            "checkpoint_interval": interval,
            "records": records,
            "decoded_size": decoded_size,
            "record_offsets": record_offsets,
            "block_offsets": block_offsets,
        },
    )
    print(f"created Wikidata seek index {index} ({records} entities, {len(block_offsets)} bzip2 blocks)", flush=True)


def load_validated(source: Path, index: Path) -> dict[str, object]:
    with index.open("r", encoding="utf-8") as index_file:
        value = json.load(index_file)
    if value.get("version") != INDEX_VERSION:
        raise ValueError("unsupported Wikidata seek-index version")
    if value.get("source") != fingerprint(source):
        raise ValueError("Wikidata seek index does not match the dump")
    return value


def stream(source: Path, index: Path, record: int, threads: int) -> None:
    print(
        f"streaming Wikidata from indexed record {record} "
        f"with {threads} decompression threads",
        file=sys.stderr,
        flush=True,
    )
    value = load_validated(source, index)
    record_offsets = value.get("record_offsets")
    if not isinstance(record_offsets, dict) or str(record) not in record_offsets:
        raise ValueError(f"seek index has no byte offset for record {record}")
    block_pairs = value.get("block_offsets")
    if not isinstance(block_pairs, list):
        raise ValueError("seek index has no bzip2 block map")
    block_offsets = {int(pair[0]): int(pair[1]) for pair in block_pairs}
    decoded_offset = int(record_offsets[str(record)])
    with indexed_bzip2.open(str(source), parallelization=threads) as decoded:
        decoded.set_block_offsets(block_offsets)
        decoded.seek(decoded_offset)
        shutil.copyfileobj(decoded, sys.stdout.buffer, length=1024 * 1024)


def verify(source: Path, index: Path, record: int, threads: int) -> None:
    value = load_validated(source, index)
    record_offsets = value.get("record_offsets")
    block_pairs = value.get("block_offsets")
    if not isinstance(record_offsets, dict) or str(record) not in record_offsets:
        raise ValueError(f"seek index has no byte offset for record {record}")
    if not isinstance(block_pairs, list):
        raise ValueError("seek index has no bzip2 block map")
    with indexed_bzip2.open(str(source), parallelization=threads) as decoded:
        decoded.set_block_offsets({int(pair[0]): int(pair[1]) for pair in block_pairs})
        decoded.seek(int(record_offsets[str(record)]))
        line = decoded.readline()
        if not line or not is_entity_line(line):
            raise ValueError(f"seek index did not reach an entity after record {record}")
    print(f"verified Wikidata seek index at record {record}", flush=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    build_parser = subparsers.add_parser("build")
    build_parser.add_argument("source", type=Path)
    build_parser.add_argument("index", type=Path)
    build_parser.add_argument("--interval", type=int, default=20_000)
    build_parser.add_argument("--threads", type=int, default=1)
    stream_parser = subparsers.add_parser("stream")
    stream_parser.add_argument("source", type=Path)
    stream_parser.add_argument("index", type=Path)
    stream_parser.add_argument("record", type=int)
    stream_parser.add_argument("--threads", type=int, default=2)
    verify_parser = subparsers.add_parser("verify")
    verify_parser.add_argument("source", type=Path)
    verify_parser.add_argument("index", type=Path)
    verify_parser.add_argument("record", type=int)
    verify_parser.add_argument("--threads", type=int, default=2)
    arguments = parser.parse_args()
    if arguments.command == "build":
        if arguments.interval <= 0 or arguments.threads <= 0:
            parser.error("interval and threads must be positive")
        build(arguments.source, arguments.index, arguments.interval, arguments.threads)
    elif arguments.command == "stream":
        if arguments.record < 0 or arguments.threads <= 0:
            parser.error("record must be non-negative and threads must be positive")
        stream(arguments.source, arguments.index, arguments.record, configured_threads(arguments.threads))
    else:
        if arguments.record < 0 or arguments.threads <= 0:
            parser.error("record must be non-negative and threads must be positive")
        verify(arguments.source, arguments.index, arguments.record, configured_threads(arguments.threads))


if __name__ == "__main__":
    main()
