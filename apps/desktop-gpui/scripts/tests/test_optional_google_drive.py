"""Check that packaging opts into Drive only when explicitly requested."""
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[4]
spec = importlib.util.spec_from_file_location("desktop_build", ROOT / "apps/desktop-gpui/scripts/build.py")
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)


class OptionalGoogleDriveTests(unittest.TestCase):
    def build_command(self, target, *options):
        prepare = Mock(return_value=Path("/prepared/pdfium"))
        prepare_speech = Mock()

        def load_script(path):
            if path.endswith("toolchain.py"):
                return {"build_environment": Mock(return_value={})}
            if path.endswith("reader-speech/prepare.py"):
                return {"prepare_all": prepare_speech}
            return {"prepare": prepare}

        with patch("sys.argv", ["build.py", "--target", target, *options]), \
             patch.object(build.runpy, "run_path", side_effect=load_script), \
             patch.object(build.subprocess, "run") as run:
            build.main()
        return next(call.args[0] for call in run.call_args_list if call.args[0][:2] == ["cargo", "build"])

    def test_default_and_release_builds_exclude_drive_on_each_desktop_platform(self):
        for target in ("x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-pc-windows-msvc"):
            for options in ((), ("--no-google-drive",)):
                with self.subTest(target=target, options=options):
                    command = self.build_command(target, *options)
                    self.assertIn("--no-default-features", command)
                    self.assertEqual(command[command.index("--features") + 1], "desktop")

    def test_explicit_opt_in_preserves_platform_features(self):
        for options, platform_feature in (((), "desktop"), (("--flatpak",), "flatpak")):
            with self.subTest(options=options):
                command = self.build_command("x86_64-unknown-linux-gnu", "--google-drive", *options)
                self.assertEqual(set(command[command.index("--features") + 1].split(",")), {platform_feature, "google-drive"})


if __name__ == "__main__":
    unittest.main()
