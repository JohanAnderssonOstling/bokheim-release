#!/usr/bin/env python3
"""Build Sherpa/Piper/eSpeak from source against official Microsoft Maven ONNX."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/fdroid"))
from native_build import digest, extract, fetch, notices, receipt, run, write

PINS = Path(__file__).with_name("source.json")


def prefetch_sources(work, offline=False):
    pins = json.loads(PINS.read_text())
    dependency_pins = PINS.with_name("source-dependencies.json")
    if digest(dependency_pins) != pins["dependencies_manifest_sha256"]:
        raise RuntimeError("Speech dependency pins checksum mismatch")
    work = Path(work).resolve()
    archive = fetch(pins["source_url"], pins["source_sha256"], work / "sherpa.tar.gz", offline)
    source = extract(archive, work / "source")
    for item in json.loads(dependency_pins.read_text())["dependencies"]:
        fetch(item["url"], item["sha256"], work / "downloads" / (item["name"] + ".archive"), offline)
    return source


def dependency_sources(work):
    """Stage pinned source archives without CMake downloads or compiler checks."""
    flags = ["-DFETCHCONTENT_FULLY_DISCONNECTED=ON"]
    for item in json.loads(PINS.with_name("source-dependencies.json").read_text())["dependencies"]:
        archive = work / "downloads" / (item["name"] + ".archive")
        destination = work / "build/_deps" / (item["name"] + "-src")
        # Sherpa patches these sources during configure. Start with pristine
        # archive contents so repeated configure calls cannot accumulate edits.
        if destination.exists():
            shutil.rmtree(destination)
        if zipfile.is_zipfile(archive):
            with zipfile.ZipFile(archive) as compressed:
                with tempfile.TemporaryDirectory(dir=work) as temporary:
                    for info in compressed.infolist():
                        path = Path(info.filename)
                        if path.is_absolute() or ".." in path.parts:
                            raise RuntimeError("Unsafe speech dependency archive")
                    compressed.extractall(temporary)
                    children = list(Path(temporary).iterdir())
                    if len(children) != 1 or not children[0].is_dir():
                        raise RuntimeError("Expected one speech dependency source directory")
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    children[0].replace(destination)
        else:
            extract(archive, destination)
        flags.append(f"-DFETCHCONTENT_SOURCE_DIR_{item['name'].upper()}={destination}")
    return flags


def prepare_source(destination, aar, ndk, work=None, jobs=2, offline=False):
    pins = json.loads(PINS.read_text())
    for name, metadata in pins["licenses"].items():
        if digest(PINS.parent / "licenses" / name) != metadata["sha256"]:
            raise RuntimeError(f"Runtime or voice license checksum mismatch: {name}")
    destination = Path(destination)
    work = Path(work or ROOT / ".tmp/speech-source-1.13.8").resolve()
    ndk = Path(ndk).resolve()
    if not (ndk / "build/cmake/android.toolchain.cmake").is_file():
        raise RuntimeError(f"Android NDK is incomplete: {ndk}")
    ndk_properties = (ndk / "source.properties").read_text()
    if f"Pkg.Revision = {pins['ndk_version']}" not in ndk_properties.splitlines():
        raise RuntimeError(f"Speech source builds require Android NDK {pins['ndk_version']}")
    if digest(aar) != pins["onnxruntime_sha256"]:
        raise RuntimeError("ONNX Runtime AAR does not match the pinned official Maven artifact")
    work.mkdir(parents=True, exist_ok=True)
    source = prefetch_sources(work, offline)
    onnx = work / "onnxruntime"
    with zipfile.ZipFile(aar) as compressed:
        for name in compressed.namelist():
            if name.startswith("headers/") or name == "jni/arm64-v8a/libonnxruntime.so" or name.startswith("META-INF/") and any(s in name.upper() for s in ("LICENSE", "NOTICE")):
                path = Path(name)
                if path.is_absolute() or ".." in path.parts or name.endswith("/"):
                    continue
                write(onnx / path, compressed.read(name))
    env = os.environ.copy()
    env.update(SHERPA_ONNXRUNTIME_LIB_DIR=str(onnx / "jni/arm64-v8a"),
        SHERPA_ONNXRUNTIME_INCLUDE_DIR=str(onnx / "headers"))
    build = work / "build"
    flags = [f"-DCMAKE_TOOLCHAIN_FILE={ndk}/build/cmake/android.toolchain.cmake",
        "-DANDROID_ABI=arm64-v8a", "-DANDROID_PLATFORM=android-26", "-DANDROID_STL=c++_static",
        "-DCMAKE_BUILD_TYPE=Release", "-DBUILD_SHARED_LIBS=ON", "-DSHERPA_ONNX_ENABLE_C_API=ON",
        "-DSHERPA_ONNX_ENABLE_JNI=OFF", "-DSHERPA_ONNX_ENABLE_TTS=ON"]
    for name in ("PORTAUDIO", "WEBSOCKET", "BINARY", "TESTS", "CHECK", "SPEAKER_DIARIZATION", "PYTHON"):
        flags.append(f"-DSHERPA_ONNX_ENABLE_{name}=OFF")
    flags.append("-DSHERPA_ONNX_BUILD_C_API_EXAMPLES=OFF")
    flags += dependency_sources(work)
    remaps = f"-ffile-prefix-map={work}=/build/speech -ffile-prefix-map={ndk}=/build/android-ndk"
    flags += [f"-DCMAKE_C_FLAGS={remaps}", f"-DCMAKE_CXX_FLAGS={remaps}"]
    # LLD hashes debug sections into the build ID before a later llvm-strip.
    # Discard them while linking so checkout paths cannot change the runtime ID.
    flags.append("-DCMAKE_SHARED_LINKER_FLAGS=-Wl,--strip-debug")
    run(["cmake", "-S", source, "-B", build, "-G", "Ninja", *flags], env=env)
    run(["cmake", "--build", build, "--target", "sherpa-onnx-c-api", "--parallel", str(jobs)], env=env)
    built = list(build.glob("**/libsherpa-onnx-c-api.so"))
    if len(built) != 1:
        raise RuntimeError("Expected exactly one Sherpa C API shared library")
    destination.mkdir(parents=True, exist_ok=True)
    temporary = work / "libsherpa-onnx-c-api.stripped.so"
    shutil.copy2(built[0], temporary)
    strip = ndk / "toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip"
    run([strip, "--strip-unneeded", temporary])
    write(destination / "libsherpa-onnx-c-api.so", temporary.read_bytes())
    write(destination / "libonnxruntime.so", (onnx / "jni/arm64-v8a/libonnxruntime.so").read_bytes())
    roots = [("sherpa-onnx", source), ("microsoft-onnxruntime", onnx)]
    roots += [("dependencies/" + p.name, p) for p in sorted((build / "_deps").glob("*-src"))]
    license_data = notices(roots)
    # Maven does not always include its license texts; preserve the pinned MIT
    # license and third-party notices obtained from Microsoft's matching tag.
    for name in ("onnxruntime-LICENSE.txt", "onnxruntime-ThirdPartyNotices.txt"):
        license_data += b"\n===== " + name.encode() + b" =====\n" + (PINS.parent / "licenses" / name).read_bytes()
    write(destination / "LICENSE", license_data)
    provenance = dict(pins, components={"libsherpa-onnx-c-api.so": "source", "libonnxruntime.so": "official-maven"},
        cmake_flags=[flag.replace(str(ndk), "${ANDROID_NDK}").replace(str(work), "${SPEECH_SOURCE_WORK}")
            for flag in flags], ndk=ndk_properties)
    write(destination / "SOURCE.json", json.dumps(provenance, indent=2, sort_keys=True) + "\n")
    receipt(destination, {"schema": 1, "kind": "source", "target": pins["target"],
        "source_manifest_sha256": digest(PINS), "source_commit": pins["sherpa_commit"]},
        ["libsherpa-onnx-c-api.so", "libonnxruntime.so", "LICENSE", "SOURCE.json"])
    return destination.resolve()
