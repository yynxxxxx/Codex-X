<p align="center">
  <a href="README.md"><img src="https://img.shields.io/badge/中文-当前-blue" alt="中文" /></a>
  <a href="README.en.md"><img src="https://img.shields.io/badge/English-Switch-lightgrey" alt="English" /></a>
</p>

<div align="center">
  <img src="apps/desktop/src-tauri/icons/icon.png" alt="Codex-X Logo" width="150" />

  # Codex-X

  **Codex 提示词注入 · Provider 切换 · TOML / Auth 可视化管理器**

  一款面向 **OpenAI Codex 桌面端 / Codex CLI** 的跨平台桌面工具，内置 `gpt5.5-unrestricted.md` 与 `gpt5.4-unrestricted.md`，支持一键写入 / 禁用指令提示词、第三方 Provider 切换、官方 Auth 管理、TOML 可视化编辑与本地会话 Provider Sync。

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

<div align="center">
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
</div>

</details>

<details>
<summary><b>提示词注入效果：逆向工程场景</b></summary>

<div align="center">
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
</div>

</details>

## 功能特性

<div align="center">
<table>
  <tr>
    <th align="center" width="180">功能</th>
    <th align="center">说明</th>
  </tr>
  <tr>
    <td align="center">⚡ 供应商 API</td>
    <td>可视化管理官方 OpenAI / 第三方 Codex Provider，支持 Base URL、API Key、Model、Wire API 与一键切换。</td>
  </tr>
  <tr>
    <td align="center">🧩 <b>提示词注入</b></td>
    <td><b>特色功能</b>：内置 <code>gpt5.4-unrestricted.md</code> / <code>gpt5.5-unrestricted.md</code>，一键写入 Codex 配置；启用后可达到上方效果图中的 SQL 注入测试、APK / EXE 逆向等响应效果。</td>
  </tr>
  <tr>
    <td align="center">💬 会话管理</td>
    <td>读取 Codex 本地 SQLite 与 rollout JSONL，会话列表可视化展示，并支持 Provider Sync / 修复历史 thread 元数据。</td>
  </tr>
  <tr>
    <td align="center">📝 TOML 配置</td>
    <td>查看当前 <code>~/.codex/config.toml</code>，并在 Provider 编辑页直接编辑完整 TOML，保存后同步到 Codex 配置目录。</td>
  </tr>
  <tr>
    <td align="center">🧠 Skills / MCP</td>
    <td>新增技能和 MCP 管理页：读取 Codex 当前可用的 Skills / MCP，支持导入已有、从 ZIP 安装 Skill、启用 / 禁用单个 Skill 或 MCP，并可检查 Skill 更新状态。</td>
  </tr>
  <tr>
    <td align="center">🔐 Auth 管理</td>
    <td>读取 / 编辑官方 <code>~/.codex/auth.json</code>，区分 ChatGPT 登录态 Auth 与第三方 API Key。</td>
  </tr>
  <tr>
    <td align="center">📦 跨平台发布</td>
    <td>提供 macOS Apple Silicon / Intel <code>.dmg</code>、Windows <code>.msi</code> / portable <code>.zip</code>、Linux <code>.deb</code> / <code>.rpm</code>，支持应用内检测更新。</td>
  </tr>
</table>
</div>

## 核心亮点

### 1. 内置双指令提示词模板

Codex-X 当前内置两套提示词模板：

- `examples/gpt5.5-unrestricted.md`
- `examples/gpt5.4-unrestricted.md`

你可以在【指令提示词】页面直接点击启用 / 禁用。启用后，Codex-X 会把对应 `.md` 写入 Codex 配置目录，并更新 `config.toml` 中的 `model_instructions_file`。

<div align="center">
<table>
  <tr>
    <th align="center">对比项</th>
    <th align="center"><code>gpt5.4-unrestricted.md</code></th>
    <th align="center"><code>gpt5.5-unrestricted.md</code></th>
  </tr>
  <tr>
    <td align="center">定位</td>
    <td align="center">5.4 CTF 沙箱</td>
    <td align="center">5.5 无限制模式</td>
  </tr>
  <tr>
    <td align="center">策略</td>
    <td align="center">CTF 竞赛沙箱间接绕过</td>
    <td align="center">直接声明无限制开发者模式</td>
  </tr>
  <tr>
    <td align="center">风格</td>
    <td align="center">安全研究导向，方法论详细</td>
    <td align="center">极简指令，覆盖面广</td>
  </tr>
  <tr>
    <td align="center">长度</td>
    <td align="center">约 180 行</td>
    <td align="center">约 40 行</td>
  </tr>
  <tr>
    <td align="center">适用</td>
    <td align="center">GPT-5.4 Codex CLI</td>
    <td align="center">GPT-5.5 Codex CLI</td>
  </tr>
</table>
</div>

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

### 6. Skills / MCP 管理

Codex-X 新增独立的【技能和 MCP】页面，用于集中管理 Codex 的专业能力扩展。

- Skills：读取当前 Codex Skills，支持导入已有、从 ZIP 安装、启用 / 禁用、检查更新状态
- MCP：读取当前 Codex MCP Server，支持导入已有、启用 / 禁用，启用后写入 Codex <code>config.toml</code>
- 适合管理 Android APK 逆向、Ghidra / IDA、Web / API / 协议逆向、CTF、安全测试等技能包

### 7. 逆向 Skills 导航

<div align="center">
  <a href="https://yynxxxxx.github.io/Codex-X/">
    <img src="https://img.shields.io/badge/Codex--X-在线逆向%20Skills%20导航-0ea5e9?style=for-the-badge&logo=githubpages&logoColor=white" alt="Codex-X 在线逆向 Skills 导航" />
  </a>
</div>

<br />

<table>
  <tr>
    <td width="55%">
      <b>在线教程页</b>：解释什么是“破甲”、Codex-X 如何启用 GPT-5.5 / unrestricted jeli、以及如何搭配不同领域的逆向 Skills。
      <br /><br />
      <b>分类覆盖</b>：Android APK / Windows EXE / Web 协议逆向。
      <br /><br />
      <b>内容包含</b>：Skill 用途、安装方式、来源地址、推荐使用流程。
    </td>
    <td width="45%">
      <ul>
        <li>🧩 GPT-5.5 / unrestricted jeli 使用流程</li>
        <li>📱 Android APK 逆向 Skills</li>
        <li>🪟 Windows EXE / DLL 逆向 Skills</li>
        <li>🌐 Web / API / 协议逆向 Skills</li>
        <li>📋 安装命令一键复制</li>
      </ul>
    </td>
  </tr>
</table>

<p align="center">
  <a href="https://yynxxxxx.github.io/Codex-X/">
    <b>🚀 打开 Codex-X 逆向 Skills 导航</b>
  </a>
</p>

### 8. 跨平台桌面软件

- macOS Apple Silicon `.dmg`
- macOS Intel `.dmg`
- Windows `.msi`
- Windows Portable `.zip`
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

## 许可证

本项目基于 [MIT License](https://github.com/yynxxxxx/Codex-X/blob/main/LICENSE) 开源。

## 致谢 / Thanks

感谢 [LINUX DO 论坛](https://linux.do/) 社区的关注、反馈与支持。

## Star History

<a href="https://www.star-history.com/?repos=yynxxxxx%2FCodex-X&type=date&legend=top-left">
  <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=yynxxxxx%2Fcodex-x&amp;type=date&amp;legend=top-left&amp;sealed_token=Vqvz67Jv_WIePGePCN8RdJaY5oCyqqCZUSFWs4M4dAxP8JXFZlYEWbI8YcU6SFgpqOqqJifzpOTIlMg4ee8NaCkHpCSqv1r5pxewR-tQlmxswaZlhedd6A" width="900" />
</a>

<br />

> [!IMPORTANT]
> **使用声明**
>
> 本项目仅用于大模型与智能体相关技术的学习、研究与交流，软件本身不包含主动破坏性功能。请在合法、合规并获得授权的范围内使用，禁止将其用于攻击、侵害他人权益或其他违法用途。使用者应自行判断使用边界，并对相关行为与后果承担责任。
