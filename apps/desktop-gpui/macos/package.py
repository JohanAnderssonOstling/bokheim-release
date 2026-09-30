#!/usr/bin/env python3
"""Package release builds for manual macOS downloads, without Developer ID."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[3]
ARCHES = {"arm64": "aarch64-apple-darwin", "x86_64": "x86_64-apple-darwin"}


def run(*args):
    subprocess.run([str(arg) for arg in args], check=True)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def executable(app):
    return app / "Contents/MacOS/Bokheim"


def pdfium(app):
    return app / "Contents/Frameworks/libpdfium.dylib"


def verify_arches(path, expected):
    actual = set(subprocess.check_output(["lipo", "-archs", str(path)], text=True).split())
    if actual != set(expected):
        raise ValueError(f"Unexpected architectures in {path}: {actual}")


def sign_and_verify(app):
    # Sign from the inside out. Ad-hoc signatures require no Apple account;
    # users still explicitly approve this unnotarized app in macOS settings.
    for path in (pdfium(app), executable(app), app):
        run("codesign", "--force", "--sign", "-", path)
    run("codesign", "--verify", "--deep", "--strict", app)


def architecture(args, version, source):
    target = ARCHES[args.arch]
    binary = ROOT / "target" / target / "release/desktop-gpui"
    prepared = ROOT / ".pdfium" / target
    if not binary.is_file() or not (prepared / "libpdfium.dylib").is_file():
        raise ValueError(f"Build the optimized {target} desktop app first")
    with tempfile.TemporaryDirectory() as scratch:
        scratch = Path(scratch)
        app = scratch / "Bokheim.app"
        resources = app / "Contents/Resources"
        resources.mkdir(parents=True)
        executable(app).parent.mkdir(parents=True)
        shutil.copy2(binary, executable(app))
        pdfium(app).parent.mkdir()
        shutil.copy2(prepared / "libpdfium.dylib", pdfium(app))
        notices = resources / "pdfium"
        notices.mkdir()
        for name in ("LICENSE", "VERSION"):
            shutil.copy2(prepared / name, notices / name)
        iconset = scratch / "Bokheim.iconset"
        iconset.mkdir()
        icons = ROOT / "apps/ui/design-tokens/assets/icon"
        for size in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                pixels = size * scale
                destination = iconset / f"icon_{size}x{size}{'@2x' if scale == 2 else ''}.png"
                original = icons / f"bokheim-{min(pixels, 512)}.png"
                if pixels <= 512:
                    shutil.copy2(original, destination)
                else:
                    run("sips", "-z", pixels, pixels, original, "--out", destination)
        run("iconutil", "-c", "icns", iconset, "-o", resources / "Bokheim.icns")
        info = dict(CFBundleIdentifier="se.bokheim.Bokheim", CFBundleName="Bokheim",
                    CFBundleDisplayName="Bokheim", CFBundleExecutable="Bokheim",
                    CFBundlePackageType="APPL", CFBundleIconFile="Bokheim.icns",
                    CFBundleShortVersionString=version, CFBundleVersion=version,
                    LSMinimumSystemVersion="13.0", NSHighResolutionCapable=True,
                    NSPrincipalClass="NSApplication")
        (app / "Contents/Info.plist").write_bytes(plistlib.dumps(info))
        for path in (executable(app), pdfium(app)):
            verify_arches(path, [args.arch])
        sign_and_verify(app)
        archive = args.output / f"Bokheim-macOS-{args.arch}.zip"
        run("ditto", "-c", "-k", "--keepParent", app, archive)
        metadata = dict(application=version, source_commit=source, architecture=args.arch,
                        sha256=digest(archive), signing="ad-hoc", notarized=False)
        archive.with_suffix(".json").write_text(json.dumps(metadata, indent=2) + "\n")


def universal(args, version, source):
    inputs = {"arm64": args.arm64, "x86_64": args.x86_64}
    with tempfile.TemporaryDirectory() as scratch:
        scratch = Path(scratch)
        apps = {}
        for arch, archive in inputs.items():
            metadata = json.loads(archive.with_suffix(".json").read_text())
            if (metadata.get("application"), metadata.get("source_commit"), metadata.get("architecture"), metadata.get("sha256")) != (version, source, arch, digest(archive)):
                raise ValueError(f"Archive version, source or checksum mismatch: {archive}")
            destination = scratch / arch
            run("ditto", "-x", "-k", archive, destination)
            apps[arch] = destination / "Bokheim.app"
            for path in (executable(apps[arch]), pdfium(apps[arch])):
                verify_arches(path, [arch])
        if (apps["arm64"] / "Contents/Info.plist").read_bytes() != (apps["x86_64"] / "Contents/Info.plist").read_bytes():
            raise ValueError("The architecture bundles have different application metadata")
        staging = scratch / "disk"
        staging.mkdir()
        app = staging / "Bokheim.app"
        shutil.copytree(apps["arm64"], app)
        for location in ("Contents/MacOS/Bokheim", "Contents/Frameworks/libpdfium.dylib"):
            run("lipo", "-create", apps["arm64"] / location, apps["x86_64"] / location,
                "-output", app / location)
            verify_arches(app / location, ARCHES)
        sign_and_verify(app)
        (staging / "Applications").symlink_to("/Applications")
        (staging / "Install.txt").write_text(
            "Drag Bokheim to Applications.\n\n"
            "If macOS blocks it, try opening Bokheim, then choose System Settings > "
            "Privacy & Security > Open Anyway and confirm Open.\n"
            "This download is not notarized by Apple.\n")
        image = args.output / "Bokheim-macOS-universal.dmg"
        run("hdiutil", "create", "-volname", "Bokheim", "-srcfolder", staging,
            "-format", "UDZO", image)
        run("hdiutil", "verify", image)
        sha = digest(image)
        image.with_suffix(".dmg.sha256").write_text(f"{sha}  {image.name}\n")
        (args.output / "build-macos.json").write_text(json.dumps(
            dict(application=version, source_commit=source, architectures=list(ARCHES),
                 minimum_macos="13.0", signing="ad-hoc", notarized=False,
                 artifact=dict(name=image.name, bytes=image.stat().st_size, sha256=sha)),
            indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("architecture", "universal"))
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--arch", choices=ARCHES)
    parser.add_argument("--arm64", type=Path)
    parser.add_argument("--x86-64", dest="x86_64", type=Path)
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("macOS packaging requires Apple's codesign, lipo and hdiutil tools")
    if args.mode == "architecture" and not args.arch:
        parser.error("architecture mode requires --arch")
    if args.mode == "universal" and not (args.arm64 and args.x86_64):
        parser.error("universal mode requires both architecture archives")
    args.output = args.output.resolve()
    if args.output.exists():
        parser.error("--output must be a fresh directory")
    args.output.mkdir(parents=True)
    version = tomllib.loads((ROOT / "apps/desktop-gpui/Cargo.toml").read_text())["package"]["version"]
    source = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    if os.environ.get("GITHUB_SHA", source) != source:
        raise ValueError("Checkout differs from the workflow source commit")
    (architecture if args.mode == "architecture" else universal)(args, version, source)


if __name__ == "__main__":
    main()
