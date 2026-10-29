#!/usr/bin/env python3
"""Verify that a developer signature can be copied to an independently rebuilt APK."""
import argparse
from importlib.metadata import version
import json
import os
from pathlib import Path
import tempfile

import apksigcopier

from native_build import digest
from release import java_environment, payloads, sdk_directory


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--signed-apk", required=True, type=Path)
    parser.add_argument("--rebuilt-apk", required=True, type=Path)
    parser.add_argument("--sdk", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    environment = java_environment(os.environ)
    sdk = sdk_directory(args.sdk, environment)
    os.environ["PATH"] = environment["PATH"]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    tempfile.tempdir = str(args.output.parent.resolve())
    first, second = payloads(args.signed_apk), payloads(args.rebuilt_apk)
    changes = [name for name in sorted(first.keys() | second.keys()) if first.get(name) != second.get(name)]
    report = {"schema": 1, "apksigcopier_version": version("apksigcopier"),
        "reference_signed_apk_sha256": digest(args.signed_apk), "rebuilt_unsigned_apk_sha256": digest(args.rebuilt_apk),
        "different_payload_entries": changes, "signature_copy_verification": "failed"}
    try:
        apksigcopier.do_compare(str(args.signed_apk), str(args.rebuilt_apk), unsigned=True,
            verify_cmd=(str(sdk / "build-tools/36.0.0/apksigner"), "verify"))
        report["signature_copy_verification"] = "passed"
    finally:
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    print("APK signature-copy reproducibility verification passed.")


if __name__ == "__main__":
    main()
