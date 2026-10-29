#!/usr/bin/env python3
"""Build and verify deterministic HTTP compression sidecars."""

import argparse
import gzip
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

SUFFIXES = {".wasm", ".js", ".mjs", ".css", ".html", ".svg", ".json", ".sqlite3", ".ttf"}


def digest(stream):
    result = hashlib.sha256()
    while chunk := stream.read(1024 * 1024):
        result.update(chunk)
    return result.digest()


def compress_asset(source):
    with source.open("rb") as stream:
        expected = digest(stream)
    sizes = {"raw_bytes": source.stat().st_size}
    for encoding in ("br", "gz"):
        destination = source.with_name(source.name + "." + encoding)
        with tempfile.NamedTemporaryFile(dir=source.parent, prefix=destination.name + ".", delete=False) as reserved:
            staging = Path(reserved.name)
        try:
            with staging.open("wb") as output:
                if encoding == "br":
                    subprocess.run(["brotli", "-q", "11", "-c", str(source)], stdout=output, check=True)
                else:
                    with gzip.GzipFile(filename="", mode="wb", compresslevel=9, fileobj=output, mtime=0) as compressed:
                        with source.open("rb") as stream:
                            shutil.copyfileobj(stream, compressed)
            if encoding == "br":
                with subprocess.Popen(["brotli", "-d", "-c", str(staging)], stdout=subprocess.PIPE) as decoder:
                    actual = digest(decoder.stdout)
                    if decoder.wait() != 0:
                        raise RuntimeError(f"Brotli validation failed: {source}")
            else:
                with gzip.open(staging, "rb") as stream:
                    actual = digest(stream)
            if actual != expected:
                raise RuntimeError(f"Compression changed asset contents: {source}")
            sizes[encoding + "_bytes"] = staging.stat().st_size
            # Caddy must be able to read these after deployment. Temporary files
            # start private, so retain the original asset's access permissions.
            shutil.copymode(source, staging)
            staging.replace(destination)
        finally:
            staging.unlink(missing_ok=True)
    return sizes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--top-level", action="store_true")
    args = parser.parse_args()
    if shutil.which("brotli") is None:
        parser.error("install the brotli command to compress web assets")
    files = args.directory.glob("*") if args.top_level else args.directory.rglob("*")
    sources = sorted(path for path in files if path.is_file() and path.suffix in SUFFIXES)
    totals = {"raw_bytes": 0, "br_bytes": 0, "gz_bytes": 0}
    for source in sources:
        sizes = compress_asset(source)
        for key, size in sizes.items():
            totals[key] += size
    print(json.dumps({"assets": len(sources), **totals}), flush=True)


if __name__ == "__main__":
    main()
