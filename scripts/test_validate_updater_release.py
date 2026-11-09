#!/usr/bin/env python3

import base64
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
VALIDATOR = ROOT / "scripts" / "validate_updater_release.py"
REPOSITORY = "example/Codex-X"
RELEASE_TAG = "v0.3.1"
VERSION = "0.3.1"
PLATFORM_ASSETS = {
    "darwin-aarch64": "Codex-X.app.tar.gz",
    "darwin-aarch64-app": "Codex-X.app.tar.gz",
    "darwin-x86_64": "Codex-X-intel.app.tar.gz",
    "darwin-x86_64-app": "Codex-X-intel.app.tar.gz",
    "windows-x86_64": "Codex-X.msi",
    "windows-x86_64-nsis": "Codex-X.msi",
    "windows-x86_64-msi": "Codex-X.msi",
    "linux-x86_64": "Codex-X.AppImage",
    "linux-x86_64-deb": "Codex-X.deb",
    "linux-x86_64-rpm": "Codex-X.rpm",
    "linux-x86_64-appimage": "Codex-X.AppImage",
}


def draft_url(asset_name: str) -> str:
    return (
        "https://github.com/example/Codex-X/releases/download/"
        f"untagged-test/{asset_name}"
    )


class ValidateUpdaterReleaseTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.root = Path(self.temp_dir.name)
        self.manifest_path = self.root / "latest.json"
        self.assets_path = self.root / "assets.json"

        signature = base64.b64encode(b"s" * 64).decode("ascii")
        manifest = {
            "version": VERSION,
            "platforms": {
                platform: {
                    "signature": signature,
                    "url": draft_url(asset_name),
                }
                for platform, asset_name in PLATFORM_ASSETS.items()
            },
        }
        asset_names = sorted(set(PLATFORM_ASSETS.values()))
        assets = [
            {
                "name": asset_name,
                "url": f"https://api.github.com/assets/{index}",
                "browser_download_url": draft_url(asset_name),
            }
            for index, asset_name in enumerate(asset_names, start=1)
        ]
        assets.append(
            {
                "name": "latest.json",
                "url": "https://api.github.com/assets/latest",
                "browser_download_url": draft_url("latest.json"),
            }
        )
        self.manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        self.assets_path.write_text(json.dumps(assets), encoding="utf-8")

    def run_validator(self, *extra_args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(VALIDATOR),
                "--manifest",
                str(self.manifest_path),
                "--assets",
                str(self.assets_path),
                "--version",
                VERSION,
                "--repository",
                REPOSITORY,
                "--release-tag",
                RELEASE_TAG,
                *extra_args,
            ],
            check=False,
            capture_output=True,
            text=True,
        )

    def test_rewrites_draft_urls_to_stable_release_tag(self) -> None:
        result = self.run_validator("--rewrite-download-urls")
        self.assertEqual(result.returncode, 0, result.stderr)

        manifest = json.loads(self.manifest_path.read_text(encoding="utf-8"))
        urls = {entry["url"] for entry in manifest["platforms"].values()}
        self.assertTrue(urls)
        self.assertTrue(
            all("/releases/download/v0.3.1/" in url for url in urls),
            urls,
        )
        self.assertTrue(all("untagged-" not in url for url in urls), urls)

    def test_rejects_draft_urls_without_rewrite(self) -> None:
        result = self.run_validator()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("stable release tag download URL", result.stderr)

    def test_repairs_stale_urls_after_release_is_published(self) -> None:
        assets = json.loads(self.assets_path.read_text(encoding="utf-8"))
        for asset in assets:
            asset["browser_download_url"] = (
                f"https://github.com/{REPOSITORY}/releases/download/"
                f"{RELEASE_TAG}/{asset['name']}"
            )
        self.assets_path.write_text(json.dumps(assets), encoding="utf-8")

        result = self.run_validator("--rewrite-download-urls")
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = json.loads(self.manifest_path.read_text(encoding="utf-8"))
        self.assertTrue(
            all(
                "/releases/download/v0.3.1/" in entry["url"]
                for entry in manifest["platforms"].values()
            )
        )

    def test_restores_all_windows_targets_without_changing_other_platforms(self) -> None:
        manifest = json.loads(self.manifest_path.read_text())
        for key in ("windows-x86_64", "windows-x86_64-nsis"):
            manifest["platforms"][key]["url"] = draft_url("withdrawn-installer.exe")
            manifest["platforms"][key]["signature"] = base64.b64encode(b"e" * 64).decode()
        original = manifest["platforms"]
        self.manifest_path.write_text(json.dumps(manifest))
        result = self.run_validator("--rewrite-download-urls", "--restore-windows-msi")
        self.assertEqual(result.returncode, 0, result.stderr)
        actual = json.loads(self.manifest_path.read_text())["platforms"]
        self.assertEqual(actual["windows-x86_64"], actual["windows-x86_64-msi"])
        self.assertEqual(actual["windows-x86_64-msi"], actual["windows-x86_64-nsis"])
        for key, value in original.items():
            if not key.startswith("windows-"):
                self.assertEqual(actual[key]["signature"], value["signature"])
                self.assertEqual(actual[key]["url"].rsplit("/", 1)[1], value["url"].rsplit("/", 1)[1])

    def test_restoration_requires_an_actual_msi_source_entry(self) -> None:
        manifest = json.loads(self.manifest_path.read_text())
        del manifest["platforms"]["windows-x86_64-msi"]
        self.manifest_path.write_text(json.dumps(manifest))
        result = self.run_validator("--rewrite-download-urls", "--restore-windows-msi")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing signed MSI", result.stderr)

    def test_restoration_adds_missing_windows_aliases_from_msi_source(self) -> None:
        manifest = json.loads(self.manifest_path.read_text())
        del manifest["platforms"]["windows-x86_64-nsis"]
        del manifest["platforms"]["windows-x86_64"]
        self.manifest_path.write_text(json.dumps(manifest))
        result = self.run_validator("--rewrite-download-urls", "--restore-windows-msi")
        self.assertEqual(result.returncode, 0, result.stderr)
        actual = json.loads(self.manifest_path.read_text())["platforms"]
        self.assertEqual(actual["windows-x86_64-msi"], actual["windows-x86_64-nsis"])
        self.assertEqual(actual["windows-x86_64-msi"], actual["windows-x86_64"])

    def test_rejects_msi_signature_divergence(self) -> None:
        manifest = json.loads(self.manifest_path.read_text())
        manifest["platforms"]["windows-x86_64"]["signature"] = base64.b64encode(b"x" * 64).decode()
        self.manifest_path.write_text(json.dumps(manifest))
        result = self.run_validator("--rewrite-download-urls")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("same signed MSI installer", result.stderr)

    def test_rejects_divergent_signature_on_historical_nsis_target(self) -> None:
        manifest = json.loads(self.manifest_path.read_text())
        manifest["platforms"]["windows-x86_64-nsis"]["signature"] = base64.b64encode(b"n" * 64).decode()
        self.manifest_path.write_text(json.dumps(manifest))
        result = self.run_validator("--rewrite-download-urls")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("all Windows updater keys must use the same signed MSI", result.stderr)

    def test_rejects_unreferenced_exe_assets_and_signature_assets(self) -> None:
        original_assets = json.loads(self.assets_path.read_text())
        for name in ("Codex-X.exe", "Codex-X.exe.sig", "CODEX-X.EXE", "CODEX-X.EXE.SIG"):
            with self.subTest(name=name):
                self.assets_path.write_text(json.dumps([*original_assets, {"name": name}]))
                result = self.run_validator("--rewrite-download-urls", "--restore-windows-msi")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("EXE installer assets are forbidden", result.stderr)

    def test_historical_nsis_target_requires_msi_even_if_other_asset_exists(self) -> None:
        manifest = json.loads(self.manifest_path.read_text())
        manifest["platforms"]["windows-x86_64-nsis"]["url"] = draft_url("Codex-X.deb")
        self.manifest_path.write_text(json.dumps(manifest))
        result = self.run_validator("--rewrite-download-urls")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("expected a .msi updater", result.stderr)

    def test_requires_msi_and_other_platform_signature_assets_before_publishing(self) -> None:
        result = self.run_validator("--rewrite-download-urls", "--require-signature-assets")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("signature asset is missing", result.stderr)
        assets = json.loads(self.assets_path.read_text())
        assets.extend({"name": name + ".sig"} for name in set(PLATFORM_ASSETS.values()))
        self.assets_path.write_text(json.dumps(assets))
        result = self.run_validator("--require-signature-assets")
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
