#!/usr/bin/env python3
"""Fetch verified PDFium artifacts before compilation; no third-party Python packages."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys
import tarfile
import tempfile
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = Path(__file__).with_name("artifacts.toml")


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def fetch(spec, cache, override, offline):
    path = Path(override) if override else cache / (spec["sha256"] + ".tgz")
    if not path.exists():
        if offline or override:
            raise RuntimeError(f"PDFium archive unavailable: {path}")
        cache.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(dir=cache, delete=False) as output:
            temporary = Path(output.name)
        try:
            urllib.request.urlretrieve(spec["url"], temporary)
            if digest(temporary) != spec["sha256"]:
                raise RuntimeError(f"PDFium checksum mismatch: {spec['url']}")
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)
    if digest(path) != spec["sha256"]:
        raise RuntimeError(f"PDFium checksum mismatch: {path}")
    if override:
        cache.mkdir(parents=True, exist_ok=True)
        cached = cache / (spec["sha256"] + ".tgz")
        if not cached.exists():
            with tempfile.NamedTemporaryFile(dir=cache, delete=False) as output:
                temporary = Path(output.name)
            try:
                shutil.copyfile(path, temporary)
                temporary.replace(cached)
            finally:
                temporary.unlink(missing_ok=True)
    return path


def prepare(target, *, cache=None, destination=None, archive=None, offline=False):
    manifest = tomllib.loads(MANIFEST.read_text())
    if manifest["schema"] != 1 or target not in manifest["targets"]:
        raise RuntimeError(f"Unsupported PDFium target: {target}")
    spec = manifest["targets"][target]
    cache = Path(cache or os.environ.get("PDFIUM_CACHE_DIR", ROOT / ".pdfium-cache"))
    destination = Path(destination or ROOT / ".pdfium" / target)
    source_path = fetch(spec, cache, archive, offline)
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent) as staging:
        staging = Path(staging)
        with tarfile.open(source_path, "r:gz") as source:
            for member_name, output_name in spec["files"].items():
                member = source.getmember(member_name)
                if not member.isfile() or Path(output_name).name != output_name:
                    raise RuntimeError(f"Invalid PDFium artifact member: {member_name}")
                with source.extractfile(member) as data:
                    (staging / output_name).write_bytes(data.read())
        if "library_sha256" in spec and digest(staging / "pdfium.lib") != spec["library_sha256"]:
            raise RuntimeError("PDFium static library checksum mismatch")
        if "notices_file" in spec:
            notices = MANIFEST.parent / spec["notices_file"]
            if digest(notices) != spec["notices_sha256"]:
                raise RuntimeError("PDFium notices checksum mismatch")
            shutil.copyfile(notices, staging / "LICENSE")
            notices_origin = spec["notices_file"]
        else:
            notices_spec = manifest["targets"][spec["notices_target"]] if "notices_target" in spec else spec
            notices_path = fetch(notices_spec, cache, None, offline) if "notices_target" in spec else source_path
            with tarfile.open(notices_path, "r:gz") as source:
                notices = []
                for member in source.getmembers():
                    name = member.name
                    if member.isfile() and (name == "LICENSE" or name.startswith("licenses/")):
                        raw = source.extractfile(member).read()
                        try:
                            notice = raw.decode("utf-8")
                        except UnicodeDecodeError:
                            notice = raw.decode("latin-1")
                        notices.append(b"===== " + name.encode() + b" =====\n" + notice.encode("utf-8"))
                if not notices:
                    raise RuntimeError(f"PDFium archive has no license notices: {source_path}")
                (staging / "LICENSE").write_bytes(b"\n\n".join(notices))
            notices_origin = notices_spec["url"]
        (staging / "VERSION").write_text(f"PDFium {spec['version']}\n{spec['url']}\nSHA256 {spec['sha256']}\nNotices: {notices_origin}\n")
        receipt = {"manifest_sha256": digest(MANIFEST), "target": target, "archive_sha256": spec["sha256"], "files": {
            item.name: digest(item) for item in staging.iterdir()
        }}
        (staging / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
        destination.mkdir(parents=True, exist_ok=True)
        # Preserve mtimes of unchanged inputs so repeated preparation does not
        # invalidate Cargo builds. Unique temporary files tolerate parallel builds.
        for item in sorted(staging.iterdir(), key=lambda path: path.name == "receipt.json"):
            output = destination / item.name
            if not output.exists() or digest(output) != digest(item):
                item.replace(output)
    return destination.resolve()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--cache", type=Path)
    parser.add_argument("--dest", type=Path)
    parser.add_argument("--archive", type=Path, help="Use a local archive (still checksum verified)")
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    try:
        print(prepare(args.target, cache=args.cache, destination=args.dest, archive=args.archive, offline=args.offline))
    except (OSError, RuntimeError, tarfile.TarError, KeyError) as error:
        sys.exit(str(error))


if __name__ == "__main__":
    main()
