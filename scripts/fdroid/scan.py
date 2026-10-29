#!/usr/bin/env python3
"""Run fdroidserver's source/APK scanners and retain their findings as JSON."""
import argparse
from importlib.metadata import version
import json
import os
from pathlib import Path
from types import SimpleNamespace

from fdroidserver import common, metadata, scanner


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path)
    parser.add_argument("--apk", type=Path)
    parser.add_argument("--sdk", type=Path, default=os.environ.get("ANDROID_HOME"))
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--refresh", action="store_true")
    args = parser.parse_args()
    if not args.source and not args.apk:
        parser.error("Provide --source and/or --apk")
    common.options = SimpleNamespace(json=True, verbose=False, refresh_scanner=args.refresh)
    common.config = {}
    common.fill_config_defaults(common.config)
    if args.sdk:
        common.config["sdk_path"] = str(args.sdk)
    report = {"fdroidserver_version": version("fdroidserver")}
    problems = 0
    if args.source:
        messages = scanner.MessageStore()
        build = metadata.Build()
        build.gradle = ["yes"]
        build.gradleprops = ["bokheim.fdroid=true"]
        count = scanner.scan_source(str(args.source.resolve()), build, messages)
        report["source"] = {"fatal_count": count, "errors": messages.errors,
            "warnings": messages.warnings, "infos": messages.infos}
        problems += count
    if args.apk:
        count = scanner.scan_binary(str(args.apk.resolve()))
        report["apk"] = {"fatal_count": count}
        problems += count
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    raise SystemExit(1 if problems else 0)


if __name__ == "__main__":
    main()
