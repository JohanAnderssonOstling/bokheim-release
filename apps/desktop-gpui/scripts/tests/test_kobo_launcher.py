"""Run the real launcher with fake firmware commands and a fake application."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class KoboLauncherTests(unittest.TestCase):
    def test_records_exit_status_and_enables_backtraces(self):
        launcher = Path(__file__).resolve().parents[2] / "kobo" / "run.sh"
        for status in (0, 7, 137):
            with self.subTest(status=status), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                shutil.copy2(launcher, root / "run.sh")
                fake_bin = root / "bin"
                fake_bin.mkdir()
                for name, code in (("pidof", 1), ("sleep", 0), ("sync", 0)):
                    path = fake_bin / name
                    path.write_text(f"#!/bin/sh\nexit {code}\n")
                    path.chmod(0o755)
                (root / "fbink").write_text("#!/bin/sh\nexit 0\n")
                (root / "desktop-gpui-kobo").write_text(
                    '#!/bin/sh\nprintf "backtrace=%s\\n" "$RUST_BACKTRACE"\n'
                    'printf "log_filter=%s\\n" "$RUST_LOG"\n'
                    f"exit {status}\n"
                )
                env = dict(os.environ, PATH=f"{fake_bin}:/usr/bin:/bin")
                env.pop("RUST_BACKTRACE", None)
                env.pop("RUST_LOG", None)
                result = subprocess.run(["/bin/sh", str(root / "run.sh")], env=env, timeout=5)
                self.assertEqual(result.returncode, status)
                log = (root / "desktop-gpui-kobo.log").read_text()
                self.assertIn(f"exited: status={status}", log)
                self.assertIn("backtrace=1", log)
                self.assertIn("log_filter=warn,book_startup=debug", log)
                if status == 137:
                    self.assertIn("Possible terminating signal: 9", log)
                if status == 0:
                    self.assertIn("Bokheim Kobo complete", log)


if __name__ == "__main__":
    unittest.main()
