#!/usr/bin/env python3
"""Prepare verified, bundled Alba and George FP32 speech assets."""
import argparse
import hashlib
import os
from pathlib import Path
import shutil
import tarfile
import tempfile
import urllib.request

URL = "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-piper-en_GB-alba-medium.tar.bz2"
SHA256 = "fcd45962906933eec4431d3688f7d74aaac8713c87c6717f91fd3b23463aa1a1"
# Identify the prepared contents separately from the unchanged upstream archive.
ASSET_SHA256 = hashlib.sha256(f"{SHA256}:english-only-v1".encode("ascii")).hexdigest()
GEORGE_URL = "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-multi-lang-v1_0.tar.bz2"
GEORGE_SHA256 = "60703707bc05ccd48d60e1eb723f38b07d6e474795a0b6a588d7220400e0f7a4"
GEORGE_ASSET_SHA256 = hashlib.sha256(f"{GEORGE_SHA256}:george-english-fp32-v1".encode("ascii")).hexdigest()
GEORGE_REQUIRED = ("model.onnx", "voices.bin", "tokens.txt", "lexicon-gb-en.txt",
                   "espeak-ng-data/en_dict", "espeak-ng-data/lang/gmw/en", "LICENSE", "VOICE-LICENSE.txt")


def is_cached(directory):
    marker = directory / ".archive-sha256"
    return marker.is_file() and marker.read_text().strip() == SHA256 and all(
        (directory / name).exists()
        for name in ("en_GB-alba-medium.onnx", "tokens.txt", "espeak-ng-data/en_dict",
                     "espeak-ng-data/lang/gmw/en", "VOICE-LICENSE.txt")
    )


def is_prepared(directory):
    marker = directory / ".assets-sha256"
    return is_cached(directory) and marker.is_file() and marker.read_text().strip() == ASSET_SHA256


def retain_english(directory, asset_sha256=ASSET_SHA256):
    """Keep English pronunciation data and the shared phoneme/voice resources."""
    data = directory / "espeak-ng-data"
    for dictionary in data.glob("*_dict"):
        if dictionary.name != "en_dict":
            dictionary.unlink()
    for language in (data / "lang").rglob("*"):
        if language.is_file() and not (language.name == "en" or language.name.startswith("en-")):
            language.unlink()
    (directory / ".assets-sha256").write_text(asset_sha256 + "\n")


def prepare(destination=None, offline=False, archive=None):
    data = Path(os.environ.get("XDG_DATA_HOME", os.environ.get("LOCALAPPDATA", Path.home() / ".local/share")))
    installed = data / "bokheim/tts/alba"
    destination = Path(destination) if destination else installed
    if is_prepared(destination):
        return destination
    if is_cached(destination):
        retain_english(destination)
        return destination
    if archive is None and is_cached(installed):
        shutil.copytree(installed, destination, dirs_exist_ok=True)
        retain_english(destination)
        return destination
    cache = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "bokheim/tts"
    archive = Path(archive) if archive else cache / "vits-piper-en_GB-alba-medium.tar.bz2"
    if not archive.is_file():
        if offline:
            raise RuntimeError(f"Alba archive is not cached: {archive}")
        archive.parent.mkdir(parents=True, exist_ok=True)
        with urllib.request.urlopen(URL, timeout=120) as response, tempfile.NamedTemporaryFile(dir=archive.parent, delete=False) as output:
            downloaded = Path(output.name)
            shutil.copyfileobj(response, output)
        if hashlib.file_digest(downloaded.open("rb"), "sha256").hexdigest() != SHA256:
            downloaded.unlink()
            raise RuntimeError("Alba archive checksum mismatch")
        downloaded.replace(archive)
    with archive.open("rb") as source:
        if hashlib.file_digest(source, "sha256").hexdigest() != SHA256:
            raise RuntimeError("Alba archive checksum mismatch")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent) as staging:
        with tarfile.open(archive) as compressed:
            compressed.extractall(staging, filter="data")
        model = Path(staging) / "vits-piper-en_GB-alba-medium"
        (model / ".archive-sha256").write_text(SHA256 + "\n")
        (model / "VOICE-LICENSE.txt").write_text(
            "Piper Alba medium: Scottish English voice from the University of Edinburgh Alba corpus.\n"
            "Voice data/model attribution: University of Edinburgh and the Piper project.\n"
            "License: Creative Commons Attribution 4.0 (CC BY 4.0).\n"
            "https://datashare.ed.ac.uk/handle/10283/3270\n"
            "https://creativecommons.org/licenses/by/4.0/\n"
            "Runtime: Sherpa-ONNX (Apache-2.0), Piper phonemization/eSpeak-ng; see packaged runtime notices.\n")
        retain_english(model)
        if destination.exists():
            for item in model.iterdir():
                target = destination / item.name
                if item.is_dir(): shutil.copytree(item, target, dirs_exist_ok=True)
                else: shutil.copy2(item, target)
            # Remove dictionaries left behind by a previous, incomplete bundle.
            retain_english(destination)
        else:
            shutil.move(str(model), str(destination))
    return destination


def is_george_prepared(directory):
    marker = directory / ".assets-sha256"
    return marker.is_file() and marker.read_text().strip() == GEORGE_ASSET_SHA256 and all(
        (directory / name).is_file() for name in GEORGE_REQUIRED)


def prepare_george(destination=None, offline=False, archive=None):
    data = Path(os.environ.get("XDG_DATA_HOME", os.environ.get("LOCALAPPDATA", Path.home() / ".local/share")))
    installed = data / "bokheim/tts/george"
    destination = Path(destination) if destination else installed
    if is_george_prepared(destination):
        return destination
    if archive is None and is_george_prepared(installed):
        shutil.copytree(installed, destination, dirs_exist_ok=True)
        return destination
    cache = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "bokheim/tts"
    archive = Path(archive) if archive else cache / f"kokoro-multi-lang-v1_0-{GEORGE_SHA256}.tar.bz2"
    if not archive.is_file():
        if offline:
            raise RuntimeError(f"George archive is not cached: {archive}")
        archive.parent.mkdir(parents=True, exist_ok=True)
        with urllib.request.urlopen(GEORGE_URL, timeout=120) as response, tempfile.NamedTemporaryFile(dir=archive.parent, delete=False) as output:
            downloaded = Path(output.name)
            shutil.copyfileobj(response, output)
        with downloaded.open("rb") as source:
            valid = hashlib.file_digest(source, "sha256").hexdigest() == GEORGE_SHA256
        if not valid:
            downloaded.unlink()
            raise RuntimeError("George archive checksum mismatch")
        downloaded.replace(archive)
    with archive.open("rb") as source:
        if hashlib.file_digest(source, "sha256").hexdigest() != GEORGE_SHA256:
            raise RuntimeError("George archive checksum mismatch")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent) as staging:
        with tarfile.open(archive) as compressed:
            compressed.extractall(staging, filter="data")
        model = Path(staging) / "kokoro-multi-lang-v1_0"
        keep = {"model.onnx", "voices.bin", "tokens.txt", "lexicon-gb-en.txt", "espeak-ng-data", "LICENSE"}
        for item in model.iterdir():
            if item.name not in keep:
                if item.is_dir():
                    shutil.rmtree(item)
                else:
                    item.unlink()
        (model / "VOICE-LICENSE.txt").write_text(
            "Kokoro v1.0 FP32, George (bm_george), British English male voice.\n"
            "Model and voice embeddings: hexgrad/Kokoro-82M, Apache License 2.0.\n"
            "https://huggingface.co/hexgrad/Kokoro-82M\n"
            "Sherpa-ONNX conversion: https://github.com/k2-fsa/sherpa-onnx\n"
            "Pronunciation: British English lexicon and eSpeak-ng; see runtime notices.\n")
        retain_english(model, GEORGE_ASSET_SHA256)
        (model / ".archive-sha256").write_text(GEORGE_SHA256 + "\n")
        if not is_george_prepared(model):
            raise RuntimeError("George archive is missing required speech assets")
        if destination.exists():
            shutil.rmtree(destination)
        shutil.move(str(model), str(destination))
    return destination


def prepare_all(destination, offline=False):
    destination = Path(destination)
    prepare(destination / "alba", offline=offline)
    prepare_george(destination / "george", offline=offline)
    return destination


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dest", type=Path)
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--voice", choices=("alba", "george", "all"), default="alba")
    args = parser.parse_args()
    if args.voice == "all":
        if args.dest is None or args.archive is not None:
            parser.error("--voice all requires --dest and uses the verified per-voice caches")
        print(prepare_all(args.dest, args.offline))
    else:
        print((prepare if args.voice == "alba" else prepare_george)(args.dest, args.offline, args.archive))
