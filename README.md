<div align="center">
  <img src="apps/desktop/src-tauri/icons/icon.png" alt="Codex-X Logo" width="160" />

  # Codex-X

  **Codex 提示词注入与配置切换管理器**

  一款面向 **OpenAI Codex / Codex CLI** 的跨平台桌面工具，重点提供 `gpt5.5-unrestricted.md` 指令提示词注入、Provider 切换、官方 Auth 管理与 TOML 可视化编辑。

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

## Codex-X 是什么？

Codex-X 不是单纯的配置文件编辑器，而是一个更偏向「Codex 增强启动器」的桌面管理工具。

它可以把预设或自定义的 **指令提示词模板** 写入 Codex 配置，例如项目内置的：

```text
examples/gpt5.5-unrestricted.md
```

你可以在 UI 中直接启用、禁用、切换多个提示词模板，也可以同时管理不同第三方供应商 API 和官方 OpenAI Codex 登录配置。

## 核心亮点

### 1. 指令提示词注入

- 一键启用 `gpt5.5-unrestricted.md`
- 支持多个提示词模板并存
- 启用前自动记录状态，方便回退
- 支持禁用当前提示词，让 Codex 恢复普通配置

### 2. Provider 可视化切换

- 添加第三方 Codex API Provider
- 编辑 Base URL / API Key / Model / TOML 配置
- 当前 Provider 状态清晰可见
- 支持从 cc-switch 数据库导入 Codex Provider

### 3. 官方 Codex 配置管理

- 读取 Codex 官方 `auth.json`
- 支持官方 ChatGPT 登录态 Auth 查看与编辑
- 区分官方 Auth 与第三方 API Key，不混在一起

### 4. TOML 配置编辑

- 可视化查看当前 Codex `config.toml`
- 在 Provider 编辑页直接编辑完整 TOML
- 保存后立即同步到 Codex 配置目录

### 5. 跨平台桌面软件

- macOS `.dmg`
- Windows `.msi`
- Linux `.deb` / `.rpm`
- GitHub Releases 自动构建发布

## 软件预览

<details open>
<summary><b>主界面 / Provider / TOML / Auth 截图</b></summary>

> 这里可以放软件主界面、供应商切换页面、TOML 编辑页面、官方 Auth 页面截图。

<!-- 示例：
<p align="center">
  <img src="docs/screenshots/overview.png" width="760" />
</p>
-->

</details>

<details>
<summary><b>提示词注入前后效果图</b></summary>

> 这里可以放启用 `gpt5.5-unrestricted.md` 前后的 Codex 效果对比图。

<!-- 示例：
<p align="center">
  <img src="docs/screenshots/prompt-before.png" width="760" />
  <img src="docs/screenshots/prompt-after.png" width="760" />
</p>
-->

</details>

## 技术栈

| 类型 | 技术 |
| --- | --- |
| 桌面框架 | Tauri 2 |
| 前端 | React 18 / TypeScript / Vite |
| 后端 | Rust |
| 本地数据 | SQLite / rusqlite |
| 配置编辑 | TOML / JSON |
| 发布 | GitHub Actions / GitHub Releases |

## 配置路径

Codex-X 默认读取 Codex 配置目录：

```text
~/.codex/config.toml
~/.codex/auth.json
```

也支持环境变量：

```text
CODEX_HOME=/path/to/.codex
CODEXX_HOME=/path/to/codex-x-data
CC_SWITCH_HOME=/path/to/.cc-switch
```

Codex-X 自身数据库默认位于：

```text
~/.codexx/codexx.db
```

## 下载

请前往 Releases 页面下载：

https://github.com/yynxxxxx/Codex-X/releases

## 开发运行

```bash
pnpm install
pnpm dev
```

构建桌面端：

```bash
pnpm --dir apps/desktop tauri build
```

## License

MIT © yynxxxxx


## macOS 安装说明

如果你在未签名/未公证的 DMG 中看到“软件已损坏”提示，这是 macOS Gatekeeper 的正常行为。

- 最佳方式：使用 Apple Developer ID 签名并 notarize
- 仅本地测试：可手动移除 quarantine 属性

```bash
xattr -dr com.apple.quarantine /Applications/Codex-X.app
```
