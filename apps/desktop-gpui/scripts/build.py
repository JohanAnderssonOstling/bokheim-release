#!/usr/bin/env python3
"""Build an optimized desktop app and stage its PDFium runtime beside it."""
import argparse
import os
from pathlib import Path
import runpy
import subprocess

ROOT = Path(__file__).resolve().parents[3]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target")
    parser.add_argument("--flatpak", action="store_true")
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    requested_target = args.target or os.environ.get("CARGO_BUILD_TARGET")
    target = requested_target or next(line.removeprefix("host: ") for line in
        subprocess.check_output(["rustc", "-vV"], text=True).splitlines() if line.startswith("host: "))
    prepare = runpy.run_path(str(ROOT / "shared/pdfium/prepare.py"))["prepare"]
    prepared = prepare(target, offline=args.offline)
    command = ["cargo", "build", "--release", "-p", "desktop-gpui", "--bin", "desktop-gpui",
               "--no-default-features", "--features", "flatpak" if args.flatpak else "desktop"]
    if requested_target:
        command += ["--target", target]
    if args.offline:
        command += ["--offline"]
    subprocess.run(command, cwd=ROOT, check=True, env={**os.environ, "PDFIUM_PREPARED_DIR": str(prepared)})
    output = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    if not output.is_absolute():
        output = ROOT / output
    if requested_target:
        output /= target
    if target != "x86_64-pc-windows-msvc":
        prepare(target, destination=output / "release/pdfium", offline=True)


if __name__ == "__main__":
    main()
