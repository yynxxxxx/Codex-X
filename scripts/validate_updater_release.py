#!/usr/bin/env python3
"""Validate updater configuration, artifacts, signatures, and manifests."""

from __future__ import annotations

import argparse
import base64
import binascii
import json
from pathlib import Path
from urllib.parse import urlparse

from prepare_updater_manifests import UPDATER_ARTIFACTS


ROOT = Path(__file__).resolve().parents[1]
TAURI_CONFIG = ROOT / "apps/desktop/src-tauri/tauri.conf.json"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--asset-dir", type=Path)
    parser.add_argument("--repository")
    parser.add_argument("--tag")
    parser.add_argument("--config-only", action="store_true")
    return parser.parse_args()


def decode_base64(value: str, label: str) -> bytes:
    try:
        return base64.b64decode(value.strip(), validate=True)
    except (binascii.Error, ValueError) as error:
        raise ValueError(f"{label} is not valid base64: {error}") from error


def minisign_public_key_id(encoded_public_key: str) -> bytes:
    text = decode_base64(encoded_public_key, "updater public key").decode("utf-8")
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    if len(lines) < 2 or not lines[0].startswith("untrusted comment:"):
        raise ValueError("updater public key is not a minisign public key")
    packet = decode_base64(lines[1], "minisign public key packet")
    if len(packet) != 42 or packet[:2] != b"Ed":
        raise ValueError("updater public key packet has an unexpected format")
    return packet[2:10]


def minisign_signature_key_id(encoded_signature: str, label: str) -> bytes:
    text = decode_base64(encoded_signature, label).decode("utf-8")
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    if len(lines) < 4 or not lines[0].startswith("untrusted comment:"):
        raise ValueError(f"{label} is not a complete minisign signature")
    packet = decode_base64(lines[1], f"{label} packet")
    if len(packet) != 74 or packet[:2] not in {b"Ed", b"ED"}:
        raise ValueError(f"{label} packet has an unexpected format")
    return packet[2:10]


def load_updater_config() -> tuple[dict, bytes]:
    config = json.loads(TAURI_CONFIG.read_text(encoding="utf-8"))
    bundle = config.get("bundle", {})
    if bundle.get("createUpdaterArtifacts") is not True:
        raise ValueError("bundle.createUpdaterArtifacts must be true")

    updater = config.get("plugins", {}).get("updater", {})
    endpoints = updater.get("endpoints") or []
    expected_endpoint = "https://github.com/yynxxxxx/Codex-X/releases/latest/download/latest-{{target}}-{{arch}}-{{bundle_type}}.json"
    if endpoints != [expected_endpoint]:
        raise ValueError(f"unexpected updater endpoint: {endpoints!r}")
    return config, minisign_public_key_id(updater.get("pubkey", ""))


def validate_release(asset_dir: Path, repository: str, tag: str, public_key_id: bytes) -> None:
    version = tag.removeprefix("v")
    expected_base_url = f"https://github.com/{repository}/releases/download/{tag}"
    for manifest_name, artifact_template in UPDATER_ARTIFACTS.items():
        artifact_name = artifact_template.format(version=version)
        artifact = asset_dir / artifact_name
        signature_path = asset_dir / f"{artifact_name}.sig"
        manifest_path = asset_dir / manifest_name
        for path in (artifact, signature_path, manifest_path):
            if not path.is_file() or path.stat().st_size == 0:
                raise ValueError(f"missing or empty updater release file: {path}")

        signature = signature_path.read_text(encoding="utf-8").strip()
        if minisign_signature_key_id(signature, signature_path.name) != public_key_id:
            raise ValueError(f"{signature_path.name} was not produced by the configured updater key")

        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        expected_url = f"{expected_base_url}/{artifact_name}"
        if manifest.get("version") != version:
            raise ValueError(f"{manifest_name} has an unexpected version")
        if manifest.get("url") != expected_url or urlparse(expected_url).scheme != "https":
            raise ValueError(f"{manifest_name} has an unexpected artifact URL")
        if manifest.get("signature") != signature:
            raise ValueError(f"{manifest_name} does not embed the matching signature")
        if not manifest.get("pub_date"):
            raise ValueError(f"{manifest_name} is missing pub_date")


def main() -> None:
    args = parse_args()
    _, public_key_id = load_updater_config()
    if args.config_only:
        print("Updater configuration is valid for macOS, Windows, and Linux.")
        return
    if not args.asset_dir or not args.repository or not args.tag:
        raise SystemExit("--asset-dir, --repository, and --tag are required")
    validate_release(args.asset_dir, args.repository, args.tag, public_key_id)
    print("Updater artifacts, signatures, and manifests are valid for all release targets.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, UnicodeError, ValueError, json.JSONDecodeError) as error:
        raise SystemExit(str(error)) from error
