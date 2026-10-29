"""Run the real launcher with fake firmware commands and a fake application."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest


class KoboLauncherTests(unittest.TestCase):
    def test_normal_launch_skips_resource_sampling_and_preserves_exit_status(self):
        source = Path(__file__).resolve().parents[2] / "kobo" / "run-monitored.sh"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            launcher = root / "run-monitored.sh"
            launcher.write_text(source.read_text().replace(
                "WORKDIR=/mnt/onboard/.adds/bokheim", f"WORKDIR='{root}'"))
            (root / "run.sh").write_text('#!/bin/sh\nprintf launched > "$0.started"\nexit 7\n')
            env = dict(os.environ, BOKHEIM_KOBO_RESOURCE_MONITOR="0")
            result = subprocess.run(["/bin/sh", str(launcher)], env=env, timeout=3)
            self.assertEqual(result.returncode, 7)
            self.assertEqual((root / "run.sh.started").read_text(), "launched")
            self.assertFalse((root / "bokheim-resource.log").exists())

    def test_uninstall_removes_program_after_exit_and_preserves_user_data(self):
        launcher = Path(__file__).resolve().parents[2] / "kobo" / "run.sh"
        for action in ("uninstall", ""):
            with self.subTest(action=action), tempfile.TemporaryDirectory() as directory:
                device = Path(directory)
                root = device / ".adds/bokheim"
                root.mkdir(parents=True)
                shutil.copy2(launcher, root / "run.sh")
                fake_bin = device / "bin"
                fake_bin.mkdir()
                for name, code in (("pidof", 1), ("sync", 0)):
                    path = fake_bin / name
                    path.write_text(f'#!/bin/sh\nexit {code}\n')
                    path.chmod(0o755)
                program = root / "desktop-gpui-kobo"
                program.write_text(
                    '#!/bin/sh\n'
                    '[ "$BOKHEIM_KOBO_CAN_UNINSTALL" = 1 ] || exit 9\n'
                    # Removal must happen only after the app has closed its data.
                    f'printf "%s\\n" "{action}" > "$BOKHEIM_KOBO_EXIT_ACTION_FILE"\n'
                    f'test -f "{root}/runtime/libc.so.6" || exit 9\n'
                    'exit 0\n'
                )
                (root / "fbink").write_text('#!/bin/sh\nexit 0\n')
                installed = [
                    ".adds/bokheim/run.sh", ".adds/bokheim/desktop-gpui-kobo", ".adds/bokheim/fbink",
                    ".adds/bokheim/runtime/libc.so.6", ".adds/bokheim/pdfium/libpdfium.so",
                    ".adds/bokheim/taxonomy.sqlite3", ".adds/bokheim/command-shims/bluetoothctl",
                    ".adds/bokheim/nickelmenu-icons/bokheim.png", ".adds/bokheim/nickelmenu-icons/bokheim-active.png",
                    ".adds/nm/desktop-gpui-kobo",
                ]
                preserved = [
                    "Bokheim/Libraries/default/book.epub", "Bokheim/Libraries/default/.bokheim/library.db",
                    ".adds/bokheim/data/app-state-v1.sqlite3", ".adds/bokheim/app-state-v1.sqlite3",
                    ".adds/bokheim/reader_preferences.json", ".adds/bokheim/remote-book-cache/book.epub",
                    ".adds/bokheim/libraries/book.epub", ".adds/bokheim/taxonomy/custom.sqlite3",
                    ".adds/bokheim/ssh/start.sh", ".adds/nm/other-app", ".adds/nm/doc", ".kobo/KoboReader.sqlite",
                ]
                for relative in installed + preserved:
                    path = device / relative
                    path.parent.mkdir(parents=True, exist_ok=True)
                    if not path.exists():
                        path.write_text(relative)
                env = dict(os.environ, PATH=f"{fake_bin}:/usr/bin:/bin")
                result = subprocess.run(["/bin/sh", str(root / "run.sh")], env=env, timeout=5)
                self.assertEqual(result.returncode, 0)
                for relative in installed:
                    self.assertEqual((device / relative).exists(), action != "uninstall", relative)
                for relative in preserved:
                    self.assertEqual((device / relative).read_text(), relative)
                self.assertFalse((root / "exit-action").exists())
                if action:
                    self.assertIn("Bokheim uninstalled", (root / "desktop-gpui-kobo.log").read_text())

    def test_firmware_timeout_preserves_fast_command_status(self):
        helper = Path(__file__).resolve().parents[2] / "kobo" / "command-timeout.sh"
        started = time.monotonic()
        result = subprocess.run(["sh", str(helper), "10", "sh", "-c", "printf ready; exit 7"], capture_output=True, timeout=3)
        self.assertEqual(result.returncode, 7)
        self.assertEqual(result.stdout, b"ready")
        self.assertLess(time.monotonic() - started, 3)

    def test_firmware_timeout_ends_an_unresponsive_diagnostic(self):
        helper = Path(__file__).resolve().parents[2] / "kobo" / "command-timeout.sh"
        result = subprocess.run(["sh", str(helper), "1", "sh", "-c", "exec /bin/sleep 20"], capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 137)

    def test_records_exit_status_and_enables_backtraces(self):
        launcher = Path(__file__).resolve().parents[2] / "kobo" / "run.sh"
        for status in (0, 7, 137, 143):
            with self.subTest(status=status), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                shutil.copy2(launcher, root / "run.sh")
                fake_bin = root / "bin"
                fake_bin.mkdir()
                for name, code in (("pidof", 1), ("sleep", 0), ("sync", 0)):
                    path = fake_bin / name
                    path.write_text(
                        '#!/bin/sh\nprintf "%s\\n" "$LD_LIBRARY_PATH" '
                        f'> "{root}/firmware-runtime"\nexit {code}\n'
                    )
                    path.chmod(0o755)
                (root / "fbink").write_text(f'#!/bin/sh\nprintf "%s\\n" "$@" >> "{root}/display-messages"\nexit 0\n')
                (root / "desktop-gpui-kobo").write_text(
                    '#!/bin/sh\nprintf "backtrace=%s\\n" "$RUST_BACKTRACE"\n'
                    'printf "log_filter=%s\\n" "$RUST_LOG"\n'
                    'printf "loader_path=%s\\n" "$LD_LIBRARY_PATH"\n'
                    f"exit {status}\n"
                )
                env = dict(os.environ, PATH=f"{fake_bin}:/usr/bin:/bin", LD_LIBRARY_PATH="/firmware/runtime")
                env.pop("RUST_BACKTRACE", None)
                env.pop("RUST_LOG", None)
                result = subprocess.run(["/bin/sh", str(root / "run.sh")], env=env, timeout=5)
                self.assertEqual(result.returncode, 0 if status == 143 else status)
                log = (root / "desktop-gpui-kobo.log").read_text()
                self.assertIn(f"exited: status={status}", log)
                self.assertIn("backtrace=1", log)
                self.assertIn("log_filter=warn,book_startup=debug", log)
                self.assertIn("loader_path=\n", log)
                self.assertEqual((root / "firmware-runtime").read_text().strip(), "/firmware/runtime")
                self.assertEqual("Bokheim Kobo failed" in (root / "display-messages").read_text(), status in (7, 137))
                if status == 137:
                    self.assertIn("Possible terminating signal: 9", log)
                if status == 0:
                    self.assertIn("Bokheim Kobo complete", log)


if __name__ == "__main__":
    unittest.main()
