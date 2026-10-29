"""Exercise the production shutdown script with isolated fake kernel/commands."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class WifiPowerTests(unittest.TestCase):
    def run_shutdown(self, failures, driver="8189fs", modules=None, platform="test", saved_driver=None, verify_restart=False):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "net/wlan0").mkdir(parents=True)
            (root / "modules").write_text("8189fs 0 0 - Live 0\nsdio_wifi_pwr 0 0 - Live 0\n" if modules is None else modules)
            (root / "attempts").write_text("0")
            if saved_driver is not None:
                (root / "wifi-module").write_text(saved_driver + "\n")
            source = Path(__file__).resolve().parents[2] / "kobo/wifi-power.sh"
            script = source.read_text().replace("/proc/modules", str(root / "modules")).replace("/sys/class/net/", str(root / "net") + "/")
            (root / "power.sh").write_text(script)
            ioctl = root / "desktop-gpui-kobo"
            ioctl.write_text('#!/bin/sh\necho "$*" > "$FIXTURE/ioctl"\n')
            ioctl.chmod(0o755)
            commands = root / "bin"
            commands.mkdir()
            for name in ("ifconfig", "sleep", "wlarm_le"):
                command = commands / name
                command.write_text("#!/bin/sh\nexit 0\n")
                command.chmod(0o755)
            control = commands / "wpa_cli"
            control.write_text('#!/bin/sh\n[ "$3" != ping ] || echo PONG\nexit 0\n')
            control.chmod(0o755)
            remove = commands / "rmmod"
            remove.write_text('''#!/bin/sh
if [ "$1" = 8189fs ]; then
    attempt=$(cat "$FIXTURE/attempts")
    attempt=$((attempt+1))
    echo "$attempt" > "$FIXTURE/attempts"
    if [ "$attempt" -le "$FAILURES" ]; then
        echo 'rmmod: module is in use' >&2
        exit 1
    fi
fi
awk -v module="$1" '$1 != module' "$FIXTURE/modules" > "$FIXTURE/modules.next"
mv "$FIXTURE/modules.next" "$FIXTURE/modules"
''')
            remove.chmod(0o755)
            env = dict(os.environ, PATH=f"{commands}:/usr/bin:/bin", INTERFACE="wlan0", PLATFORM=platform, FIXTURE=str(root), FAILURES=str(failures))
            env.pop("WIFI_MODULE", None)
            if driver is not None:
                env["WIFI_MODULE"] = driver
            result = subprocess.run(["/bin/sh", str(root / "power.sh"), "off"], env=env,
                capture_output=True, text=True, timeout=5)
            if verify_restart:
                self.assertEqual(result.returncode, 0, result.stderr)
                # The next invocation has no loaded module or inherited hint.
                # Exercise startup using the saved driver and fake firmware.
                module_file = root / "drivers/test/wifi/8723ds.ko"
                module_file.parent.mkdir(parents=True)
                module_file.touch()
                (root / "power.sh").write_text(script.replace("/drivers/", str(root / "drivers") + "/"))
                insert = commands / "insmod"
                insert.write_text('#!/bin/sh\necho "$(basename "$1" .ko) 0 0 - Live 0" >> "$FIXTURE/modules"\n')
                insert.chmod(0o755)
                restart = subprocess.run(["/bin/sh", str(root / "power.sh"), "on"], env=env,
                    capture_output=True, text=True, timeout=5)
                self.assertEqual(restart.returncode, 0, restart.stderr)
                self.assertIn("8723ds", (root / "modules").read_text())
                # Leave the fixture off again, as in the normal test path.
                result = subprocess.run(["/bin/sh", str(root / "power.sh"), "off"], env=env,
                    capture_output=True, text=True, timeout=5)
            if driver == "moal" and result.returncode == 0:
                self.assertEqual((root / "ioctl").read_text().strip(), "--wifi-power-ioctl 0")
            else:
                self.assertFalse((root / "ioctl").exists())
            return result, (root / "modules").read_text(), int((root / "attempts").read_text())

    def test_waits_for_driver_to_be_released_before_power_off(self):
        result, modules, attempts = self.run_shutdown(2)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(attempts, 3)
        self.assertEqual(modules, "")

    def test_permanently_busy_driver_has_bounded_failure_and_diagnostics(self):
        result, modules, attempts = self.run_shutdown(100)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(attempts, 20)
        self.assertIn("sdio_wifi_pwr", modules)
        self.assertIn("failed at unload 8189fs", result.stderr)

    def test_already_off_module_radio_does_not_use_ioctl(self):
        result, modules, attempts = self.run_shutdown(0, modules="")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((modules, attempts), ("", 0))

    def test_shutdown_does_not_require_platform(self):
        result, modules, _ = self.run_shutdown(0, platform="")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(modules, "")

    def test_moal_uses_ioctl_after_unloading_both_modules(self):
        result, modules, _ = self.run_shutdown(0, driver="moal", modules="moal 0 0 - Live 0\nmlan 0 0 - Live 0\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(modules, "")

    def test_8723ds_is_supported(self):
        result, modules, _ = self.run_shutdown(0, driver="8723ds", modules="8723ds 0 0 - Live 0\nsdio_wifi_pwr 0 0 - Live 0\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(modules, "")

    def test_missing_nickel_hint_detects_driver_and_restores_it_on_wake(self):
        result, modules, _ = self.run_shutdown(0, driver=None,
            modules="8723ds 0 0 - Live 0\nsdio_wifi_pwr 0 0 - Live 0\n", verify_restart=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(modules, "")

    def test_loaded_driver_takes_precedence_over_saved_hint(self):
        result, modules, _ = self.run_shutdown(0, driver=None, saved_driver="moal")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(modules, "")

    def test_already_off_radio_uses_saved_driver_without_nickel_hint(self):
        result, modules, _ = self.run_shutdown(0, driver=None, modules="", saved_driver="8723ds")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(modules, "")

    def test_ambiguous_or_unknown_drivers_abort_before_changing_radio(self):
        for modules, saved, hint in [
            ("8189fs 0 0 - Live 0\n8723ds 0 0 - Live 0\n", None, None),
            ("", None, None),
            ("", "../unsupported", None),
            ("8189fs 0 0 - Live 0\n", None, "unsupported"),
        ]:
            with self.subTest(modules=modules, saved=saved, hint=hint):
                result, remaining, attempts = self.run_shutdown(0, driver=hint, modules=modules, saved_driver=saved)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(remaining, modules)
                self.assertEqual(attempts, 0)


if __name__ == "__main__":
    unittest.main()
