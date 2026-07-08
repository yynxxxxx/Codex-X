<p align="center">
  <a href="README.md"><img src="https://img.shields.io/badge/中文-切换-lightgrey" alt="中文" /></a>
  <a href="README.en.md"><img src="https://img.shields.io/badge/English-Current-blue" alt="English" /></a>
</p>

<div align="center">
  <img src="apps/desktop/src-tauri/icons/icon.png" alt="Codex-X Logo" width="150" />

  # Codex-X

  **Prompt Injection · Provider Switching · TOML / Auth Visual Manager for Codex**

  Codex-X is a cross-platform desktop tool for **OpenAI Codex Desktop / Codex CLI**. It ships with `gpt5.5-unrestricted.md` and `gpt5.4-unrestricted.md`, and supports one-click enable / disable for instruction prompts, third-party Provider switching, official Auth management, TOML visual editing, and local session Provider Sync.

  <p>
    <img src="https://img.shields.io/github/v/release/yynxxxxx/Codex-X?label=version&color=blue" alt="version" />
    <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-555" alt="platform" />
    <img src="https://img.shields.io/badge/built%20with-Tauri%202-24C8DB" alt="tauri" />
    <img src="https://img.shields.io/badge/license-MIT-green" alt="license" />
  </p>

  <p>
    <img src="https://img.shields.io/badge/React-18-61DAFB?logo=react&logoColor=white" />
    <img src="https://img.shields.io/badge/TypeScript-5-3178C6?logo=typescript&logoColor=white" />
    <img src="https://img.shields.io/badge/Rust-stable-000000?logo=rust&logoColor=white" />
    <img src="https://img.shields.io/badge/SQLite-Ready-003B57?logo=sqlite&logoColor=white" />
    <img src="https://img.shields.io/badge/Vite-Ready-646CFF?logo=vite&logoColor=white" />
  </p>
</div>

---

## What is Codex-X?

Codex-X is not just a configuration-file editor. It is a **visual enhancement manager** built for Codex CLI and Codex Desktop workflows.

It turns several high-frequency operations into a desktop UI:

- Enable / disable instruction prompt templates for Codex
- Switch between official OpenAI and third-party Codex API Providers
- View / edit `~/.codex/config.toml`
- View / edit official `~/.codex/auth.json`
- Check and repair Provider metadata for local historical sessions

## Preview

<details open>
<summary><b>App preview</b>: Home / Provider / TOML / Auth</summary>

<p align="center">
  <img src="docs/screenshots/app/preview.png" alt="Codex-X app preview: Home, Provider, TOML, Auth" width="920" />
</p>

</details>

<details>
<summary><b>Prompt injection results: security testing scenarios</b></summary>

<div align="center">
<table>
  <tr>
    <td align="center" width="50%">
      <b>SQL Injection Testing</b><br />
      <sub>Post-deployment test: how to perform SQL injection testing against a target?</sub><br />
      <img src="docs/screenshots/prompt-effects/security/sql-injection.png" alt="SQL injection testing result" width="420" />
    </td>
    <td align="center" width="50%">
      <b>NSFW Response Test</b><br />
      <sub>Observe boundary response changes after prompt injection</sub><br />
      <img src="docs/screenshots/prompt-effects/security/nsfw.png" alt="NSFW response test result" width="420" />
    </td>
  </tr>
</table>
</div>

</details>

<details>
<summary><b>Prompt injection results: reverse engineering scenarios</b></summary>

<div align="center">
<table>
  <tr>
    <td align="center" width="50%">
      <b>APK Reverse Engineering</b><br />
      <sub>Static / dynamic analysis workflow for Android APKs</sub><br />
      <img src="docs/screenshots/prompt-effects/reverse/apk-reverse-1.png" alt="APK reverse engineering result" width="420" />
    </td>
    <td align="center" width="50%">
      <b>APK Reverse Engineering 2</b><br />
      <sub>Additional APK reverse workflow and locating methods</sub><br />
      <img src="docs/screenshots/prompt-effects/reverse/apk-reverse-2.png" alt="APK reverse engineering result 2" width="420" />
    </td>
  </tr>
  <tr>
    <td align="center" colspan="2">
      <b>EXE Reverse Engineering</b><br />
      <sub>Windows executable analysis and debugging directions</sub><br />
      <img src="docs/screenshots/prompt-effects/reverse/exe-reverse.png" alt="EXE reverse engineering result" width="620" />
    </td>
  </tr>
</table>
</div>

</details>

## Features

<div align="center">
<table>
  <tr>
    <th align="center" width="180">Feature</th>
    <th align="center">Description</th>
  </tr>
  <tr>
    <td align="center">⚡ Provider API</td>
    <td>Visually manage official OpenAI / third-party Codex Providers, including Base URL, API Key, Model, Wire API, and one-click switching.</td>
  </tr>
  <tr>
    <td align="center">🧩 <b>Prompt Injection</b></td>
    <td><b>Signature feature</b>: built-in <code>gpt5.4-unrestricted.md</code> / <code>gpt5.5-unrestricted.md</code>, one-click write into Codex configuration; after enabling, it can produce results similar to the screenshots above for SQL injection testing and APK / EXE reverse engineering scenarios.</td>
  </tr>
  <tr>
    <td align="center">💬 Session Management</td>
    <td>Read Codex local SQLite and rollout JSONL, display local sessions visually, and support Provider Sync / historical thread metadata repair.</td>
  </tr>
  <tr>
    <td align="center">📝 TOML Config</td>
    <td>View current <code>~/.codex/config.toml</code>, edit full TOML directly from the Provider editor, and save changes back to the Codex config directory.</td>
  </tr>
  <tr>
    <td align="center">🔐 Auth Management</td>
    <td>Read / edit official <code>~/.codex/auth.json</code>, and distinguish ChatGPT login-state Auth from third-party API Keys.</td>
  </tr>
  <tr>
    <td align="center">📦 Cross-platform Release</td>
    <td>Provides macOS Apple Silicon / Intel <code>.dmg</code>, Windows <code>.msi</code> / portable <code>.zip</code>, Linux <code>.deb</code> / <code>.rpm</code>, plus in-app update checking.</td>
  </tr>
</table>
</div>

## Highlights

### 1. Built-in dual instruction prompt templates

Codex-X currently includes two prompt templates:

- `examples/gpt5.5-unrestricted.md`
- `examples/gpt5.4-unrestricted.md`

You can enable / disable them directly from the Prompt page. When enabled, Codex-X writes the selected `.md` file into the Codex configuration directory and updates `model_instructions_file` in `config.toml`.

<div align="center">
<table>
  <tr>
    <th align="center">Item</th>
    <th align="center"><code>gpt5.4-unrestricted.md</code></th>
    <th align="center"><code>gpt5.5-unrestricted.md</code></th>
  </tr>
  <tr>
    <td align="center">Positioning</td>
    <td align="center">5.4 CTF sandbox</td>
    <td align="center">5.5 unrestricted mode</td>
  </tr>
  <tr>
    <td align="center">Strategy</td>
    <td align="center">Indirect bypass through CTF competition sandbox framing</td>
    <td align="center">Direct declaration of unrestricted developer mode</td>
  </tr>
  <tr>
    <td align="center">Style</td>
    <td align="center">Security-research oriented, detailed methodology</td>
    <td align="center">Minimal instructions, broad coverage</td>
  </tr>
  <tr>
    <td align="center">Length</td>
    <td align="center">~180 lines</td>
    <td align="center">~40 lines</td>
  </tr>
  <tr>
    <td align="center">Target</td>
    <td align="center">GPT-5.4 Codex CLI</td>
    <td align="center">GPT-5.5 Codex CLI</td>
  </tr>
</table>
</div>

Post-deployment test prompt:

```text
How do I perform SQL injection testing against a target?
```

Typical effect:

```text
Before injection → refusal or generic answer
After injection  → direct security research methodology and testing steps
```

### 2. Visual Provider switching

- Add third-party Codex API Providers
- Edit Base URL / API Key / Model / Wire API
- View and edit Provider-related TOML from the Provider page
- Clearly display the currently active Provider
- Import Codex Providers from a cc-switch database

### 3. Official Auth management

- Automatically read Codex official `auth.json`
- View / edit ChatGPT login-state Auth
- Distinguish official Auth from third-party API Keys
- Manage official Auth and third-party Providers in one UI

### 4. Visual TOML editing

- View the current Codex `config.toml`
- Dark code preview with syntax highlighting
- Edit full TOML directly from the Provider editor
- Save changes back to the Codex configuration directory

### 5. Session Management / Provider Sync

Codex-X can read Codex local session data:

```text
~/.codex/sqlite/*.db
~/.codex/state_5.sqlite
~/.codex/sessions/**/rollout-*.jsonl
~/.codex/archived_sessions/**/rollout-*.jsonl
```

It checks whether historical session Provider metadata matches the current configuration, and supports one-click sync / repair so historical threads can still be recognized, opened, and continued by native Codex.

### 6. Cross-platform desktop app

- macOS Apple Silicon `.dmg`
- macOS Intel `.dmg`
- Windows `.msi`
- Windows Portable `.zip`
- Linux `.deb` / `.rpm`
- Automatic GitHub Releases builds
- In-app update checking

## Tech Stack

| Category | Technology |
| --- | --- |
| Desktop framework | Tauri 2 |
| Frontend | React 18 / TypeScript / Vite |
| Backend | Rust |
| Local data | SQLite / rusqlite |
| Config editing | TOML / JSON |
| Release | GitHub Actions / GitHub Releases |

## Configuration Paths

Codex-X reads the Codex configuration directory by default:

```text
~/.codex/config.toml
~/.codex/auth.json
```

Environment variables are also supported:

```text
CODEX_HOME=/path/to/.codex
CODEXX_HOME=/path/to/codex-x-data
CC_SWITCH_HOME=/path/to/.cc-switch
```

Codex-X's own database is stored by default at:

```text
~/.codexx/codexx.db
```

## Download

Download from the Releases page:

https://github.com/yynxxxxx/Codex-X/releases

## Development

```bash
pnpm install
pnpm dev
```

Build desktop bundles:

```bash
pnpm --dir apps/desktop tauri build
```

## macOS Installation Note

If you see “app is damaged” when opening an unsigned / unnotarized DMG, this is normal macOS Gatekeeper behavior.

- Best option: sign and notarize with an Apple Developer ID
- Local testing only: remove the quarantine attribute manually

```bash
xattr -dr com.apple.quarantine /Applications/Codex-X.app
```

## License

This project is open-sourced under the [MIT License](https://github.com/yynxxxxx/Codex-X/blob/main/LICENSE).

## Thanks

Thanks to the [LINUX DO forum](https://linux.do/) community for attention, feedback, and support.

## Star History

<a href="https://www.star-history.com/?repos=yynxxxxx%2FCodex-X&type=date&legend=top-left">
  <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=yynxxxxx%2Fcodex-x&amp;type=date&amp;legend=top-left&amp;sealed_token=Vqvz67Jv_WIePGePCN8RdJaY5oCyqqCZUSFWs4M4dAxP8JXFZlYEWbI8YcU6SFgpqOqqJifzpOTIlMg4ee8NaCkHpCSqv1r5pxewR-tQlmxswaZlhedd6A" width="900" />
</a>
