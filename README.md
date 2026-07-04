# Codex-X

Codex-X 是一个面向 OpenAI Codex / Codex CLI 的跨平台桌面配置管理器，用来可视化管理 Codex 的供应商、`config.toml`、`auth.json` 和指令提示词。

> 当前桌面端基于 **Tauri 2 + React + TypeScript + Rust + SQLite**。

## 功能

- **供应商管理**：添加、编辑、切换 Codex 第三方 API Provider。
- **官方配置**：查看/编辑 OpenAI Official 的 `auth.json` 与官方模型配置。
- **TOML 编辑**：在供应商编辑页直接编辑并保存完整 `config.toml`。
- **指令提示词管理**：管理多个 `model_instructions_file` 模板，支持自定义提示词、启用、禁用。
- **cc-switch 导入**：从本机 cc-switch SQLite 数据库导入 Codex Provider。
- **跨平台路径**：默认读取 `CODEX_HOME`，否则使用用户目录下的 `.codex`。
- **更新检查**：通过 GitHub Releases 检测最新版本。

## 配置文件位置

Codex-X 默认读写：

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

Codex-X 自身数据默认存放在：

```text
~/.codexx/codexx.db
```

## Windows 权限说明

Windows 正常情况下 Codex 配置位于：

```text
%USERPROFILE%\.codex
```

这是用户目录，不需要管理员权限。只有当你手动把 `CODEX_HOME` 指向 `C:\Program Files`、`C:\Windows`、系统盘根目录等受保护目录时，写入才可能因为权限不足失败。Codex-X 不会静默提权，会直接显示具体 IO 错误。

## 开发

```bash
pnpm install
pnpm dev
```

类型检查：

```bash
pnpm typecheck
```

构建桌面端：

```bash
pnpm --dir apps/desktop tauri build
```

## Release

本仓库使用 GitHub Actions 在打 tag 时自动构建三平台安装包：

```bash
git tag v0.2.0
git push origin v0.2.0
```

Release 页面会包含：

- macOS `.dmg`
- Windows `.msi` / `.exe`
- Linux `.AppImage` / `.deb` / `.rpm`
- GitHub 自动生成的 Source code `.zip` / `.tar.gz`

## 技术栈

- Tauri 2
- React 18
- TypeScript
- Vite
- Rust
- SQLite / rusqlite
- toml_edit

## 项目地址

<https://github.com/yynxxxxx/Codex-X>

## License

MIT
