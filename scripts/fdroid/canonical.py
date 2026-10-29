"""Give Cargo's external path dependencies stable package identities."""
import json
from pathlib import Path
import shutil

CANONICAL_PARENT = Path("/mnt/bokheim-build")
CANONICAL_ROOT = CANONICAL_PARENT / "bokheim"


def command(root, arguments):
    bubblewrap = shutil.which("bwrap")
    if not bubblewrap:
        raise RuntimeError("Install bubblewrap to build F-Droid APKs at the canonical source path")
    result = [bubblewrap, "--die-with-parent", "--bind", "/", "/", "--dev-bind", "/dev", "/dev", "--tmpfs", "/mnt",
        "--dir", str(CANONICAL_PARENT), "--bind", str(root), str(CANONICAL_ROOT)]
    pins = json.loads((root / "scripts/fdroid/sources.json").read_text())
    names = [source["name"] for source in pins["sources"]]
    for name in names + [".fdroid-inputs", ".fdroid-tmp"]:
        source = root.parent / name
        if name.startswith("."):
            source.mkdir(exist_ok=True)
        if not source.is_dir():
            raise RuntimeError(f"Missing prefetched source directory: {source}")
        result += ["--bind", str(source), str(CANONICAL_PARENT / name)]
    return result + ["--chdir", str(CANONICAL_ROOT), "--", *arguments]
