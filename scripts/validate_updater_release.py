#!/usr/bin/env python3
"""Validate Codex-X updater metadata before a draft release is published."""

from __future__ import annotations

import argparse
import base64
import binascii
import json
from pathlib import Path
from typing import Any
from urllib.parse import quote, unquote, urlparse


REQUIRED_PLATFORMS = {
    "darwin-aarch64": ".app.tar.gz",
    "darwin-aarch64-app": ".app.tar.gz",
    "darwin-x86_64": ".app.tar.gz",
    "darwin-x86_64-app": ".app.tar.gz",
    "windows-x86_64": ".msi",
    # Keep the historical target name, but publish only MSI installers.
    "windows-x86_64-nsis": ".msi",
    "windows-x86_64-msi": ".msi",
    "linux-x86_64": ".AppImage",
    "linux-x86_64-deb": ".deb",
    "linux-x86_64-rpm": ".rpm",
    "linux-x86_64-appimage": ".AppImage",
}
WINDOWS_MSI_PLATFORMS = (
    "windows-x86_64", "windows-x86_64-msi", "windows-x86_64-nsis"
)


def restore_windows_msi(manifest: dict[str, Any]) -> None:
    """Map every Windows target to the one published, signed MSI installer.

    This metadata cannot add MSI support to old EXE-only clients. Those clients
    require a one-time manual MSI installation, as documented in release notes.
    """
    platforms = manifest.get("platforms")
    if not isinstance(platforms, dict):
        fail("latest.json has no platforms object")
    source = platforms.get("windows-x86_64-msi")
    if not isinstance(source, dict):
        fail("missing signed MSI platform for Windows restoration")
    for platform in WINDOWS_MSI_PLATFORMS:
        platforms[platform] = dict(source)


def fail(message: str) -> None:
    raise SystemExit(f"Updater release validation failed: {message}")


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read {path}: {error}")


def validate_signature(platform: str, value: Any) -> None:
    if not isinstance(value, str) or not value.strip():
        fail(f"{platform} has no signature")
    compact = "".join(value.split())
    try:
        decoded = base64.b64decode(compact, validate=True)
    except (ValueError, binascii.Error) as error:
        fail(f"{platform} signature is not valid Base64: {error}")
    if len(decoded) < 64:
        fail(f"{platform} signature is unexpectedly short")


def canonical_asset_url(repository: str, release_tag: str, asset_name: str) -> str:
    return (
        f"https://github.com/{repository}/releases/download/"
        f"{quote(release_tag, safe='')}/{quote(asset_name, safe='')}"
    )


def rewrite_download_urls(
    manifest_path: Path,
    manifest: dict[str, Any],
    assets_by_url: dict[str, dict[str, Any]],
    assets_by_name: dict[str, dict[str, Any]],
    repository: str,
    release_tag: str,
) -> int:
    platforms = manifest.get("platforms")
    if not isinstance(platforms, dict):
        fail("latest.json has no platforms object")

    rewritten = 0
    for platform, entry in platforms.items():
        if not isinstance(entry, dict):
            fail(f"invalid platform entry {platform}")
        current_url = entry.get("url")
        if not isinstance(current_url, str):
            fail(f"{platform} has an invalid download URL")
        asset = assets_by_url.get(current_url)
        if asset is None:
            parsed_url = urlparse(current_url)
            asset_name = unquote(parsed_url.path.rsplit("/", 1)[-1])
            if parsed_url.scheme == "https" and parsed_url.netloc == "github.com":
                asset = assets_by_name.get(asset_name)
        if asset is None:
            fail(f"{platform} URL does not point to an asset in this release")
        download_url = canonical_asset_url(repository, release_tag, asset["name"])
        if current_url != download_url:
            entry["url"] = download_url
            rewritten += 1

    manifest_path.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    return rewritten


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--assets", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--release-tag", required=True)
    parser.add_argument("--rewrite-download-urls", action="store_true")
    parser.add_argument("--restore-windows-msi", action="store_true")
    parser.add_argument("--require-signature-assets", action="store_true")
    args = parser.parse_args()

    repository_parts = args.repository.split("/")
    if len(repository_parts) != 2 or not all(repository_parts):
        fail(f"invalid GitHub repository {args.repository!r}")

    manifest = load_json(args.manifest)
    assets = load_json(args.assets)
    if not isinstance(manifest, dict):
        fail("latest.json must contain an object")
    if not isinstance(assets, list):
        fail("release assets response must contain a list")
    if manifest.get("version") != args.version:
        fail(
            f"latest.json version {manifest.get('version')!r} does not match {args.version!r}"
        )

    asset_names: set[str] = set()
    assets_by_name: dict[str, dict[str, Any]] = {}
    assets_by_url: dict[str, dict[str, Any]] = {}
    for asset in assets:
        if not isinstance(asset, dict) or not isinstance(asset.get("name"), str):
            fail("release assets response contains an invalid entry")
        if asset["name"].lower().endswith((".exe", ".exe.sig")):
            fail("EXE installer assets are forbidden in an MSI-only release")
        asset_names.add(asset["name"])
        assets_by_name[asset["name"]] = asset
        for field in ("url", "browser_download_url"):
            value = asset.get(field)
            if isinstance(value, str) and value:
                assets_by_url[value] = asset
        assets_by_url[
            canonical_asset_url(args.repository, args.release_tag, asset["name"])
        ] = asset

    if "latest.json" not in asset_names:
        fail("the draft release does not contain latest.json")

    platforms = manifest.get("platforms")
    if not isinstance(platforms, dict):
        fail("latest.json has no platforms object")

    if args.restore_windows_msi:
        if not args.rewrite_download_urls:
            fail("--restore-windows-msi requires --rewrite-download-urls")
        restore_windows_msi(manifest)

    if args.rewrite_download_urls:
        rewritten = rewrite_download_urls(
            args.manifest,
            manifest,
            assets_by_url,
            assets_by_name,
            args.repository,
            args.release_tag,
        )
        print(f"Rewrote {rewritten} updater asset URLs to public download URLs.")

    for platform, entry in platforms.items():
        if not isinstance(entry, dict):
            fail(f"invalid platform entry {platform}")
        validate_signature(platform, entry.get("signature"))
        url = entry.get("url")
        if not isinstance(url, str) or not url.startswith("https://"):
            fail(f"{platform} has an invalid download URL")
        asset = assets_by_url.get(url)
        if asset is None:
            fail(f"{platform} URL does not point to an asset in this release")
        expected_url = canonical_asset_url(
            args.repository,
            args.release_tag,
            asset["name"],
        )
        if url != expected_url:
            fail(f"{platform} URL does not use the stable release tag download URL")

    for platform, suffix in REQUIRED_PLATFORMS.items():
        entry = platforms.get(platform)
        if not isinstance(entry, dict):
            fail(f"missing platform {platform}")

        asset = assets_by_url[entry["url"]]
        asset_name = asset["name"]
        if not asset_name.endswith(suffix):
            fail(f"{platform} points to {asset_name!r}, expected a {suffix} updater")
        if args.require_signature_assets and f"{asset_name}.sig" not in asset_names:
            fail(f"signature asset is missing for {asset_name}")

    windows_entries = {
        (platforms[key]["url"], platforms[key]["signature"])
        for key in WINDOWS_MSI_PLATFORMS
    }
    if len(windows_entries) != 1:
        fail("all Windows updater keys must use the same signed MSI installer")

    signature_status = (
        "signature assets required"
        if args.require_signature_assets
        else "signature assets optional"
    )
    print(
        f"Validated updater {args.version}: "
        f"{len(REQUIRED_PLATFORMS)} platform installers are complete; "
        f"{signature_status}."
    )


if __name__ == "__main__":
    main()
