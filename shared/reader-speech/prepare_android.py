#!/usr/bin/env python3
"""Prepare verified ARM64 speech libraries and offline voice assets for Android."""
import argparse
import hashlib
import os
from pathlib import Path
import runpy
import shutil
import tarfile
import tempfile
import urllib.request

from prepare import ASSET_SHA256 as VOICE_SHA256, prepare

VERSION = "1.13.8"
SHA256 = "2ff63469a71cb6009aa2e3ed5f4a670f8abdcbe4bb9ffd23776afc792a6b4f44"
URL = f"https://github.com/k2-fsa/sherpa-onnx/releases/download/v{VERSION}/sherpa-onnx-v{VERSION}-android.tar.bz2"
REQUIRED = ("libsherpa-onnx-c-api.so", "libonnxruntime.so")


def prepare_android(destination, offline=False, from_source=False, onnxruntime_aar=None, ndk=None, source_work=None, jobs=2):
    destination = Path(destination)
    libraries = destination / "jniLibs/arm64-v8a"
    marker = libraries / ".archive-sha256"
    if from_source:
        if onnxruntime_aar is None or ndk is None:
            raise RuntimeError("Source speech requires --onnxruntime-aar and --ndk")
        prepare_source = runpy.run_path(str(Path(__file__).with_name("build_android_source.py")))["prepare_source"]
        prepare_source(libraries, onnxruntime_aar, ndk, source_work, jobs, offline)
    elif not (marker.is_file() and marker.read_text().strip() == SHA256 and all((libraries / name).is_file() for name in REQUIRED)):
        destination.mkdir(parents=True, exist_ok=True)
        archive = destination / f"sherpa-onnx-v{VERSION}-android.tar.bz2"
        if not archive.is_file():
            if offline:
                raise RuntimeError(f"Android speech runtime is not cached: {archive}")
            partial = archive.with_suffix(".part")
            with urllib.request.urlopen(URL, timeout=120) as response, partial.open("wb") as output:
                shutil.copyfileobj(response, output)
            with partial.open("rb") as source:
                if hashlib.file_digest(source, "sha256").hexdigest() != SHA256:
                    partial.unlink()
                    raise RuntimeError("Android speech runtime checksum mismatch")
            partial.replace(archive)
        with archive.open("rb") as source:
            if hashlib.file_digest(source, "sha256").hexdigest() != SHA256:
                raise RuntimeError("Android speech runtime checksum mismatch")
        with tempfile.TemporaryDirectory(dir=destination) as staging:
            with tarfile.open(archive) as compressed:
                compressed.extractall(staging, filter="data")
            candidates = list(Path(staging).glob("**/jniLibs/arm64-v8a"))
            if len(candidates) != 1 or not all((candidates[0] / name).is_file() for name in REQUIRED):
                raise RuntimeError("Android speech archive is missing ARM64 runtime libraries")
            libraries.mkdir(parents=True, exist_ok=True)
            for library in (candidates[0] / name for name in REQUIRED):
                shutil.copy2(library, libraries / library.name)
            marker.write_text(SHA256 + "\n")
    voice_archive = (Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) /
        "bokheim/tts/vits-piper-en_GB-alba-medium.tar.bz2") if from_source else None
    prepare(destination / "assets/tts/alba", offline=offline, archive=voice_archive)
    (destination / "assets/tts/alba/voice.sha256").write_text(VOICE_SHA256 + "\n")
    if from_source:
        license_directory = Path(__file__).with_name("licenses")
        for name in ("alba-model-card.txt", "alba-provenance.json", "CC-BY-4.0.txt"):
            shutil.copy2(license_directory / name, destination / "assets/tts/alba" / name)
    # Remove the previously bundled voice from reused build directories.
    george = destination / "assets/tts/george"
    if george.exists():
        shutil.rmtree(george)
    return libraries


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--from-source", action="store_true")
    parser.add_argument("--onnxruntime-aar", type=Path)
    parser.add_argument("--ndk", type=Path)
    parser.add_argument("--source-work", type=Path)
    parser.add_argument("--jobs", type=int, default=2)
    args = parser.parse_args()
    if args.jobs < 1:
        parser.error("--jobs must be positive")
    print(prepare_android(args.destination, args.offline, args.from_source, args.onnxruntime_aar,
        args.ndk, args.source_work, args.jobs))
