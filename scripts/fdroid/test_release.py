import argparse
from pathlib import Path
import runpy
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import release


class FdroidReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def test_existing_gradle_credentials_and_environment_precedence(self):
        android = self.root / "android"
        home = self.root / "gradle-home"
        android.mkdir()
        home.mkdir()
        (android / "key.jks").touch()
        (android / "store.password").write_text("private-password\r\n")
        (android / "gradle.properties").write_text("BOKHEIM_ANDROID_KEY_ALIAS=project-alias\n")
        (home / "gradle.properties").write_text(
            "# existing private Gradle settings\n"
            "BOKHEIM_ANDROID_KEYSTORE=key.jks\n"
            "BOKHEIM_ANDROID_KEY_ALIAS=user-alias\n"
            "BOKHEIM_ANDROID_STORE_PASSWORD_FILE=store.password\n")
        environment = {"GRADLE_USER_HOME": str(home), "BOKHEIM_ANDROID_KEY_ALIAS": "environment-alias"}
        store, alias, child = release.signing_settings(android, environment)
        self.assertEqual(store, android / "key.jks")
        self.assertEqual(alias, "environment-alias")
        self.assertEqual(child["BOKHEIM_FDROID_STORE_PASSWORD"], "private-password")
        self.assertEqual(child["BOKHEIM_FDROID_KEY_PASSWORD"], "private-password")
        self.assertNotIn("BOKHEIM_FDROID_STORE_PASSWORD", environment)
        environment["BOKHEIM_ANDROID_KEY_PASSWORD"] = "different-key-password"
        self.assertEqual(release.signing_settings(android, environment)[2]["BOKHEIM_FDROID_KEY_PASSWORD"],
            "different-key-password")

    def test_java_property_escaping_preserves_private_paths_and_passwords(self):
        path = self.root / "gradle.properties"
        path.write_text("! comment\nkey\\ name : C\\:\\\\private\\\\key.jks\n"
            "password=p\\=ass\\:word\\\\end\ncontinued=first\\\n  second\nunicode=\\u00e5\n")
        self.assertEqual(release.properties(path), {"key name": "C:\\private\\key.jks",
            "password": "p=ass:word\\end", "continued": "firstsecond", "unicode": "å"})

    def test_wrong_expected_certificate_is_rejected_without_password_arguments(self):
        environment = {"BOKHEIM_FDROID_STORE_PASSWORD": "private-password", "JAVA_HOME": "/jdk",
            "BOKHEIM_ANDROID_CERT_SHA256": "ab" * 32}
        result = subprocess.CompletedProcess([], 0, stdout=b"different certificate", stderr=b"")
        with patch.object(release.subprocess, "run", return_value=result) as command:
            with self.assertRaisesRegex(RuntimeError, "expected production certificate"):
                release.signing_certificate(self.root / "private.jks", "production", environment)
        self.assertNotIn("private-password", " ".join(command.call_args.args[0]))
        self.assertEqual(command.call_args.kwargs["env"], environment)

    def test_keytool_failure_does_not_expose_its_output_or_password(self):
        result = subprocess.CompletedProcess([], 1, stdout=b"", stderr=b"private-password")
        with patch.object(release.subprocess, "run", return_value=result):
            with self.assertRaisesRegex(RuntimeError, "Cannot read the production signing certificate") as error:
                release.signing_certificate(self.root / "private.jks", "production", {"JAVA_HOME": "/jdk"})
        self.assertNotIn("private-password", str(error.exception))

    def test_fresh_snapshot_excludes_uncommitted_files_without_touching_developer_sources(self):
        repository = self.root / "repository"
        repository.mkdir()
        subprocess.run(["git", "init", "-q", str(repository)], check=True)
        script = repository / "scripts/fdroid/prepare_checkout.py"
        script.parent.mkdir(parents=True)
        script.write_text("# source preparation\n")
        source = repository / "code.rs"
        source.write_text("committed source\n")
        archived_books = repository / "shared/subject-projection/data/reports/20261015-literature-coverage-fix/publication-input/books"
        archived_books.parent.mkdir(parents=True)
        archived_books.symlink_to(self.root / "private-books")
        subprocess.run(["git", "-C", str(repository), "add", "."], check=True)
        subprocess.run(["git", "-C", str(repository), "-c", "user.name=Release Test",
            "-c", "user.email=test@example.invalid", "commit", "-qm", "Source snapshot"], check=True)
        source.write_text("uncommitted source\n")
        (repository / "private-untracked-file").write_text("untracked\n")
        with patch.object(release, "ROOT", repository):
            snapshot, commit = release.checkout_snapshot("HEAD", self.root / "work")
        self.assertEqual((snapshot / "code.rs").read_text(), "committed source\n")
        self.assertFalse((snapshot / "private-untracked-file").exists())
        self.assertFalse((snapshot / archived_books.relative_to(repository)).is_symlink())
        self.assertTrue(archived_books.is_symlink())
        self.assertEqual(source.read_text(), "uncommitted source\n")
        self.assertEqual(len(commit), 40)

    def test_build_requires_prepared_checkout_and_enforces_fdroid_mode(self):
        checkout = self.root / "checkout"
        checkout.mkdir()
        args = argparse.Namespace(gradle="gradle", checkout=checkout, offline=False,
            jobs=2, native_jobs=2, pdfium_source_work=None, speech_source_work=None,
            work_dir=self.root / "work")
        with patch.object(release, "gradle_command", return_value="gradle"), \
                patch.object(release.subprocess, "check_output", return_value="Gradle 8.14.3\n"), \
                patch.object(release.subprocess, "run") as run:
            with self.assertRaisesRegex(RuntimeError, "isolated checkout already prepared"):
                release.build(args, self.root / "sdk", {})
            run.assert_not_called()
            (checkout / "FDROID-PREPARED.json").write_text("{}\n")
            apk, commit = release.build(args, self.root / "sdk", {})
            command = run.call_args.args[0]
            self.assertEqual(Path(command[1]).name, "build.py")
            self.assertIn("--pdfium-source-work", command)
            self.assertIn("--speech-source-work", command)
            self.assertFalse(any("localSigning" in value or "googleDrive" in value for value in command))
            self.assertEqual(apk, checkout / release.APK_PATH)
            self.assertIsNone(commit)

    def test_no_release_is_created_when_unsigned_apk_verification_fails(self):
        apk = self.root / "unsigned.apk"
        apk.write_bytes(b"invalid APK")
        output = self.root / "output"
        def reject(*args, **kwargs):
            raise RuntimeError("F-Droid policy check failed")
        with patch.object(release.runpy, "run_path", return_value={"verify": reject}), \
                patch.object(release.subprocess, "run") as signer:
            with self.assertRaisesRegex(RuntimeError, "policy check"):
                release.release(apk, output, self.root / "sdk", self.root / "private.jks", "production", {}, "ab" * 32)
            signer.assert_not_called()
        self.assertFalse(output.exists())
        self.assertEqual(list(self.root.glob(".fdroid-sign-*")), [])

    def test_signing_cannot_change_payload_and_failed_output_is_removed(self):
        apk = self.root / "unsigned.apk"
        with zipfile.ZipFile(apk, "w") as archive:
            archive.writestr("classes.dex", b"original bytecode")
        output = self.root / "output"
        def corrupt(command, **kwargs):
            destination = Path(command[command.index("--out") + 1])
            with zipfile.ZipFile(destination, "w") as archive:
                archive.writestr("classes.dex", b"replaced bytecode")
        with patch.object(release.runpy, "run_path", return_value={"verify": lambda *a, **kw: {}}), \
                patch.object(release, "normalize"), \
                patch.object(release.subprocess, "run", side_effect=corrupt):
            with self.assertRaisesRegex(RuntimeError, "changed APK contents"):
                release.release(apk, output, self.root / "sdk", self.root / "private.jks", "production", {}, "ab" * 32)
        self.assertFalse(output.exists())

    def test_signed_apk_verifier_rejects_a_different_certificate(self):
        verifier = runpy.run_path(str(release.ROOT / "apps/desktop-gpui/android/verify-fdroid.py"))["verify"]
        signature = f'Signer #1 certificate SHA-256 digest: {"cd" * 32}\n'
        with patch.object(subprocess, "check_output", return_value=signature):
            with self.assertRaisesRegex(RuntimeError, "signing certificate mismatch"):
                verifier(self.root / "signed.apk", self.root / "sdk/aapt", "ab" * 32)

    def test_duplicate_zip_entries_are_rejected(self):
        apk = self.root / "duplicate.apk"
        with zipfile.ZipFile(apk, "w") as archive:
            archive.writestr("classes.dex", b"original")
            with self.assertWarns(UserWarning):
                archive.writestr("classes.dex", b"replacement")
        with self.assertRaisesRegex(RuntimeError, "duplicate ZIP entries"):
            release.payloads(apk)


if __name__ == "__main__":
    unittest.main()
