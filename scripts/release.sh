#!/usr/bin/env bash
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "Usage: scripts/release.sh <version>" >&2
  echo "Example: scripts/release.sh 0.2.18" >&2
  exit 1
fi

version="${1#v}"
tag="v${version}"

if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$ ]]; then
  echo "Invalid version: $1" >&2
  exit 1
fi

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "Working tree has uncommitted changes. Please commit or stash first." >&2
  exit 1
fi

if git rev-parse "$tag" >/dev/null 2>&1; then
  echo "Tag already exists: $tag" >&2
  exit 1
fi

VERSION="$version" node --input-type=module <<'NODE'
import fs from 'node:fs';
const version = process.env.VERSION;
for (const file of ['package.json', 'apps/desktop/package.json']) {
  const data = JSON.parse(fs.readFileSync(file, 'utf8'));
  data.version = version;
  fs.writeFileSync(file, JSON.stringify(data, null, 2) + '\n');
}
NODE

VERSION="$version" python3 - <<'PY'
import os
from pathlib import Path
version = os.environ['VERSION']
files = [
    Path('apps/desktop/src-tauri/Cargo.toml'),
    Path('apps/desktop/src-tauri/tauri.conf.json'),
]
for path in files:
    text = path.read_text()
    if path.suffix == '.toml':
        text = text.replace(next(line for line in text.splitlines() if line.startswith('version = ')), f'version = "{version}"', 1)
    else:
        import json
        data = json.loads(text)
        data['version'] = version
        text = json.dumps(data, indent=2, ensure_ascii=False) + '\n'
    path.write_text(text)

changelog = Path('CHANGELOG.md')
text = changelog.read_text() if changelog.exists() else '# Changelog\n\n'
heading = f'## [v{version}] - TODO'
if f'## [v{version}]' not in text:
    lines = text.splitlines()
    insert_at = 1 if lines and lines[0].startswith('#') else 0
    block = [
        '',
        heading,
        '',
        '- TODO: add release highlights.',
    ]
    lines[insert_at:insert_at] = block
    changelog.write_text('\n'.join(lines).rstrip() + '\n')
PY

pnpm --dir apps/desktop typecheck
cargo check --manifest-path apps/desktop/src-tauri/Cargo.toml

git add package.json apps/desktop/package.json apps/desktop/src-tauri/Cargo.toml apps/desktop/src-tauri/Cargo.lock apps/desktop/src-tauri/tauri.conf.json CHANGELOG.md
git commit -m "Release ${tag}"
git tag "$tag"

echo "Prepared ${tag}. Review CHANGELOG TODO, then push with:"
echo "  git push origin main ${tag}"
