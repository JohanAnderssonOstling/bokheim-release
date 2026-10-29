import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import build
import canonical


class OfflineBuildTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / "bokheim"
        self.root.mkdir()

    def test_build_requires_prefetch_and_disables_downloads_and_binary_caches(self):
        tools = self.root / "scripts/fdroid"
        tools.mkdir(parents=True)
        pins = {"rust": "1.99.1", "ndk": "29.0.14206865", "source_date_epoch": 1793023253}
        (tools / "toolchains.json").write_text(json.dumps(pins))
        argv = ["build.py", "--pdfium-source-work", str(self.root / "pdfium"),
            "--speech-source-work", str(self.root / "speech")]
        with patch.object(build, "ROOT", self.root), patch("sys.argv", argv), \
                patch.object(build, "CANONICAL_ROOT", self.root), \
                patch.object(build, "java_environment", return_value={}), \
                patch.object(build, "sdk_directory", return_value=self.root / "sdk"), \
                patch.object(build, "gradle_command", return_value="gradle"), \
                patch.object(build, "normalize"), patch.object(build.subprocess, "run") as run:
            with self.assertRaises(SystemExit):
                build.main()
            run.assert_not_called()
            (self.root / "FDROID-PREFETCHED.json").write_text("{}")
            build.main()
            command = run.call_args.args[0]
            for flag in ("--offline", "--no-build-cache", "-Pbokheim.fdroid=true"):
                self.assertIn(flag, command)
            env = run.call_args.kwargs["env"]
            self.assertEqual(env["RUSTUP_TOOLCHAIN"], pins["rust"])
            self.assertEqual(env["CARGO_INCREMENTAL"], "0")
            self.assertEqual(env["RUSTC_WRAPPER"], "")

    def test_native_dependency_sources_are_recreated_from_pinned_archives(self):
        path = Path(__file__).resolve().parents[2] / "shared/reader-speech/build_android_source.py"
        spec = importlib.util.spec_from_file_location("speech_source", path)
        speech = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(speech)
        (self.root / "source.json").write_text("{}")
        pins = {"dependencies": [{"name": "example", "sha256": "unused-in-this-staging-test"}]}
        (self.root / "source-dependencies.json").write_text(json.dumps(pins))
        archive = self.root / "downloads/example.archive"
        archive.parent.mkdir()
        with zipfile.ZipFile(archive, "w") as stream:
            stream.writestr("example/source.cpp", "pristine source")
        with patch.object(speech, "PINS", self.root / "source.json"):
            flags = speech.dependency_sources(self.root)
            source = self.root / "build/_deps/example-src/source.cpp"
            source.write_text("previous CMake patch")
            speech.dependency_sources(self.root)
        self.assertEqual(source.read_text(), "pristine source")
        self.assertIn("-DFETCHCONTENT_FULLY_DISCONNECTED=ON", flags)

    def test_source_prefetch_rejects_tampered_dependency_archive(self):
        path = Path(__file__).resolve().parents[2] / "shared/reader-speech/build_android_source.py"
        spec = importlib.util.spec_from_file_location("speech_source", path)
        speech = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(speech)
        dependencies = {"dependencies": [{"name": "example", "url": "https://invalid.example/source",
            "sha256": hashlib.sha256(b"original").hexdigest()}]}
        dep_file = self.root / "source-dependencies.json"
        dep_file.write_text(json.dumps(dependencies))
        (self.root / "sherpa.tar.gz").write_bytes(b"cached main source archive")
        pins = {"source_url": "https://invalid.example/main", "source_sha256": hashlib.sha256(b"cached main source archive").hexdigest(),
            "dependencies_manifest_sha256": hashlib.sha256(dep_file.read_bytes()).hexdigest()}
        (self.root / "source.json").write_text(json.dumps(pins))
        (self.root / "downloads").mkdir()
        (self.root / "downloads/example.archive").write_bytes(b"tampered")
        with patch.object(speech, "PINS", self.root / "source.json"), \
                patch.object(speech, "extract", return_value=self.root / "source"):
            with self.assertRaisesRegex(RuntimeError, "Cached input checksum mismatch"):
                speech.prefetch_sources(self.root, offline=True)

    @unittest.skipUnless(shutil.which("bwrap") and shutil.which("cargo"), "Requires Linux bubblewrap and Rust")
    def test_external_dependency_type_identity_matches_across_checkout_paths(self):
        identities = []
        for name in ("first", "second"):
            parent = self.root / name
            app = parent / "arbitrary-checkout-name"
            dependency = parent / "dependency"
            for directory in (app / "src", app / "scripts/fdroid", dependency / "src"):
                directory.mkdir(parents=True)
            (app / "Cargo.toml").write_text('[package]\nname="identity-probe"\nversion="0.1.0"\nedition="2021"\n'
                '[workspace]\n[dependencies]\nexternal={path="../dependency"}\n')
            (app / "src/main.rs").write_text('fn main() { println!("{:?}", std::any::TypeId::of::<external::Example>()); }')
            (dependency / "Cargo.toml").write_text('[package]\nname="external"\nversion="0.1.0"\nedition="2021"\n')
            (dependency / "src/lib.rs").write_text("pub struct Example;")
            (app / "scripts/fdroid/sources.json").write_text(json.dumps({"sources": [{"name": "dependency"}]}))
            environment = dict(os.environ, RUSTC_WRAPPER="", CARGO_TARGET_DIR=str(canonical.CANONICAL_ROOT / "target"))
            result = subprocess.run(canonical.command(app, [shutil.which("cargo"), "run", "--offline", "--quiet"]),
                env=environment, check=True, capture_output=True, text=True)
            identities.append(result.stdout)
        self.assertEqual(identities[0], identities[1])


if __name__ == "__main__":
    unittest.main()
