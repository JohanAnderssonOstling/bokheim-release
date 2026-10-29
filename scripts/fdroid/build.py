#!/usr/bin/env python3
"""Build the unsigned F-Droid APK using only prefetched inputs."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

from release import ROOT, gradle_command, java_environment, sdk_directory
from normalize import normalize
from canonical import CANONICAL_ROOT, command as canonical_command


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gradle")
    parser.add_argument("--sdk", type=Path)
    parser.add_argument("--pdfium-source-work", type=Path, required=True)
    parser.add_argument("--speech-source-work", type=Path, required=True)
    parser.add_argument("--jobs", type=int, default=1)
    parser.add_argument("--native-jobs", type=int, default=1)
    args = parser.parse_args()
    if min(args.jobs, args.native_jobs) < 1:
        parser.error("Compilation job counts must be positive")
    if not (ROOT / "FDROID-PREFETCHED.json").is_file():
        parser.error("Run scripts/fdroid/prefetch.py in this isolated checkout before building")
    environment = java_environment(os.environ)
    sdk = sdk_directory(args.sdk, environment)
    if ROOT != CANONICAL_ROOT:
        arguments = [sys.executable, str(CANONICAL_ROOT / "scripts/fdroid/build.py"),
            "--gradle", gradle_command(args.gradle), "--sdk", str(sdk),
            "--pdfium-source-work", str(args.pdfium_source_work.resolve()),
            "--speech-source-work", str(args.speech_source_work.resolve()),
            "--jobs", str(args.jobs), "--native-jobs", str(args.native_jobs)]
        subprocess.run(canonical_command(ROOT, arguments), env=environment, check=True)
        return
    pins = json.loads((ROOT / "scripts/fdroid/toolchains.json").read_text())
    temporary = ROOT.parent / ".fdroid-tmp"
    temporary.mkdir(exist_ok=True)
    environment.update(ANDROID_HOME=str(sdk), RUSTUP_TOOLCHAIN=pins["rust"],
        SOURCE_DATE_EPOCH=str(pins["source_date_epoch"]), CARGO_INCREMENTAL="0", RUSTC_WRAPPER="",
        TMPDIR=str(temporary), SQLITE_TMPDIR=str(temporary))
    command = [gradle_command(args.gradle), "-p", str(ROOT / "apps/desktop-gpui/android"),
        "--no-daemon", "--max-workers=1", "--no-build-cache", "--offline",
        f"-Dorg.gradle.jvmargs=-Xmx768m -Djava.io.tmpdir={temporary}",
        "-Pbokheim.fdroid=true", f"-Pbokheim.rustJobs={args.jobs}", f"-Pbokheim.nativeJobs={args.native_jobs}",
        f"-Prust.ndkHome={sdk}/ndk/{pins['ndk']}",
        f"-Pbokheim.pdfiumSourceWork={args.pdfium_source_work.resolve()}",
        f"-Pbokheim.speechSourceWork={args.speech_source_work.resolve()}", "assembleRelease"]
    subprocess.run(command, cwd=ROOT / "apps/desktop-gpui/android", env=environment, check=True)
    normalize(ROOT / "apps/desktop-gpui/android/build/outputs/apk/release/BokheimGpui-release-unsigned.apk")


if __name__ == "__main__":
    main()
