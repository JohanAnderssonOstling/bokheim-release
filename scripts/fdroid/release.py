#!/usr/bin/env python3
"""Build, sign and verify a F-Droid APK using the existing production keystore."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile

from native_build import digest, write
from normalize import normalize, normalizer
from prepare_checkout import UNUSED_INPUTS

ROOT = Path(__file__).resolve().parents[2]
GRADLE_VERSION = "8.14.3"
BUILD_TOOLS = "36.0.0"
SIGNING_TOOLS = "36.0.0"
APK_PATH = Path("apps/desktop-gpui/android/build/outputs/apk/release/BokheimGpui-release-unsigned.apk")


def java_environment(environment):
    candidates = []
    if environment.get("JAVA_HOME"):
        candidates.append(Path(environment["JAVA_HOME"]).expanduser())
    candidates += sorted(Path("/usr/lib/jvm").glob("*21*"))
    candidates += sorted((Path.home() / ".jdks").glob("*21*"))
    for home in candidates:
        java = home / "bin/java"
        if not java.is_file():
            continue
        version = subprocess.run([str(java), "-version"], capture_output=True, text=True)
        if version.returncode == 0 and re.search(r'version "21(?:\.|\")', version.stderr + version.stdout):
            return dict(environment, JAVA_HOME=str(home.resolve()),
                PATH=str(home.resolve() / "bin") + os.pathsep + environment.get("PATH", os.defpath))
    raise RuntimeError("JDK 21 is missing: install it and configure JAVA_HOME")


def properties(path):
    """Read Java properties, including escaped paths, separators and continuations."""
    result = {}
    if not path.is_file():
        return result
    logical = ""
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.lstrip(" \t\f")
        if not logical and (not line or line.startswith(("#", "!"))):
            continue
        logical += line
        if (len(logical) - len(logical.rstrip("\\"))) % 2:
            logical = logical[:-1]
            continue
        match = re.match(r"((?:\\.|[^\s:=])+)\s*(?:[=:]\s*)?(.*)$", logical)
        if match:
            def unescape(value):
                return re.sub(r"\\u([0-9a-fA-F]{4})|\\(.)", lambda m:
                    chr(int(m[1], 16)) if m[1] else {"t": "\t", "n": "\n", "r": "\r", "f": "\f"}.get(m[2], m[2]), value)
            result[unescape(match[1])] = unescape(match[2])
        logical = ""
    if logical:
        raise RuntimeError(f"Incomplete property continuation in {path}")
    return result


def signing_settings(android, environment):
    # User Gradle properties take precedence over the project properties.
    values = properties(android / "gradle.properties")
    gradle_home = Path(environment.get("GRADLE_USER_HOME", Path.home() / ".gradle")).expanduser()
    values.update(properties(gradle_home / "gradle.properties"))

    def setting(name):
        return environment.get(name, values.get(name, environment.get("ORG_GRADLE_PROJECT_" + name)))

    def password(name):
        value = setting(name)
        if value is not None:
            return value
        filename = setting(name + "_FILE")
        if filename:
            return (android / Path(filename).expanduser()).read_text().rstrip("\r\n")
        return None

    store = setting("BOKHEIM_ANDROID_KEYSTORE")
    alias = setting("BOKHEIM_ANDROID_KEY_ALIAS")
    store_password = password("BOKHEIM_ANDROID_STORE_PASSWORD")
    key_password = password("BOKHEIM_ANDROID_KEY_PASSWORD")
    if key_password is None:
        key_password = store_password
    if not all((store, alias, store_password, key_password)):
        raise RuntimeError("Configure the existing BOKHEIM_ANDROID_KEYSTORE, KEY_ALIAS and STORE_PASSWORD "
            "(or STORE_PASSWORD_FILE) in the environment or ~/.gradle/gradle.properties")
    keystore = (android / Path(store).expanduser()).resolve()
    if not keystore.is_file():
        raise RuntimeError("The configured Android production keystore does not exist")
    child_environment = dict(environment, BOKHEIM_FDROID_STORE_PASSWORD=store_password,
        BOKHEIM_FDROID_KEY_PASSWORD=key_password)
    return keystore, alias, child_environment


def signing_certificate(keystore, alias, environment):
    java_home = environment.get("JAVA_HOME")
    keytool = str(Path(java_home) / "bin/keytool") if java_home else shutil.which("keytool")
    if not keytool:
        raise RuntimeError("keytool is missing: configure JAVA_HOME for JDK 21")
    result = subprocess.run([keytool, "-exportcert", "-keystore", str(keystore), "-alias", alias,
        "-storepass:env", "BOKHEIM_FDROID_STORE_PASSWORD"], env=environment,
        stdin=subprocess.DEVNULL, capture_output=True)
    if result.returncode or not result.stdout:
        raise RuntimeError("Cannot read the production signing certificate; check the keystore, alias and password settings")
    certificate = hashlib.sha256(result.stdout).hexdigest()
    expected = environment.get("BOKHEIM_ANDROID_CERT_SHA256") or environment.get("ANDROID_CERT_SHA256")
    if expected and certificate != expected.lower().replace(":", ""):
        raise RuntimeError("Configured keystore does not match the expected production certificate SHA-256")
    return certificate


def sdk_directory(requested, environment):
    sdk = Path(requested or environment.get("ANDROID_HOME") or environment.get("ANDROID_SDK_ROOT")
        or Path.home() / "Android/Sdk").expanduser().resolve()
    for name in ("aapt", "apksigner", "zipalign"):
        if not (sdk / "build-tools" / BUILD_TOOLS / name).is_file():
            raise RuntimeError(f"Missing Android SDK build-tools {BUILD_TOOLS}: {name}")
    return sdk


def gradle_command(requested):
    if requested:
        found = shutil.which(requested)
        if found:
            return found
        raise RuntimeError("The requested Gradle executable does not exist")
    found = shutil.which("gradle")
    if found:
        return found
    candidates = sorted((Path.home() / ".gradle/wrapper/dists").glob(
        f"gradle-{GRADLE_VERSION}-bin/*/gradle-{GRADLE_VERSION}/bin/gradle"))
    if candidates:
        return str(candidates[0])
    raise RuntimeError(f"Install Gradle {GRADLE_VERSION} or pass --gradle /path/to/gradle")


def checkout_snapshot(ref, work):
    commit = subprocess.check_output(["git", "-C", str(ROOT), "rev-parse", "--verify", ref + "^{commit}"], text=True).strip()
    directory = work / commit
    source = directory / "bokheim"
    marker = directory / "SOURCE-COMMIT"
    if directory.exists():
        if not marker.is_file() or marker.read_text().strip() != commit:
            raise RuntimeError("Existing release workspace is not an owned source snapshot; use a different --work-dir")
        return source, commit
    directory.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=directory.parent) as temporary:
        staging = Path(temporary)
        archive = staging / "source.tar"
        subprocess.run(["git", "-C", str(ROOT), "archive", "--format=tar", "--output", str(archive), commit], check=True)
        with tarfile.open(archive) as stream:
            def source_filter(member, destination):
                if member.name in UNUSED_INPUTS:
                    return None
                return tarfile.data_filter(member, destination)
            stream.extractall(staging / "bokheim", filter=source_filter)
        archive.unlink()
        if not (staging / "bokheim/scripts/fdroid/prepare_checkout.py").is_file():
            raise RuntimeError("Selected commit has no F-Droid build support; pass --ref with the F-Droid review branch")
        (staging / "SOURCE-COMMIT").write_text(commit + "\n")
        staging.rename(directory)
    return source, commit


def build(args, sdk, environment):
    gradle = gradle_command(args.gradle)
    version = subprocess.check_output([gradle, "--version"], env=environment, text=True)
    if not re.search(rf"^Gradle {re.escape(GRADLE_VERSION)}$", version, re.M):
        raise RuntimeError(f"F-Droid release builds require Gradle {GRADLE_VERSION}")
    if args.checkout:
        source = args.checkout.expanduser().resolve()
        if not (source / "FDROID-PREPARED.json").is_file():
            raise RuntimeError("--checkout must be an isolated checkout already prepared for F-Droid")
        commit = None
        if (source / ".git").exists():
            commit = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
    else:
        source, commit = checkout_snapshot(args.ref, args.work_dir)
    work_arguments = ["--gradle", gradle, "--sdk", str(sdk),
        "--pdfium-source-work", str(args.pdfium_source_work or args.work_dir / "native/pdfium"),
        "--speech-source-work", str(args.speech_source_work or args.work_dir / "native/speech")]
    prepare = [sys.executable, str(source / "scripts/fdroid/prefetch.py"), *work_arguments]
    if args.offline:
        prepare.append("--offline")
    subprocess.run(prepare, env=environment, check=True)
    command = [sys.executable, str(source / "scripts/fdroid/build.py"), *work_arguments,
        "--jobs", str(args.jobs), "--native-jobs", str(args.native_jobs)]
    subprocess.run(command, env=environment, check=True)
    return source / APK_PATH, commit


def payloads(apk):
    with zipfile.ZipFile(apk) as archive:
        entries = archive.namelist()
        if len(entries) != len(set(entries)):
            raise RuntimeError("APK has duplicate ZIP entries")
        result = {}
        for name in entries:
            if re.fullmatch(r"META-INF/(?:MANIFEST\.MF|[^/]+\.(?:SF|RSA|DSA|EC))", name, re.I):
                continue
            with archive.open(name) as stream:
                result[name] = hashlib.file_digest(stream, "sha256").hexdigest()
        return result


def release(apk, output, sdk, keystore, alias, environment, certificate, commit=None):
    apk = apk.resolve()
    output = output.resolve()
    if output.exists():
        raise RuntimeError("Release output already exists; choose a new --output-dir")
    output.parent.mkdir(parents=True, exist_ok=True)
    verifier = runpy.run_path(str(ROOT / "apps/desktop-gpui/android/verify-fdroid.py"))["verify"]
    tools = sdk / "build-tools" / BUILD_TOOLS
    with tempfile.TemporaryDirectory(prefix=".fdroid-sign-", dir=output.parent) as temporary:
        stage = Path(temporary)
        unsigned = stage / "Bokheim-fdroid-arm64-unsigned.apk"
        signed = stage / "Bokheim-fdroid-arm64.apk"
        shutil.copyfile(apk, unsigned)
        verification_environment = {name: value for name, value in environment.items()
            if name not in {"BOKHEIM_FDROID_STORE_PASSWORD", "BOKHEIM_FDROID_KEY_PASSWORD"}}
        unsigned_report = verifier(unsigned, tools / "aapt", environment=verification_environment)
        expected_payload = payloads(unsigned)
        normalize(unsigned)
        unsigned_report = verifier(unsigned, tools / "aapt", environment=verification_environment)
        subprocess.run([str(sdk / "build-tools" / SIGNING_TOOLS / "apksigner"), "sign", "--ks", str(keystore), "--ks-key-alias", alias,
            "--ks-pass", "env:BOKHEIM_FDROID_STORE_PASSWORD", "--key-pass", "env:BOKHEIM_FDROID_KEY_PASSWORD",
            "--alignment-preserved", "true",
            "--v1-signing-enabled", "false", "--v2-signing-enabled", "true", "--v3-signing-enabled", "true",
            "--v4-signing-enabled", "false", "--out", str(signed), str(unsigned)],
            env=environment, stdin=subprocess.DEVNULL, check=True)
        signed_report = verifier(signed, tools / "aapt", certificate, environment=verification_environment)
        if expected_payload != payloads(signed):
            raise RuntimeError("Signing changed APK contents")
        write(stage / "unsigned-verification.json", json.dumps(unsigned_report, indent=2) + "\n")
        write(stage / "signed-verification.json", json.dumps(signed_report, indent=2) + "\n")
        write(stage / "checksums.sha256", "".join(f"{digest(path)}  {path.name}\n" for path in (unsigned, signed)))
        write(stage / "release.json", json.dumps({"schema": 1, "source_commit": commit,
            "signing_cert_sha256": certificate, "unsigned_apk_sha256": digest(unsigned), "signed_apk_sha256": digest(signed),
            "apk_payload_unchanged": True, "build_tools": BUILD_TOOLS, "signing_tools": SIGNING_TOOLS,
            "reproducibility": "not_verified"}, indent=2) + "\n")
        stage.rename(output)
    print(f"Signed and verified: {output / signed.name}")
    print(f"Signing certificate SHA-256: {certificate}")
    print(f"Checksums and verification reports: {output}")
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--apk", type=Path, help="Sign an existing unsigned F-Droid APK instead of building")
    mode.add_argument("--checkout", type=Path, help="Reuse an isolated prepared checkout instead of making a source snapshot")
    parser.add_argument("--ref", default="HEAD", help="Committed source to build; local uncommitted changes are excluded")
    parser.add_argument("--work-dir", type=Path, default=ROOT / ".tmp/fdroid-release")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--sdk", type=Path)
    parser.add_argument("--gradle", help="Gradle 8.14.3 executable; auto-detected from PATH or the wrapper cache")
    parser.add_argument("--jobs", type=int, default=2)
    parser.add_argument("--native-jobs", type=int, default=2)
    parser.add_argument("--pdfium-source-work", type=Path)
    parser.add_argument("--speech-source-work", type=Path)
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    if args.jobs < 1 or args.native_jobs < 1:
        parser.error("Compilation job counts must be positive")
    if args.output_dir and args.output_dir.exists():
        parser.error("Release output already exists; choose a new --output-dir")
    args.work_dir = args.work_dir.expanduser().resolve()
    for option in ("pdfium_source_work", "speech_source_work"):
        if getattr(args, option):
            setattr(args, option, getattr(args, option).expanduser().resolve())
    environment = java_environment(os.environ)
    sdk = sdk_directory(args.sdk, environment)
    if not (sdk / "build-tools" / SIGNING_TOOLS / "apksigner").is_file():
        parser.error(f"Developer signing requires Android SDK build-tools {SIGNING_TOOLS} for F-Droid signature copying")
    environment["ANDROID_HOME"] = str(sdk)
    keystore, alias, signing_environment = signing_settings(ROOT / "apps/desktop-gpui/android", environment)
    certificate = signing_certificate(keystore, alias, signing_environment)
    normalizer(args.offline)
    if args.apk:
        apk, commit = args.apk, None
    else:
        apk, commit = build(args, sdk, environment)
    output = args.output_dir or ROOT / "artifacts/fdroid-release" / datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    # Keep secrets scoped to the keystore and signer child processes.
    release(apk, output, sdk, keystore, alias, signing_environment, certificate, commit)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.CalledProcessError, zipfile.BadZipFile) as error:
        sys.exit(f"F-Droid release failed: {error}")
