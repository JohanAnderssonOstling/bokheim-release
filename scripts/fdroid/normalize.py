"""Canonicalize unsigned APK alignment using F-Droid's pinned upstream tool."""
import json
from pathlib import Path
import runpy
import tempfile

from native_build import digest, extract, fetch

ROOT = Path(__file__).resolve().parents[2]


def normalizer(offline=False):
    pins = json.loads((ROOT / "scripts/fdroid/toolchains.json").read_text())["apk_normalizer"]
    cache = ROOT.parent / ".fdroid-inputs"
    archive = fetch(pins["url"], pins["sha256"], cache / f"reproducible-apk-tools-{pins['commit']}.tar.gz", offline)
    source = extract(archive, cache / f"reproducible-apk-tools-{pins['commit']}")
    script = source / "zipalign.py"
    if digest(script) != pins["zipalign_sha256"]:
        raise RuntimeError("Pinned APK normalizer source checksum mismatch")
    return runpy.run_path(str(script))["zipalign"]


def normalize(apk, offline=True):
    apk = Path(apk)
    align = normalizer(offline)
    with tempfile.TemporaryDirectory(prefix=".align-", dir=apk.parent) as temporary:
        output = Path(temporary) / "normalized.apk"
        align(str(apk), str(output), page_align=True, page_size=16,
            pad_like_apksigner=True, replace=True)
        output.replace(apk)
