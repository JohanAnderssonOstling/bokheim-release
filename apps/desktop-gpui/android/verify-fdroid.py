#!/usr/bin/env python3
"""Verify a F-Droid APK, its signing identity and native-source receipts."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import struct
import subprocess
import zipfile


def require(value, message):
    if not value:
        raise RuntimeError(message)


def verify(apk, aapt, certificate=None, environment=None):
    if certificate is not None:
        require(re.fullmatch(r"[0-9a-f]{64}", certificate), "Invalid signing certificate SHA-256")
        signature = subprocess.check_output([str(Path(aapt).with_name("apksigner")),
            "verify", "--verbose", "--print-certs", str(apk)], text=True, env=environment)
        signers = re.findall(r"^Signer #\d+ certificate SHA-256 digest: ([0-9a-fA-F]{64})$", signature, re.M)
        require([signer.lower() for signer in signers] == [certificate], "APK signing certificate mismatch")
    zipalign = Path(aapt).with_name("zipalign")
    subprocess.run([str(zipalign), "-c", "-P", "16", "4", str(apk)], check=True, env=environment)
    manifest = subprocess.check_output([str(aapt), "dump", "xmltree", str(apk), "AndroidManifest.xml"], text=True, env=environment)
    require("android.permission.REQUEST_INSTALL_PACKAGES" not in manifest, "APK requests permission to install updates")
    for name in ("UpdateMigrationService", "UpdateRestartActivity", ".updates"):
        require(name not in manifest, f"Updater component is present: {name}")
    require("android.permission.MANAGE_EXTERNAL_STORAGE" in manifest, "F-Droid storage access was accidentally restricted")
    badging = subprocess.check_output([str(aapt), "dump", "badging", str(apk)], text=True, env=environment)
    require("application-debuggable" not in badging, "F-Droid release is debuggable")
    require("package: name='se.bokheim.reader.gpui'" in badging, "Unexpected application ID")
    libraries = {}
    with zipfile.ZipFile(apk) as archive:
        names = archive.namelist()
        require(all(name.startswith("lib/arm64-v8a/") for name in names if name.startswith("lib/")), "Unexpected native ABI in APK")
        if certificate is None:
            require(not any(re.match(r"META-INF/.*\.(RSA|DSA|EC)$", n, re.I) for n in names), "Expected an unsigned APK")
            require(b"APK Sig Block 42" not in Path(apk).read_bytes(), "Expected no APK v2/v3 signing block")
        for name in names:
            if name.endswith(".dex"):
                dex = archive.read(name)
                for package in (b"Lcom/google/android/gms/", b"Lcom/google/firebase/", b"Lcom/crashlytics/"):
                    require(package not in dex, f"Proprietary SDK classes found in {name}: {package!r}")
        pdfium = json.loads(archive.read("assets/pdfium/receipt.json"))
        speech = json.loads(archive.read("assets/speech/receipt.json"))
        require(pdfium.get("kind") == "source" and speech.get("kind") == "source", "Missing native source receipts")
        speech_source = json.loads(archive.read("assets/speech/SOURCE.json"))
        require(speech_source["components"]["libonnxruntime.so"] == "official-maven", "Unidentified ONNX origin")
        require(speech_source["onnxruntime_coordinate"] == "com.microsoft.onnxruntime:onnxruntime-android:1.28.0", "Wrong ONNX Maven dependency")
        for name in ("libdesktop_gpui.so", "libpdfium.so", "libsherpa-onnx-c-api.so", "libonnxruntime.so"):
            data = archive.read("lib/arm64-v8a/" + name)
            require(data[:6] == b"\x7fELF\x02\x01" and struct.unpack_from("<H", data, 18)[0] == 183,
                f"Invalid ARM64 library: {name}")
            offset = struct.unpack_from("<Q", data, 32)[0]
            size, count = struct.unpack_from("<HH", data, 54)
            loads = [struct.unpack_from("<IIQQQQQQ", data, offset + i * size) for i in range(count)]
            loads = [segment for segment in loads if segment[0] == 1]
            require(loads and all(s[7] >= 16384 and (s[3] - s[2]) % 16384 == 0 for s in loads),
                f"Library does not support 16 KB pages: {name}")
            sha = hashlib.sha256(data).hexdigest()
            if name != "libdesktop_gpui.so":
                receipt = pdfium if name == "libpdfium.so" else speech
                require(sha == receipt["files"][name], f"APK native library differs from source-build receipt: {name}")
            libraries[name] = {"sha256": sha, "bytes": len(data), "supports_16k_pages": True}
        for name in ("assets/bokheim/LICENSE", "assets/bokheim/COPYRIGHT", "assets/pdfium/LICENSE", "assets/speech/LICENSE",
                "assets/rustls-platform-verifier/LICENSE-MIT", "assets/rustls-platform-verifier/LICENSE-APACHE",
                "assets/tts/alba/alba-model-card.txt", "assets/tts/alba/alba-provenance.json", "assets/tts/alba/CC-BY-4.0.txt"):
            require(archive.getinfo(name).file_size > 0, f"Missing attribution: {name}")
        for directory, receipt in (("pdfium", pdfium), ("speech", speech)):
            for name in ("LICENSE", "SOURCE.json"):
                sha = hashlib.sha256(archive.read(f"assets/{directory}/{name}")).hexdigest()
                require(sha == receipt["files"][name], f"Changed packaged provenance: {directory}/{name}")
    return {"apk_sha256": hashlib.sha256(Path(apk).read_bytes()).hexdigest(), "unsigned": certificate is None,
        "signing_cert_sha256": [certificate] if certificate else [],
        "updater_components_removed": True, "proprietary_sdk_class_check": "passed", "libraries": libraries,
        "native_receipts_match_apk": True, "apk_supports_16k_page_alignment": True, "device_runtime_tested": False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apk", required=True, type=Path)
    parser.add_argument("--aapt", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--cert-sha256", help="Verify a signed APK against this certificate; otherwise require unsigned")
    args = parser.parse_args()
    report = verify(args.apk, args.aapt, args.cert_sha256)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print("F-Droid APK checks passed.")


if __name__ == "__main__":
    main()
