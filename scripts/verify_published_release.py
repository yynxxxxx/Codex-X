#!/usr/bin/env python3
"""Read-only verification of complete published updater downloads."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
from urllib.error import HTTPError
from urllib.parse import quote
from urllib.request import Request, urlopen

tag = os.environ["RELEASE_TAG"]
repository = os.environ["GITHUB_REPOSITORY"]
if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?", tag):
    raise SystemExit("Invalid release tag")
if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
    raise SystemExit("Invalid repository")
api = "https://api.github.com/repos/" + repository
directory = Path("published-release-verification")
directory.mkdir()
report = {"tag": tag, "repository": repository, "status": "failed", "downloads": []}

def error_label(error):
    return "HTTP {}".format(error.code) if isinstance(error, HTTPError) else type(error).__name__

def request(url, asset=False):
    headers = {"User-Agent": "Codex-X-published-release-verifier", "Accept": "application/octet-stream" if asset else "application/vnd.github+json"}
    result = Request(url, headers=headers, method="GET")
    if url.startswith(api + "/"):
        # urllib excludes unredirected headers from redirect requests,
        # so the token never reaches the release asset/CDN origin.
        result.add_unredirected_header("Authorization", "Bearer " + os.environ["GH_TOKEN"])
        result.add_header("X-GitHub-Api-Version", "2022-11-28")
    return result

def api_json(url):
    for attempt in range(1, 4):
        try:
            with urlopen(request(url), timeout=45) as response:
                return json.load(response)
        except Exception as error:
            print("Metadata attempt {}: {}".format(attempt, error_label(error)))
            if attempt == 3:
                raise RuntimeError("GitHub metadata could not be retrieved") from None
            time.sleep(2 * attempt)

def download(asset, destination):
    expected = asset.get("digest", "")
    if not isinstance(expected, str) or not re.fullmatch(r"sha256:[0-9a-fA-F]{64}", expected):
        raise RuntimeError("Missing GitHub SHA-256 digest: " + asset["name"])
    size = asset.get("size")
    if not isinstance(size, int) or size <= 0 or not isinstance(asset.get("id"), int):
        raise RuntimeError("Invalid GitHub asset metadata: " + asset["name"])
    standard_url = "https://github.com/{}/releases/download/{}/{}".format(repository, quote(tag, safe=""), quote(asset["name"], safe=""))
    entry = {"asset": asset["name"], "standard_url": standard_url, "expected_digest": expected, "attempts": [], "verified": False}
    report["downloads"].append(entry)
    for transport, url in (("public", standard_url), ("official-asset-api", api + "/releases/assets/" + str(asset["id"]))):
        for attempt in range(1, 4):
            observation = {"transport": transport, "attempt": attempt}
            entry["attempts"].append(observation)
            try:
                digest = hashlib.sha256()
                received = 0
                deadline = time.monotonic() + 180
                with urlopen(request(url, asset=True), timeout=45) as response, destination.open("wb") as output:
                    if response.status != 200:
                        raise RuntimeError("Expected a complete HTTP 200 response")
                    while True:
                        if time.monotonic() > deadline:
                            raise TimeoutError("Complete download deadline exceeded")
                        chunk = response.read(1024 * 1024)
                        if not chunk:
                            break
                        received += len(chunk)
                        if received > size:
                            raise RuntimeError("Downloaded asset exceeds its advertised size")
                        digest.update(chunk)
                        output.write(chunk)
                actual = "sha256:" + digest.hexdigest()
                if received != size or actual.lower() != expected.lower():
                    raise RuntimeError("Downloaded size or SHA-256 digest does not match GitHub")
                observation["status"] = "verified"
                entry.update(verified=True, transport=transport, public_url_verified=(transport == "public"), api_fallback=(transport != "public"), size=received, digest=actual)
                print("Verified {}: {} bytes, {}{}".format(asset["name"], received, actual, " (official API fallback; public URL failed)" if entry["api_fallback"] else " (public URL)"))
                return
            except Exception as error:
                observation.update(status="failed", error=error_label(error))
                destination.unlink(missing_ok=True)
                print("{} {} attempt {}: {}".format(asset["name"], transport, attempt, error_label(error)))
                if attempt < 3:
                    time.sleep(2 * attempt)
    raise RuntimeError("Complete asset download failed over both public and official API URLs: " + asset["name"])

failed = False
try:
    release = api_json(api + "/releases/tags/" + quote(tag, safe=""))
    if release.get("draft") is not False or release.get("tag_name") != tag or not release.get("published_at"):
        raise RuntimeError("The requested release is not published or its tag does not match")
    (directory / "release.json").write_text(json.dumps(release, indent=2), encoding="utf-8")
    assets = []
    for page in range(1, 11):
        batch = api_json(api + "/releases/{}/assets?per_page=100&page={}".format(release["id"], page))
        if not isinstance(batch, list):
            raise RuntimeError("Invalid release assets response")
        assets.extend(batch)
        if len(batch) < 100:
            break
    else:
        raise RuntimeError("Release assets pagination limit exceeded")
    (directory / "assets.json").write_text(json.dumps(assets, indent=2), encoding="utf-8")
    by_name = {asset["name"]: asset for asset in assets}
    if len(by_name) != len(assets) or "latest.json" not in by_name:
        raise RuntimeError("Release assets are duplicated or latest.json is missing")
    manifest_path = directory / "latest.json"
    download(by_name["latest.json"], manifest_path)
    environment = dict(os.environ)
    environment.pop("GH_TOKEN", None)
    validation = subprocess.run([sys.executable, "scripts/validate_updater_release.py", "--manifest", str(manifest_path), "--assets", str(directory / "assets.json"), "--version", tag[1:], "--repository", repository, "--release-tag", tag], capture_output=True, text=True, env=environment)
    report["manifest_validation"] = (validation.stdout + validation.stderr).strip()
    if validation.returncode:
        raise RuntimeError("Published updater manifest failed validation")
    print(report["manifest_validation"])
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    by_url = {asset["browser_download_url"]: asset for asset in assets}
    urls = sorted({entry["url"] for entry in manifest["platforms"].values()})
    report.update(platform_count=len(manifest["platforms"]), installer_count=len(urls))
    with tempfile.TemporaryDirectory(prefix="codex-x-release-download-") as temporary:
        for index, url in enumerate(urls):
            if url not in by_url:
                raise RuntimeError("Manifest URL is missing from the published release assets")
            payload = Path(temporary) / str(index)
            download(by_url[url], payload)
            payload.unlink()
    report["status"] = "verified"
except Exception as error:
    failed = True
    report["error"] = str(error) if isinstance(error, RuntimeError) else error_label(error)
    print("Verification failed: " + report["error"])
finally:
    (directory / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    lines = ["# Published release verification: " + tag, "", "Result: **" + report["status"] + "**", "", "Read-only verification of published files; this check does not modify release assets.", "", "Complete GET downloads are checked against GitHub asset size and SHA-256 digest. A successful API fallback does not mean the standard public URL succeeded.", ""]
    if "error" in report:
        lines.extend(["Failure: " + report["error"], ""])
    for entry in report["downloads"]:
        lines.append("- [{}]({}): {}{}".format(entry["asset"], entry["standard_url"], entry.get("transport", "failed"), "; public URL failed; official API fallback verified" if entry.get("api_fallback") else "; SHA-256 verified" if entry["verified"] else "; not verified"))
    summary = "\n".join(lines) + "\n"
    (directory / "report.md").write_text(summary, encoding="utf-8")
    with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as output:
        output.write(summary)
if failed:
    raise SystemExit(1)
