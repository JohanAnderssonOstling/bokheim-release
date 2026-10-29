"""Shared acquisition, notices and receipt helpers for Android source builds."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request


def digest(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def write(path, data):
    path = Path(path)
    data = data.encode() if isinstance(data, str) else data
    path.parent.mkdir(parents=True, exist_ok=True)
    if not path.is_file() or path.read_bytes() != data:
        with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as stream:
            temporary = Path(stream.name)
            stream.write(data)
        temporary.replace(path)


def fetch(url, sha256, destination, offline=False):
    destination = Path(destination)
    if destination.is_file():
        if digest(destination) != sha256:
            raise RuntimeError(f"Cached input checksum mismatch: {destination}")
        return destination
    if offline:
        raise RuntimeError(f"Source input unavailable offline: {destination}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as output:
        temporary = Path(output.name)
        try:
            with urllib.request.urlopen(url, timeout=120) as response:
                shutil.copyfileobj(response, output)
            output.close()
            if digest(temporary) != sha256:
                raise RuntimeError(f"Downloaded input checksum mismatch: {url}")
            temporary.replace(destination)
        finally:
            temporary.unlink(missing_ok=True)
    return destination


def extract(archive, destination):
    destination = Path(destination)
    if destination.exists():
        return destination
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent) as staging:
        with tarfile.open(archive) as compressed:
            def source_filter(member, path):
                try:
                    return tarfile.data_filter(member, path)
                except (tarfile.AbsoluteLinkError, tarfile.LinkOutsideDestinationError):
                    # Some upstream example directories contain links to the
                    # author's machine. Never extract links escaping the tree.
                    return None
            compressed.extractall(staging, filter=source_filter)
        children = list(Path(staging).iterdir())
        if len(children) != 1 or not children[0].is_dir():
            raise RuntimeError(f"Expected one source directory in {archive}")
        children[0].replace(destination)
    return destination


def run(command, cwd=None, env=None):
    print("+ " + " ".join(map(str, command)), flush=True)
    subprocess.run(list(map(str, command)), cwd=cwd, env=env, check=True)


def revision(directory):
    return subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=directory, text=True).strip()


def apply_source_patch(patch, directory):
    # --force is essential here: --batch alone can silently reverse the
    # requested direction, mistaking an unapplied patch for an applied one.
    reverse = subprocess.run(["patch", "--reverse", "--dry-run", "--force", "--fuzz=0", "-p1", "-i", str(patch)],
        cwd=directory, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if reverse.returncode:
        run(["patch", "--forward", "--batch", "--fuzz=0", "-p1", "-i", patch], directory)


def notices(roots):
    parts = []
    for label, root in roots:
        root = Path(root)
        for base, dirs, files in os.walk(root):
            dirs[:] = sorted(name for name in dirs if name not in {
                ".git", "out", ".cipd", "node_modules", "llvm-build", "android_toolchain"})
            for name in sorted(files):
                if not (re.match(r"^(LICENSE|COPYING|NOTICE|COPYRIGHT)(?:[._-].*)?$", name, re.I)
                        or name in {"FTL.TXT", "GPLv2.TXT", "README.ijg", "README.pdfium", "README.chromium"}):
                    continue
                path = Path(base) / name
                if path.is_symlink() or not path.is_file():
                    continue
                parts.append(f"===== {label}/{path.relative_to(root)} =====\n".encode()
                    + path.read_bytes().decode("utf-8", errors="replace").encode())
    if not parts:
        raise RuntimeError("No source license notices found")
    return b"\n\n".join(parts) + b"\n"


def receipt(directory, metadata, files):
    metadata = dict(metadata, files={name: digest(Path(directory) / name) for name in files})
    write(Path(directory) / "receipt.json", json.dumps(metadata, indent=2, sort_keys=True) + "\n")
    return metadata


def validate_receipt(directory, required, pins_sha256):
    directory = Path(directory)
    data = json.loads((directory / "receipt.json").read_text())
    if data.get("kind") != "source" or data.get("source_manifest_sha256") != pins_sha256:
        raise RuntimeError("Source-build receipt does not match pinned configuration")
    for name in required:
        if Path(name).is_absolute() or ".." in Path(name).parts:
            raise RuntimeError(f"Invalid prepared file name: {name}")
        if digest(directory / name) != data.get("files", {}).get(name):
            raise RuntimeError(f"Source-built file checksum mismatch: {name}")
    return data
