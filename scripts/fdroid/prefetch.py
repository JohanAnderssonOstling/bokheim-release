#!/usr/bin/env python3
"""Fetch pinned build inputs without compiling application or native libraries."""
import argparse
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys

from native_build import fetch, write
from normalize import normalizer
from release import ROOT, gradle_command, java_environment, sdk_directory


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gradle")
    parser.add_argument("--sdk", type=Path)
    parser.add_argument("--pdfium-source-work", type=Path, required=True)
    parser.add_argument("--speech-source-work", type=Path, required=True)
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    pins = json.loads((ROOT / "scripts/fdroid/toolchains.json").read_text())
    environment = java_environment(os.environ)
    environment.update(ANDROID_HOME=str(sdk_directory(args.sdk, environment)), RUSTUP_TOOLCHAIN=pins["rust"],
        SOURCE_DATE_EPOCH=str(pins["source_date_epoch"]))
    environment.setdefault("CARGO_HOME", str(Path.home() / ".cargo"))
    environment.setdefault("RUSTUP_HOME", str(Path.home() / ".rustup"))
    prepare = [sys.executable, str(ROOT / "scripts/fdroid/prepare_checkout.py")]
    if args.offline:
        prepare.append("--offline")
    subprocess.run(prepare, env=environment, check=True)
    if not args.offline:
        subprocess.run(["rustup", "toolchain", "install", pins["rust"], "--profile", "minimal",
            "--component", "rust-src", "--target", "aarch64-linux-android", "--no-self-update"], env=environment, check=True)
    # Cross builds also resolve host-side build dependencies. Fetch the whole
    # locked graph so a fresh Cargo home can compile without network access.
    cargo = ["cargo", "fetch", "--locked"]
    if args.offline:
        cargo.append("--offline")
    subprocess.run(cargo, cwd=ROOT, env=environment, check=True)
    gradle = [gradle_command(args.gradle), "-p", str(ROOT / "apps/desktop-gpui/android"),
        "--no-daemon", "--max-workers=1", "-Dorg.gradle.jvmargs=-Xmx768m", "-Pbokheim.fdroid=true",
        "prefetchFdroidDependencies"]
    if args.offline:
        gradle.append("--offline")
    subprocess.run(gradle, cwd=ROOT / "apps/desktop-gpui/android", env=environment, check=True)
    # Do not generate objects or runtime libraries in the source-fetch phase.
    pdfium = runpy.run_path(str(ROOT / "shared/pdfium/build_android_source.py"))
    pdfium["prepare_source"](ROOT / ".pdfium/fdroid/aarch64-linux-android", args.pdfium_source_work.resolve(),
        offline=args.offline, fetch_only=True)
    speech = runpy.run_path(str(ROOT / "shared/reader-speech/build_android_source.py"))
    speech["prefetch_sources"](args.speech_source_work.resolve(), args.offline)
    voice = runpy.run_path(str(ROOT / "shared/reader-speech/prepare.py"))
    cache = Path(environment.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "bokheim/tts"
    fetch(voice["URL"], voice["SHA256"], cache / "vits-piper-en_GB-alba-medium.tar.bz2", args.offline)
    normalizer(args.offline)
    write(ROOT / "FDROID-PREFETCHED.json", json.dumps({"schema": 1, "toolchains": pins,
        "application_binaries_built": False}, indent=2) + "\n")
    print("F-Droid source, toolchain and dependency inputs are ready for an offline build.")


if __name__ == "__main__":
    main()
