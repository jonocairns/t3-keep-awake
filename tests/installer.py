#!/usr/bin/env python3
"""Exercise piped installers without touching real Windows or user services."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
ASSET = "t3-keep-awake-linux-x86_64"
MOCK = r'''#!/usr/bin/python3
import json
import os
from pathlib import Path
import shutil
import sys

name = Path(sys.argv[0]).name
args = sys.argv[1:]
with open(os.environ["TEST_LOG"], "a") as log:
    log.write(json.dumps([name, *args]) + "\n")
if name == "uname":
    print({"-s": "Linux", "-r": os.environ.get("TEST_KERNEL", "microsoft-standard-WSL2"),
           "-m": os.environ.get("TEST_ARCH", "x86_64")}[args[0]])
elif name == "systemctl":
    if "show-environment" in args and os.environ.get("TEST_NO_SYSTEMD"):
        sys.exit(1)
elif name == "gh":
    if args[:2] == ["auth", "status"]:
        sys.exit(0 if os.environ.get("TEST_GH") else 1)
    elif args[:2] == ["release", "view"]:
        print("v0.1.0")
    elif args[:2] == ["release", "download"]:
        dest = Path(args[args.index("--dir") + 1])
        for asset in Path(os.environ["TEST_ASSETS"]).iterdir():
            shutil.copyfile(asset, dest / asset.name)
    else:
        sys.exit(1)
elif name == "curl":
    if os.environ.get("TEST_DOWNLOAD_FAIL"):
        sys.exit(22)
    if args[-1].endswith("/latest"):
        print("https://github.com/jonocairns/t3-keep-awake/releases/tag/v0.1.0", end="")
    else:
        url = args[args.index("-o") - 1]
        dest = args[args.index("-o") + 1]
        shutil.copyfile(Path(os.environ["TEST_ASSETS"]) / url.rsplit("/", 1)[-1], dest)
'''
BINARY = '''#!/bin/sh
printf 'binary %s\n' "$1" >> "$TEST_LOG"
case "$1" in
    help|stop) exit 0 ;;
    status) echo "fake daemon ready" ;;
    *) exit 1 ;;
esac
'''


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.home = self.base / "home with spaces"
        self.home.mkdir()
        self.commands = self.base / "commands"
        self.commands.mkdir()
        for name in ("uname", "gh", "curl", "systemctl", "loginctl", "powershell.exe"):
            path = self.commands / name
            path.write_text(MOCK)
            path.chmod(0o755)
        self.assets = self.base / "assets"
        self.assets.mkdir()
        self.binary = self.assets / ASSET
        self.binary.write_text(BINARY)
        checksum = subprocess.check_output(["sha256sum", str(self.binary)], text=True).split()[0]
        (self.assets / f"{ASSET}.sha256").write_text(f"{checksum}  {ASSET}\n")
        self.log = self.base / "commands.log"
        self.log.touch()
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("T3_KEEP_AWAKE_", "XDG_", "TEST_"))}
        self.env.update(HOME=str(self.home), PATH=f"{self.commands}:/usr/bin:/bin",
                        XDG_CONFIG_HOME=str(self.home / "custom config"),
                        TEST_LOG=str(self.log), TEST_ASSETS=str(self.assets))
        self.installed = self.home / ".local/bin/t3-keep-awake"
        self.unit = Path(self.env["XDG_CONFIG_HOME"]) / "systemd/user/t3-keep-awake.service"

    def run_script(self, script="install.sh", success=True):
        result = subprocess.run(["sh"], input=(ROOT / script).read_text(), env=self.env,
                                text=True, capture_output=True, timeout=15)
        if success:
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
        return result

    def events(self):
        return [json.loads(line) if line.startswith("[") else line
                for line in self.log.read_text().splitlines()]

    def old_install(self):
        self.installed.parent.mkdir(parents=True)
        old = self.base / "checkout-binary"
        old.write_text("original checkout binary\n")
        self.installed.symlink_to(old)
        self.unit.parent.mkdir(parents=True)
        self.unit.write_text("old unit\n")
        return old

    def assert_untouched(self, old):
        self.assertTrue(self.installed.is_symlink())
        self.assertEqual(old.read_text(), "original checkout binary\n")
        self.assertEqual(self.unit.read_text(), "old unit\n")
        self.assertNotIn("binary stop", self.events())
        self.assertFalse(any(event[0] == "loginctl" for event in self.events()
                             if isinstance(event, list)))

    def test_public_piped_install_and_repeat_update_replace_old_symlink(self):
        old = self.old_install()
        config = self.home / "custom config/t3-keep-awake/config.toml"
        config.parent.mkdir()
        config.write_text("grace_secs = 5\n")
        self.run_script()
        self.assertFalse(self.installed.is_symlink())
        self.assertEqual(self.installed.read_text(), BINARY)
        self.assertTrue(os.access(self.installed, os.X_OK))
        self.assertEqual(old.read_text(), "original checkout binary\n")
        self.assertIn("ExecStart=%h/.local/bin/t3-keep-awake daemon", self.unit.read_text())
        self.assertIn("KillMode=mixed", self.unit.read_text())
        urls = [part for event in self.events() if isinstance(event, list)
                and event[0] == "curl" for part in event if part.startswith("https://")]
        self.assertEqual(len(urls), 3)
        self.assertTrue(all("/download/v0.1.0/" in url for url in urls[1:]))
        self.run_script()
        self.assertEqual(config.read_text(), "grace_secs = 5\n")
        self.assertIn(["systemctl", "--user", "restart", "t3-keep-awake.service"], self.events())

    def test_private_authenticated_release_download(self):
        self.env["TEST_GH"] = "1"
        self.run_script()
        events = self.events()
        download = next(event for event in events if isinstance(event, list)
                        and event[:3] == ["gh", "release", "download"])
        self.assertEqual(download[3], "v0.1.0")
        self.assertIn(ASSET, download)
        self.assertIn(f"{ASSET}.sha256", download)
        self.assertFalse(any(event[0] == "curl" for event in events if isinstance(event, list)))

    def test_pinned_version_skips_latest_lookup(self):
        self.env["T3_KEEP_AWAKE_VERSION"] = "v0.1.0"
        self.run_script()
        self.assertFalse(any("/releases/latest" in part for event in self.events()
                             if isinstance(event, list) for part in event))

    def test_checksum_failure_preserves_running_install(self):
        old = self.old_install()
        self.binary.write_text(BINARY + "# modified after checksumming\n")
        result = self.run_script(success=False)
        self.assertIn("checksum did not match", result.stderr)
        self.assert_untouched(old)

    def test_download_failure_preserves_running_install(self):
        old = self.old_install()
        self.env["TEST_DOWNLOAD_FAIL"] = "1"
        self.run_script(success=False)
        self.assert_untouched(old)

    def test_systemd_required_before_download(self):
        self.env["TEST_NO_SYSTEMD"] = "1"
        result = self.run_script(success=False)
        self.assertIn("systemd user manager", result.stderr)
        self.assertFalse(any(event[0] in ("gh", "curl", "loginctl") for event in self.events()
                             if isinstance(event, list)))

    def test_non_wsl_rejected(self):
        self.env["TEST_KERNEL"] = "ordinary-linux"
        result = self.run_script(success=False)
        self.assertIn("inside WSL2", result.stderr)
        self.assertFalse(self.installed.exists())

    def test_unsupported_release_architecture(self):
        self.env["TEST_ARCH"] = "aarch64"
        result = self.run_script(success=False)
        self.assertIn("building from source", result.stderr)

    def test_source_install_on_other_architecture(self):
        self.env.update(TEST_ARCH="aarch64", T3_KEEP_AWAKE_BINARY=str(self.binary))
        self.run_script()
        self.assertEqual(self.installed.read_text(), BINARY)
        self.assertFalse(any(event[0] in ("gh", "curl") for event in self.events()
                             if isinstance(event, list)))

    def test_uninstall_without_checkout_retains_config_and_logs(self):
        self.run_script()
        config = self.home / "custom config/t3-keep-awake/config.toml"
        state = self.home / ".local/state/t3-keep-awake/daemon.log"
        for path in (config, state):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("keep me\n")
        self.run_script("uninstall.sh")
        self.assertFalse(self.installed.exists())
        self.assertFalse(self.unit.exists())
        self.assertEqual(config.read_text(), "keep me\n")
        self.assertEqual(state.read_text(), "keep me\n")
        self.assertIn(["systemctl", "--user", "disable", "--now", "t3-keep-awake.service"], self.events())
        self.run_script("uninstall.sh")


if __name__ == "__main__":
    unittest.main(verbosity=2)
