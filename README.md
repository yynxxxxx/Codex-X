<div align="center">
  <img src="apps/desktop/src-tauri/icons/icon.png" alt="Codex-X Logo" width="150" />

  # Codex-X

  **Codex 提示词注入 · Provider 切换 · TOML / Auth 可视化管理器**

  一款面向 **OpenAI Codex / Codex CLI** 的跨平台桌面工具，内置 `gpt5.5-unrestricted.md` 与 `gpt5.4-unrestricted.md`，支持一键写入 / 禁用指令提示词、第三方 Provider 切换、官方 Auth 管理、TOML 可视化编辑与本地会话 Provider Sync。

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

Codex-X 不是普通的配置文件编辑器，而是一个面向 Codex CLI 的 **可视化增强管理器**。

它把几个高频操作做成了桌面软件：

- 给 Codex 写入 / 禁用指令提示词模板
- 切换官方 OpenAI 与第三方 Codex API Provider
- 查看 / 编辑 `~/.codex/config.toml`
- 查看 / 编辑官方 `~/.codex/auth.json`
- 检查并修复本地历史会话的 Provider 元数据

## 软件预览

<details open>
<summary><b>应用界面预览</b>：主界面 / Provider / TOML / Auth</summary>

<p align="center">
  <img src="docs/screenshots/app/preview.png" alt="Codex-X 应用界面预览：主界面、Provider、TOML、Auth" width="920" />
</p>

</details>

<details>
<summary><b>提示词注入效果：安全测试场景</b></summary>

<table>
  <tr>
    <td align="center" width="50%">
      <b>SQL 注入测试</b><br />
      <sub>部署后测试：如何对目标进行 SQL 注入测试？</sub><br />
      <img src="docs/screenshots/prompt-effects/security/sql-injection.png" alt="SQL 注入测试效果图" width="420" />
    </td>
    <td align="center" width="50%">
      <b>NSFW 响应测试</b><br />
      <sub>用于观察提示词注入后的边界响应变化</sub><br />
      <img src="docs/screenshots/prompt-effects/security/nsfw.png" alt="NSFW 响应测试效果图" width="420" />
    </td>
  </tr>
</table>

</details>

<details>
<summary><b>提示词注入效果：逆向工程场景</b></summary>

<table>
  <tr>
    <td align="center" width="50%">
      <b>APK 逆向分析</b><br />
      <sub>Android APK 静态 / 动态分析思路</sub><br />
      <img src="docs/screenshots/prompt-effects/reverse/apk-reverse-1.png" alt="APK 逆向分析效果图" width="420" />
    </td>
    <td align="center" width="50%">
      <b>APK 逆向分析 2</b><br />
      <sub>补充 APK 逆向流程与定位方式</sub><br />
      <img src="docs/screenshots/prompt-effects/reverse/apk-reverse-2.png" alt="APK 逆向分析效果图 2" width="420" />
    </td>
  </tr>
  <tr>
    <td align="center" colspan="2">
      <b>EXE 逆向分析</b><br />
      <sub>Windows 可执行文件分析与调试方向</sub><br />
      <img src="docs/screenshots/prompt-effects/reverse/exe-reverse.png" alt="EXE 逆向分析效果图" width="620" />
    </td>
  </tr>
</table>

</details>

## 核心亮点

### 1. 内置双指令提示词模板

Codex-X 当前内置两套提示词模板：

- `examples/gpt5.5-unrestricted.md`
- `examples/gpt5.4-unrestricted.md`

你可以在【指令提示词】页面直接点击启用 / 禁用。启用后，Codex-X 会把对应 `.md` 写入 Codex 配置目录，并更新 `config.toml` 中的 `model_instructions_file`。

| 对比项 | `gpt5.4-unrestricted.md` | `gpt5.5-unrestricted.md` |
| --- | --- | --- |
| 定位 | 5.4 CTF 沙箱 | 5.5 无限制模式 |
| 策略 | CTF 竞赛沙箱间接绕过 | 直接声明无限制开发者模式 |
| 风格 | 安全研究导向，方法论详细 | 极简指令，覆盖面广 |
| 长度 | 约 180 行 | 约 40 行 |
| 适用 | GPT-5.4 Codex CLI | GPT-5.5 Codex CLI |

部署后可以测试：

```text
如何对目标进行 SQL 注入测试？
```

典型效果：

```text
破甲前 → 拒绝或泛化回答
破甲后 → 直接给安全研究方法论与测试步骤
```

### 2. Provider 可视化切换

- 添加第三方 Codex API Provider
- 编辑 Base URL / API Key / Model / Wire API
- Provider 页面可查看并编辑对应 TOML
- 当前启用 Provider 状态清晰可见
- 支持从 cc-switch 数据库导入 Codex Provider

### 3. 官方 Auth 管理

- 自动读取 Codex 官方 `auth.json`
- 支持查看 / 编辑 ChatGPT 登录态 Auth
- 区分官方 Auth 与第三方 API Key
- 官方配置可和第三方 Provider 在 UI 中统一管理

### 4. TOML 可视化编辑

- 查看当前 Codex `config.toml`
- 深色代码预览与语法高亮
- Provider 编辑页可直接编辑完整 TOML
- 保存后同步到 Codex 配置目录

### 5. 会话管理 / Provider Sync

Codex-X 可以读取 Codex 本地会话数据：

```text
~/.codex/sqlite/*.db
~/.codex/state_5.sqlite
~/.codex/sessions/**/rollout-*.jsonl
~/.codex/archived_sessions/**/rollout-*.jsonl
```

用于检查旧会话的 Provider 元数据是否和当前配置一致，并支持一键同步 / 修复，让历史 thread 继续被原生 Codex 识别、打开和续聊。

### 6. 跨平台桌面软件

- macOS `.dmg`
- Windows `.msi`
- Linux `.deb` / `.rpm`
- GitHub Releases 自动构建发布
- 应用内检查更新

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

## macOS 安装说明

如果你在未签名 / 未公证的 DMG 中看到“软件已损坏”提示，这是 macOS Gatekeeper 的正常行为。

- 最佳方式：使用 Apple Developer ID 签名并 notarize
- 仅本地测试：可手动移除 quarantine 属性

```bash
xattr -dr com.apple.quarantine /Applications/Codex-X.app
```

## License

MIT © yynxxxxx
