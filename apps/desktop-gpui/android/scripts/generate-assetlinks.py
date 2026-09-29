#!/usr/bin/env python3
"""Generate the public credential association from production certificate hashes.

Use the certificate that signs the installed APK (Play App Signing certificate,
not the upload certificate, when distributed by Play). No private keys are read.
"""
import argparse
import json
from pathlib import Path
import re


def association(fingerprints):
    normalized = []
    for value in fingerprints:
        value = value.replace(":", "").strip().upper()
        if not re.fullmatch(r"[0-9A-F]{64}", value):
            raise ValueError("Each certificate fingerprint must contain exactly 32 SHA-256 bytes")
        fingerprint = ":".join(value[i:i+2] for i in range(0, 64, 2))
        if fingerprint not in normalized:
            normalized.append(fingerprint)
    if not normalized:
        raise ValueError("At least one production signing certificate is required")
    relation = ["delegate_permission/common.get_login_creds"]
    return [
        {"relation": relation, "target": {"namespace": "web", "site": "https://app.bokheim.se"}},
        {"relation": relation, "target": {"namespace": "android_app", "package_name": "se.bokheim.reader.gpui", "sha256_cert_fingerprints": normalized}},
    ]


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sha256", action="append", required=True, help="Production signing certificate SHA-256; repeat for multiple certificates")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        content = association(args.sha256)
    except ValueError as error:
        parser.error(str(error))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(content, indent=2) + "\n")
