#!/usr/bin/env python3
"""Prepare an isolated F-Droid checkout with pinned public sibling sources."""
import argparse
import json
from pathlib import Path
import sys
import subprocess

from native_build import apply_source_patch, digest, extract, fetch, write

UNUSED_INPUTS = (
    "shared/subject-projection/data/reports/20261015-literature-coverage-fix/publication-reviewed/releases/248/catalog.sqlite3.gz",
    "shared/subject-projection/data/reports/20261015-literature-coverage-fix/publication/releases/248/catalog.sqlite3.gz",
    "shared/book-model/tests/fixtures/annotation_pdf.bin",
    "shared/book-model/tests/fixtures/annotation_epub.bin",
    "apps/desktop-gpui/kobo/nickelmenu-install/KoboRoot.tgz",
    "apps/kobo-installer/package/Bokheim-Kobo-Libra2-armv7.tar.gz",
    "shared/subject-projection/data/reports/20261015-literature-coverage-fix/publication-input/books",
    "shared/subject-projection/data/reports/20261015-literature-coverage-fix/publication-reviewed/books",
    "shared/subject-projection/data/reports/20261015-literature-coverage-fix/publication/books",
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--cache", type=Path)
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    root = args.root.resolve()
    pins_path = root / "scripts/fdroid/sources.json"
    pins = json.loads(pins_path.read_text())
    cache = args.cache or root.parent / ".fdroid-inputs"
    for item in pins["sources"]:
        destination = root.parent / item["name"]
        marker = destination / "SOURCE-FDROID.json"
        if destination.exists():
            previous = json.loads(marker.read_text()) if marker.is_file() else {}
            basis = {key: value for key, value in item.items() if key != "patches"}
            if {key: value for key, value in previous.items() if key != "patches"} != basis:
                raise RuntimeError(f"Refusing to overwrite existing developer sources: {destination}. Use an isolated checkout.")
        else:
            archive = fetch(item["url"], item["sha256"], cache / (item["name"] + "-" + item["commit"] + ".tar.gz"), args.offline)
            extract(archive, destination)
        for change in item.get("patches", []):
            patch = root / change["path"]
            if digest(patch) != change["sha256"]:
                raise RuntimeError(f"Companion source patch checksum mismatch: {patch}")
            apply_source_patch(patch, destination)
            for name, sha256 in change["files"].items():
                if digest(destination / name) != sha256:
                    raise RuntimeError(f"Companion source differs from the required patch: {name}")
        write(marker, json.dumps(item, indent=2, sort_keys=True) + "\n")
    android = root / "apps/desktop-gpui/android"
    # This replaces the direct-build repository configuration in this isolated
    # checkout. The F-Droid version uses source Rustls and only trusted Maven
    # repositories; no unused custom Maven repository is left for the scanner.
    write(android / "settings.gradle.kts", (android / "settings.fdroid.gradle.kts").read_bytes())
    removed = []
    for relative in UNUSED_INPUTS:
        # Historical report databases, test fixtures and Kobo packages are not
        # Android build inputs. Remove them only in this prepared checkout.
        path = root / relative
        if path.is_symlink() or path.is_file():
            path.unlink()
            removed.append(relative)
    write(root / "FDROID-PREPARED.json", json.dumps({"sources_sha256": digest(pins_path),
        "removed_non_android_inputs": removed}, indent=2) + "\n")
    print(f"Prepared F-Droid source checkout: {root}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError) as error:
        sys.exit(str(error))
