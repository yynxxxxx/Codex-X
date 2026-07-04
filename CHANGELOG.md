# Changelog

All notable changes to Codex-X will be documented here.

## [v0.2.9] - 2026-07-04

- 修复 GitHub Actions Release 发布流程：不再由三平台矩阵并发创建 Release，改为先上传构建产物，再由单独发布任务统一创建/更新 Release。
- 修复 `Resource not accessible by integration` 导致 Release 创建失败的问题。
- Release 仍会从 `CHANGELOG.md` 自动读取当前 tag 的更新日志，并上传 macOS / Windows / Linux 安装包。

## [v0.2.8] - 2026-07-04

- 更新页进一步产品化：去掉资源/仓库调试信息，将“有更新”改为更明显的绿色标签。
- 概览页顶部新增轻量“发现新版本”提示条，可直接打开 Releases 页面。
- 新增【会话管理】页面：检查 Codex 本地 sessions / archived_sessions 与 SQLite threads 是否和当前 Provider 同步。
- 新增一键 Provider Sync / 修复历史会话：写入前备份到 `~/.codex/backups_state/provider-sync/`，并保留最近 5 份备份。

## [v0.2.7] - 2026-07-04

- 简化更新检查页展示。
- “打开下载页”改为打开 GitHub Releases 页面。
- 更新弹窗保持简洁，仅提示版本差异。
- 首次启动自动检查更新并弹窗提醒。

## [v0.2.5] - 2026-07-04

- Windows 版双击启动不再额外弹出终端窗口。
- 改进 Windows MSI 安装器品牌展示与图标。
- About 页面外部链接改为系统默认浏览器打开。
- Release 流程加入 Rust cache，后续构建更快。

## [v0.2.4] - 2026-07-04

- 美化 Windows MSI 安装器横幅与对话图。
- Windows 安装包加入应用图标。

## [v0.2.3] - 2026-07-04

- 修复 macOS DMG 中应用图标缺失问题。
- 补充 macOS 安装说明。
- Release 流程加入基础缓存优化。

## [v0.2.2] - 2026-07-04

- macOS / Linux / Windows 首次三平台 Release。
- Linux 产物改为 `deb` / `rpm`，避免 AppImage 图标问题。

## [v0.2.1] - 2026-07-04

- 首次加入应用图标与 GitHub Release 自动发布。
