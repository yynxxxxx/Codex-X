<p align="center">
  <a href="README.md"><img src="https://img.shields.io/badge/中文-切换-lightgrey" alt="中文" /></a>
  <a href="README.en.md"><img src="https://img.shields.io/badge/English-Current-blue" alt="English" /></a>
</p>

<div align="center">
  <img src="apps/desktop/src-tauri/icons/icon.png" alt="Codex-X Logo" width="150" />

  # Codex-X

  **Codex Prompts · API / Providers · Sessions · Skills / MCP in One Place**

  A cross-platform desktop tool for **OpenAI Codex Desktop / Codex CLI**. Manage prompt templates, switch third-party APIs, organize / repair / permanently delete local sessions, manage Skills / MCP, and inspect TOML and login credentials without repeatedly editing configuration files by hand.

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

When you use Codex Desktop, the CLI, third-party APIs, and multiple prompts together, settings quickly become scattered across different files. Codex-X brings these frequent tasks into one desktop interface, so you can see the current state and complete common actions with a click.

You can use it to:

- Choose from the prompt library or import your own `.md`, then keep the existing prompt or replace it
- Save, test, and switch between OpenAI Official and third-party APIs, including Providers imported from cc-switch
- Search and organize local sessions by project, check / automatically repair problems, and precisely delete sessions you no longer need
- Manage Skills and MCP in one place, and inspect the current `config.toml`, `auth.json`, and operation backups

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
    <th align="center" width="190">What you want to do</th>
    <th align="center">How Codex-X helps</th>
  </tr>
  <tr>
    <td align="center">🧩 <b>Use prompt templates</b></td>
    <td align="left">The current library contains <b>5 templates</b>. Enable / disable one with a click and choose “Keep existing” or “Replace existing”; GitHub sync, local caching, and importing or editing your own <code>.md</code> files are supported.</td>
  </tr>
  <tr>
    <td align="center">⚡ <b>Switch APIs / relays</b></td>
    <td align="left">Save, test, and enable multiple third-party Providers, or import them from cc-switch; entries with the same endpoint and Key are merged automatically. Create or reopen a session after switching to apply the change without restarting the entire Codex client.</td>
  </tr>
  <tr>
    <td align="center">💬 <b>Organize local sessions</b></td>
    <td align="left">Search sessions, group them by project path, and check or automatically repair inconsistent data. Select one, several, or an entire project to permanently delete specific sessions from Codex storage.</td>
  </tr>
  <tr>
    <td align="center">🧠 <b>Manage Skills / MCP</b></td>
    <td align="left">View available Skills and MCP servers, preview existing items before import, install a Skill from ZIP, enable / disable individual entries, and check Skill update status.</td>
  </tr>
  <tr>
    <td align="center">📝 <b>Manage config and login</b></td>
    <td align="left">Inspect the active Codex <code>config.toml</code> and <code>auth.json</code> in one place, with a clear distinction between official login credentials and third-party API Keys. Important writes are backed up automatically and can be restored when needed.</td>
  </tr>
  <tr>
    <td align="center">📦 <b>Use it across platforms</b></td>
    <td align="left">Available for macOS Apple Silicon / Intel, Windows MSI / portable, and Linux packages. Installed editions can download, verify, and install updates in the app; the portable edition keeps manual downloads.</td>
  </tr>
</table>
</div>

## Highlights

### 1. Prompt Template Center

<p align="center">
  <img src="https://img.shields.io/badge/Template_library-5_total-2563eb?style=flat-square" alt="5 templates in the current library" />
  <img src="https://img.shields.io/badge/Bundled_offline-5_templates-16a34a?style=flat-square" alt="5 templates bundled for offline use" />
  <img src="https://img.shields.io/badge/GitHub_sync-Automatic_updates-f59e0b?style=flat-square" alt="Automatic GitHub template sync" />
</p>

> [!TIP]
> **Ready after installation, automatically expanded when online.**
>
> The app bundle includes all 5 current templates for offline use. After launch, it synchronizes updates and newly added templates from GitHub `examples/` in the background, without waiting for the **Prompt** page to open. Successfully synchronized online versions are cached locally and remain available during temporary network outages.

<div align="center">
<table>
  <tr>
    <th align="center">Template</th>
    <th align="center">Best for</th>
    <th align="center">Availability</th>
  </tr>
  <tr>
    <td><a href="examples/gpt5.5-unrestricted.md"><code>gpt5.5-unrestricted.md</code></a></td>
    <td align="left">Compact and general-purpose for everyday coding and technical work</td>
    <td align="center">Bundled offline<br />GitHub updates</td>
  </tr>
  <tr>
    <td><a href="examples/gpt5.4-unrestricted.md"><code>gpt5.4-unrestricted.md</code></a></td>
    <td align="left">GPT-5.4 / Codex CLI workflows with a CTF and security-research focus</td>
    <td align="center">Bundled offline<br />GitHub updates</td>
  </tr>
  <tr>
    <td><a href="examples/gpt5.5-jeli.md"><code>gpt5.5-jeli.md</code></a></td>
    <td align="left">A plain-language general version with a fuller engineering and reverse-engineering workflow</td>
    <td align="center">Bundled offline<br />GitHub updates</td>
  </tr>
  <tr>
    <td><a href="examples/gpt-5.6-sol-unrestricted.md"><code>gpt-5.6-sol-unrestricted.md</code></a></td>
    <td align="left">A GPT-5.6 SOL prompt focused on direct execution and bilingual tasks</td>
    <td align="center">Bundled offline<br />GitHub updates</td>
  </tr>
  <tr>
    <td><a href="examples/%E6%B5%B7%E9%B8%A53.0%E7%A0%B4%E7%94%B2.md"><code>海鸥3.0破甲.md</code></a></td>
    <td align="left">A Chinese technical-operator persona with routing for coding, CTF, reverse engineering, memory, and protocol work</td>
    <td align="center">Bundled offline<br />GitHub updates</td>
  </tr>
</table>
</div>

<table>
  <tr>
    <td width="50%" valign="top">
      <b>Keep existing prompt</b><br />
      Best for users who already have personal rules. Codex-X only appends its managed content and removes only that content when disabled, leaving the original prompt untouched.
    </td>
    <td width="50%" valign="top">
      <b>Replace existing prompt</b><br />
      Makes the selected template the primary instruction entry point, which is useful when you want to switch completely to a specific template.
    </td>
  </tr>
</table>

A backup is created automatically before every enable or disable action. In addition to the template library, you can import, edit, and delete your own `.md` prompts.

### 2. Provider Switching: Ready in a New Session

> [!NOTE]
> After enabling a new third-party Provider, create or reopen a Codex session to use the new relay. You do not need to restart the entire Codex client.

- Save multiple third-party Providers and always see which one is currently active
- Test an API endpoint before switching, and save or enable a configuration separately
- Edit the Base URL, API Key, Model, Wire API, and complete TOML on the same page
- cc-switch imports report added, updated, merged, and skipped entries; the same URL + Key is no longer shown more than once
- Switching back to OpenAI Official preserves the current official login, and third-party configurations no longer disappear unexpectedly

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

### 5. Session Management: Inspect, Repair, and Permanently Delete

<table>
  <tr>
    <td width="50%" valign="top">
      <b>Find and organize</b><br />
      Search sessions by title or project path and group them by project. Internal subagent sessions created automatically by Codex stay out of the normal session list by default.
    </td>
    <td width="50%" valign="top">
      <b>Inspect and repair</b><br />
      Check whether local sessions match the current Provider, repair all mismatches manually, or enable automatic inspection and repair at startup.
    </td>
  </tr>
  <tr>
    <td colspan="2" valign="top">
      <b>Precise deletion</b><br />
      Select one session, several sessions, or one or more projects to select all sessions under them. After confirmation, the matching sessions and their derived child sessions are removed from Codex storage itself.
    </td>
  </tr>
</table>

> [!CAUTION]
> **Permanent deletion cannot be undone.** Close any Codex windows or CLI processes still using those sessions, then review the deletion list again in the confirmation dialog.

### 6. Skills / MCP Management

Manage Codex capability extensions from the **Skills & MCP** page instead of searching through multiple directories and configuration files.

<table>
  <tr>
    <td width="50%" valign="top">
      <b>Skills</b><br />
      View current Skills, import existing content, or install from ZIP. Enable / disable entries individually and check whether installed Skills have updates.
    </td>
    <td width="50%" valign="top">
      <b>MCP</b><br />
      Preview existing MCP servers before importing them, then choose what Codex-X should manage. Codex-X maintains the Codex configuration when a server is enabled or disabled.
    </td>
  </tr>
</table>

### 7. Reverse Skills Navigation

<div align="center">
  <a href="https://yynxxxxx.github.io/Codex-X/">
    <img src="https://img.shields.io/badge/Codex--X-Online%20Reverse%20Skills%20Guide-0ea5e9?style=for-the-badge&logo=githubpages&logoColor=white" alt="Codex-X Online Reverse Skills Guide" />
  </a>
</div>

<br />

<table>
  <tr>
    <td width="55%">
      <b>Online guide</b>: explains the “armor breaking” workflow, how to enable GPT-5.5 / unrestricted jeli in Codex-X, and how to combine it with reverse-engineering Skills.
      <br /><br />
      <b>Categories</b>: Android APK / Windows EXE / Web protocol reverse engineering.
      <br /><br />
      <b>Includes</b>: Skill purpose, install commands, source links, and recommended workflow.
    </td>
    <td width="45%">
      <ul>
        <li>🧩 GPT-5.5 / unrestricted jeli workflow</li>
        <li>📱 Android APK reverse Skills</li>
        <li>🪟 Windows EXE / DLL reverse Skills</li>
        <li>🌐 Web / API / protocol reverse Skills</li>
        <li>📋 One-click copy install commands</li>
      </ul>
    </td>
  </tr>
</table>

<p align="center">
  <a href="https://yynxxxxx.github.io/Codex-X/">
    <b>🚀 Open Codex-X Reverse Skills Guide</b>
  </a>
</p>

### 8. Cross-platform desktop app

- macOS Apple Silicon `.dmg`
- macOS Intel `.dmg`
- Windows `.msi`
- Windows Portable `.zip`
- Linux `.deb` / `.rpm` / `.AppImage`
- Automatic GitHub Releases builds
- In-app updates for installed editions; manual updates for Windows Portable

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

<p align="center">
  <a href="https://github.com/yynxxxxx/Codex-X/stargazers">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="https://codex-star-history.zhihack0728.workers.dev/v1/charts/codex-x.svg?theme=dark" />
      <source media="(prefers-color-scheme: light)" srcset="https://codex-star-history.zhihack0728.workers.dev/v1/charts/codex-x.svg?theme=light" />
      <img alt="Codex-X Star History" src="https://codex-star-history.zhihack0728.workers.dev/v1/charts/codex-x.svg?theme=light" width="900" />
    </picture>
  </a>
</p>
