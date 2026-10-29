#!/usr/bin/env python3
"""Build the pinned Android PDFium C++ source and write a source receipt."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/fdroid"))
from native_build import apply_source_patch, digest, notices, receipt, revision, run, write

PINS = Path(__file__).with_name("source.json")


def prepare_source(destination, work=None, jobs=2, offline=False, fetch_only=False):
    pins = json.loads(PINS.read_text())
    work = Path(work or ROOT / ".tmp/pdfium-source-build-7961").resolve()
    depot = work / "depot_tools"
    checkout = work / "checkout"
    source = checkout / "pdfium"
    destination = Path(destination)
    work.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env.update(PATH=str(depot) + os.pathsep + env["PATH"], DEPOT_TOOLS_UPDATE="0", DEPOT_TOOLS_METRICS="0")
    short_tmp = Path.home() / ".cache/bokheim-native-tmp"
    short_tmp.mkdir(parents=True, exist_ok=True)
    env["TMPDIR"] = str(short_tmp)
    for key, name in [("CIPD_CACHE_DIR", "cipd-cache"), ("CIPD_HTTP_CACHE_DIR", "cipd-http-cache"),
            ("VPYTHON_VIRTUALENV_ROOT", "vpython")]:
        (work / name).mkdir(exist_ok=True)
        env[key] = str(work / name)
    if not (depot / "gclient").is_file():
        if offline:
            raise RuntimeError("PDFium source tools are unavailable offline")
        depot.mkdir(exist_ok=True)
        if not (depot / ".git").exists():
            run(["git", "init", "--quiet"], depot)
            run(["git", "remote", "add", "origin", "https://chromium.googlesource.com/chromium/tools/depot_tools.git"], depot)
        run(["git", "fetch", "--depth", "1", "origin", pins["depot_tools_commit"]], depot)
        run(["git", "checkout", "--detach", "FETCH_HEAD"], depot)
    if revision(depot) != pins["depot_tools_commit"]:
        raise RuntimeError("depot_tools checkout is not at the pinned revision")
    synced = work / "synced.json"
    if not offline:
        run([depot / "ensure_bootstrap"], work, env)
    if not synced.is_file():
        if offline:
            raise RuntimeError("PDFium sources are unavailable offline")
        checkout.mkdir(exist_ok=True)
        if not (checkout / ".gclient").is_file():
            run(["gclient", "config", "--unmanaged", "https://pdfium.googlesource.com/pdfium.git",
                "--custom-var", "checkout_configuration=minimal"], checkout, env)
            with (checkout / ".gclient").open("a") as output:
                output.write("target_os = ['android']\n")
        run(["gclient", "sync", "-r", "pdfium@" + pins["pdfium_commit"],
            "--no-history", "--shallow", "--jobs", str(jobs)], checkout, env)
        write(synced, json.dumps({"pdfium_revision": pins["pdfium_commit"]}) + "\n")
    if revision(source) != pins["pdfium_commit"] or revision(source / "build") != pins["chromium_build_commit"]:
        raise RuntimeError("PDFium/build sources are not at the pinned revisions")
    for item in pins["patches"]:
        patch = PINS.parent / "patches" / item["name"]
        if digest(patch) != item["sha256"]:
            raise RuntimeError(f"PDFium patch checksum mismatch: {patch}")
        directory = source / item["directory"]
        apply_source_patch(patch, directory)
    # Refuse unrelated tracked modifications in this builder-owned checkout.
    for directory in (source, source / "build"):
        allowed = set()
        for item in pins["patches"]:
            if (source / item["directory"]).resolve() == directory.resolve():
                for line in (PINS.parent / "patches" / item["name"]).read_text().splitlines():
                    if line.startswith("+++ b/"):
                        allowed.add(line[6:].split()[0])
        changed = subprocess.check_output(["git", "diff", "--name-only"], cwd=directory, text=True).splitlines()
        if not set(changed) <= allowed:
            raise RuntimeError(f"Unexpected source changes: {set(changed) - allowed}")
        with tempfile.TemporaryDirectory(dir=work) as pristine:
            pristine = Path(pristine)
            for name in allowed:
                path = pristine / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(subprocess.check_output(["git", "show", "HEAD:" + name], cwd=directory))
            for item in pins["patches"]:
                if (source / item["directory"]).resolve() == directory.resolve():
                    subprocess.run(["patch", "--batch", "--fuzz=0", "-p1", "-i",
                        str(PINS.parent / "patches" / item["name"])], cwd=pristine,
                        stdout=subprocess.DEVNULL, check=True)
            for name in allowed:
                if (directory / name).read_bytes() != (pristine / name).read_bytes():
                    raise RuntimeError(f"PDFium source differs from the pinned patch set: {name}")
    if fetch_only:
        return source
    out = source / "out/android-arm64"
    args = '\n'.join(['target_os = "android"', 'target_cpu = "arm64"', 'is_debug = false',
        'is_component_build = false', 'pdf_is_standalone = true', 'pdf_use_partition_alloc = false',
        'pdf_enable_v8 = false', 'pdf_enable_xfa = false', 'treat_warnings_as_errors = false',
        'clang_use_chrome_plugins = false', 'default_min_sdk_version = 23', 'use_mold = false',
        'symbol_level = 0', 'use_thin_lto = false', ''])
    write(out / "args.gn", args)
    run(["gn", "gen", "out/android-arm64"], source, env)
    run(["ninja", "-C", "out/android-arm64", "-j", str(jobs), "pdfium"], source, env)
    clang_version = subprocess.check_output([source / "third_party/llvm-build/Release+Asserts/bin/clang", "--version"], text=True)
    provenance = dict(pins, args_gn=args, source="https://pdfium.googlesource.com/pdfium.git",
        clang_version="\n".join(line for line in clang_version.splitlines() if not line.startswith("InstalledDir:")))
    destination.mkdir(parents=True, exist_ok=True)
    write(destination / "libpdfium.so", (out / "libpdfium.so").read_bytes())
    write(destination / "LICENSE", notices([("pdfium", source)]))
    write(destination / "SOURCE.json", json.dumps(provenance, indent=2, sort_keys=True) + "\n")
    write(destination / "VERSION", f"PDFium {pins['pdfium_branch']}\nBuilt from source: {pins['pdfium_commit']}\n")
    receipt(destination, {"schema": 2, "kind": "source", "target": pins["target"],
        "manifest_sha256": digest(PINS.with_name("artifacts.toml")),
        "source_manifest_sha256": digest(PINS), "source_commit": pins["pdfium_commit"]},
        ["libpdfium.so", "LICENSE", "VERSION", "SOURCE.json"])
    return destination.resolve()
