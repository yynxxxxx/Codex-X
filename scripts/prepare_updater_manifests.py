#!/usr/bin/env python3
"""Create the per-platform dynamic manifests consumed by the Tauri updater."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path


UPDATER_ARTIFACTS = {
    "latest-darwin-aarch64-app.json": "Codex-X_{version}_darwin_aarch64.app.tar.gz",
    "latest-darwin-x86_64-app.json": "Codex-X_{version}_darwin_x86_64.app.tar.gz",
    "latest-windows-x86_64-msi.json": "Codex-X_{version}_windows_x86_64.msi.zip",
    "latest-linux-x86_64-deb.json": "Codex-X_{version}_linux_x64.deb",
    "latest-linux-x86_64-rpm.json": "Codex-X_{version}_linux_x64.rpm",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--asset-dir", type=Path, required=True)
    parser.add_argument("--notes-file", type=Path, required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--tag", required=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    tag = args.tag.strip()
    version = tag.removeprefix("v")
    if not tag or not version:
        raise SystemExit("The release tag must contain a version")

    notes = args.notes_file.read_text(encoding="utf-8").strip()
    published_at = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    base_url = f"https://github.com/{args.repository}/releases/download/{tag}"

    for manifest_name, artifact_template in UPDATER_ARTIFACTS.items():
        artifact_name = artifact_template.format(version=version)
        artifact = args.asset_dir / artifact_name
        signature = args.asset_dir / f"{artifact_name}.sig"
        if not artifact.is_file() or not signature.is_file():
            raise SystemExit(f"Missing updater artifact or signature: {artifact_name}")

        payload = {
            "version": version,
            "notes": notes,
            "pub_date": published_at,
            "url": f"{base_url}/{artifact_name}",
            "signature": signature.read_text(encoding="utf-8").strip(),
        }
        (args.asset_dir / manifest_name).write_text(
            json.dumps(payload, ensure_ascii=False, indent=2) + "\n",
            encoding="utf-8",
        )


if __name__ == "__main__":
    main()
